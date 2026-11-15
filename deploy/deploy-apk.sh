#!/usr/bin/env bash
# deploy-apk.sh: install the newest Android app from any of your GitHub repositories
# on a phone, tablet or Fire TV over adb (wireless debugging or USB).
#
#   deploy-apk.sh                 choose an app from the list, download its newest build, install it
#   deploy-apk.sh myjam           the app whose repository name contains "myjam"
#   deploy-apk.sh path/to/x.apk   install a file you already have
#   deploy-apk.sh --list          every app found, with its newest build
#   deploy-apk.sh --fetch [app]   download newest builds only, no install (for cron)
#   deploy-apk.sh --refresh       look through your repositories again for apps
#
# Apps are found automatically. Every repository the token can see is checked for
#   - GitHub Release assets ending in .apk (e.g. last-litre-android), and
#   - GitHub Actions build artifacts that contain an .apk (e.g. MyJam-Android, Myterm).
# Builds from the repository's default branch are preferred; if it has none, the
# newest build from any branch is used (and the list says which branch).
#
# Devices you install to (IP, connection and pairing ports) are remembered in
# ~/.config/apk/adb-devices and offered next time.
#
# Needs: adb, curl, python3, and a read-only GitHub token in ~/.config/apk/github.token
# (see deploy/README.md: fine-grained, all repositories, Actions + Contents read-only).
set -euo pipefail

CONF_DIR="${APK_CONF_DIR:-$HOME/.config/apk}"
STORE="${APK_STORE:-$HOME/apks}"
TOKEN_FILE="${APK_TOKEN_FILE:-$CONF_DIR/github.token}"
KEEP="${APK_KEEP:-3}"
CATALOG="$CONF_DIR/catalog.json"
DEVICES="$CONF_DIR/adb-devices"   # one line per device: ip connect-port pairing-port model

die()  { printf '\033[31m%s\033[0m\n' "$*" >&2; exit 1; }
bold() { printf '\033[1m%s\033[0m\n' "$*"; }

mkdir -p "$CONF_DIR" "$STORE"

# Bring in the device remembered by earlier versions of this script
# (~/.config/myjam/adb-last from the MyJam one, ~/.config/apk/adb-last).
if [ ! -f "$DEVICES" ]; then
  for old in "$HOME/.config/myjam/adb-last" "$CONF_DIR/adb-last"; do
    if [ -s "$old" ]; then
      read -r o_ip o_conn _ < "$old" || true
      [ -n "${o_ip:-}" ] && printf '%s %s - \n' "$o_ip" "${o_conn:--}" >> "$DEVICES"
    fi
  done
  [ -f "$DEVICES" ] && { awk '!seen[$1]++' "$DEVICES" > "$DEVICES.tmp" && mv "$DEVICES.tmp" "$DEVICES"; }
fi
chmod 700 "$CONF_DIR"
command -v python3 >/dev/null || die "python3 is needed: sudo apt install python3"

# ---------------------------------------------------------------- GitHub side (python)
# One embedded helper does the API work: discovery, listing and downloads.
gh_py() {
  [ -f "$TOKEN_FILE" ] || [ -f "$HOME/.config/myjam/github-apk.token" ] || die \
"No GitHub token. Create a fine-grained token (all repositories; Actions: read, Contents: read),
then: printf '%s' TOKEN > $TOKEN_FILE && chmod 600 $TOKEN_FILE"
  py "$@"
}

py() {
  APK_TOKEN_FILE="$TOKEN_FILE" APK_CATALOG="$CATALOG" APK_STORE="$STORE" APK_KEEP="$KEEP" \
    python3 - "$@" <<'PY'
import io, json, os, re, sys, time, urllib.request, urllib.error, zipfile

TOKEN_FILE = os.environ["APK_TOKEN_FILE"]
if not os.path.exists(TOKEN_FILE):                       # the old MyJam-only token still works
    TOKEN_FILE = os.path.expanduser("~/.config/myjam/github-apk.token")
_token = None

def token():
    global _token
    if _token is None:
        _token = open(TOKEN_FILE).read().strip()
    return _token

CATALOG = os.environ["APK_CATALOG"]
STORE = os.environ["APK_STORE"]
KEEP = int(os.environ["APK_KEEP"])
API = "https://api.github.com"

class NoRedirectAuth(urllib.request.HTTPRedirectHandler):
    # GitHub redirects downloads to signed storage URLs; don't send our token there.
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        new = super().redirect_request(req, fp, code, msg, headers, newurl)
        if new is not None and "api.github.com" not in newurl:
            new.headers.pop("Authorization", None)
            new.unredirected_hdrs.pop("Authorization", None)
        return new

opener = urllib.request.build_opener(NoRedirectAuth)

def get(url, raw=False, accept="application/vnd.github+json"):
    req = urllib.request.Request(url if url.startswith("http") else API + url)
    req.add_header("Authorization", f"Bearer {token()}")
    req.add_header("Accept", accept)
    req.add_header("X-GitHub-Api-Version", "2022-11-28")
    for attempt in range(3):
        try:
            with opener.open(req, timeout=120) as r:
                data = r.read()
            return data if raw else json.loads(data)
        except urllib.error.HTTPError as e:
            if e.code in (401, 403) and attempt == 0:
                msg = e.read().decode(errors="replace")[:200]
                sys.exit(f"GitHub refused the token ({e.code}): {msg}\nCheck it hasn't expired and has Actions + Contents read access.")
            if e.code >= 500 and attempt < 2:
                time.sleep(3 * (attempt + 1)); continue
            raise
        except urllib.error.URLError:
            if attempt < 2:
                time.sleep(3 * (attempt + 1)); continue
            raise

def paged(url, key=None):
    page = 1
    while True:
        sep = "&" if "?" in url else "?"
        data = get(f"{url}{sep}per_page=100&page={page}")
        items = data[key] if key else data
        yield from items
        if len(items) < 100:
            return
        page += 1

def family(name):
    """Artifact name without a trailing commit/version/run number, e.g. MyJam-<sha> -> MyJam-."""
    return re.sub(r"[-_.]?(v?\d+(\.\d+)*|[0-9a-f]{7,40})$", "", name) or name

def load_catalog():
    try:
        return json.load(open(CATALOG))
    except (OSError, ValueError):
        return {"families": {}}

def save_catalog(c):
    tmp = CATALOG + ".tmp"
    json.dump(c, open(tmp, "w"), indent=1, sort_keys=True)
    os.replace(tmp, CATALOG)

def apk_in_zip(blob):
    with zipfile.ZipFile(io.BytesIO(blob)) as z:
        names = [n for n in z.namelist() if n.lower().endswith(".apk")]
        if not names:
            return None
        # Prefer a release build if both are there.
        names.sort(key=lambda n: ("debug" in n.lower(), n))
        return z.read(names[0])

def candidates(repo, cat):
    """Newest build per source for one repo: releases' .apk assets and APK artifacts."""
    out = []
    default = repo["default_branch"]
    full = repo["full_name"]
    # Releases (newest first).
    if not repo.get("archived"):
        try:
            for rel in get(f"/repos/{full}/releases?per_page=10"):
                if rel.get("draft"):
                    continue
                for a in rel.get("assets", []):
                    if a["name"].lower().endswith(".apk"):
                        out.append({"repo": full, "kind": "release", "id": f"r{a['id']}", "name": a["name"],
                                    "url": a["url"], "branch": rel.get("target_commitish") or default,
                                    "label": rel.get("tag_name") or rel.get("name") or "", "created": a["created_at"],
                                    "size": a["size"], "default": default})
                if out:
                    break
        except urllib.error.HTTPError:
            pass
    # Actions artifacts.
    try:
        arts = [a for a in get(f"/repos/{full}/actions/artifacts?per_page=100")["artifacts"] if not a["expired"]]
    except urllib.error.HTTPError:
        arts = []
    arts.sort(key=lambda a: a["created_at"], reverse=True)
    seen = set()
    for a in arts:
        fam = f"{full}:{family(a['name'])}"
        if fam in seen:
            continue
        seen.add(fam)
        known = cat["families"].get(fam)
        if known is None:
            # First time we see this kind of build: look inside once and remember.
            try:
                blob = get(f"/repos/{full}/actions/artifacts/{a['id']}/zip", raw=True)
                known = apk_in_zip(blob) is not None
            except Exception:
                continue
            cat["families"][fam] = known
        if not known:
            continue
        same = [x for x in arts if f"{full}:{family(x['name'])}" == fam]
        on_default = [x for x in same if x["workflow_run"]["head_branch"] == default]
        best = (on_default or same)[0]
        out.append({"repo": full, "kind": "artifact", "id": f"a{best['id']}", "name": best["name"],
                    "url": f"{API}/repos/{full}/actions/artifacts/{best['id']}/zip",
                    "branch": best["workflow_run"]["head_branch"], "label": best["workflow_run"]["head_sha"][:7],
                    "created": best["created_at"], "size": best["size_in_bytes"], "default": default})
    return out

def scan(refresh=False):
    cat = load_catalog()
    if refresh:
        cat["families"] = {}
    apps = []
    for repo in paged("/user/repos?affiliation=owner,collaborator,organization_member&sort=pushed"):
        apps += candidates(repo, cat)
    cat["apps"] = apps
    cat["scanned"] = int(time.time())
    save_catalog(cat)
    return apps

def apps_list(max_age=6 * 3600):
    cat = load_catalog()
    if "apps" in cat and time.time() - cat.get("scanned", 0) < max_age:
        # Cheap refresh of the newest build for known apps only.
        names = {a["repo"] for a in cat["apps"]}
        apps = []
        for full in sorted(names):
            repo = get(f"/repos/{full}")
            apps += candidates(repo, cat)
        cat["apps"] = apps
        save_catalog(cat)
        return apps
    return scan()

def short(repo):
    return repo.split("/", 1)[1]

def choose(apps, query):
    if not query:
        return apps
    q = query.lower()
    m = [a for a in apps if q in a["repo"].lower() or q in a["name"].lower()]
    return m

def describe(a):
    when = a["created"][:16].replace("T", " ")
    where = a["branch"] + ("" if a["branch"] == a["default"] else f" (not {a['default']})")
    src = "release " + a["label"] if a["kind"] == "release" else "build " + a["label"]
    return f"{short(a['repo']):<24} {src:<22} {where:<16} {when}  {a['size'] / 1e6:.1f} MB"

def fetch(a):
    """Downloads the app's newest build if it's new; returns the path of <repo>-latest.apk."""
    d = os.path.join(STORE, short(a["repo"]))
    os.makedirs(d, exist_ok=True)
    marker = os.path.join(d, ".build-id")
    latest = os.path.join(d, f"{short(a['repo'])}-latest.apk")
    have = open(marker).read().strip() if os.path.exists(marker) else ""
    if os.path.exists(latest) and have == a["id"]:
        return latest, False
    if a["kind"] == "release":
        blob = get(a["url"], raw=True, accept="application/octet-stream")
    else:
        blob = apk_in_zip(get(a["url"], raw=True))
        if blob is None:
            sys.exit(f"The build {a['name']} has no APK in it")
    name = os.path.join(d, f"{short(a['repo'])}-{a['label'] or a['id']}.apk")
    with open(name + ".tmp", "wb") as f:
        f.write(blob)
    os.replace(name + ".tmp", name)
    with open(latest + ".tmp", "wb") as f:
        f.write(blob)
    os.replace(latest + ".tmp", latest)
    open(marker, "w").write(a["id"])
    olds = sorted((p for p in os.listdir(d) if p.endswith(".apk") and not p.endswith("-latest.apk")),
                  key=lambda p: os.path.getmtime(os.path.join(d, p)), reverse=True)
    for p in olds[KEEP:]:
        os.remove(os.path.join(d, p))
    return latest, True

cmd, args = sys.argv[1], sys.argv[2:]
try:
    if cmd == "scan":
        apps = scan(refresh=True)
        print(f"Found {len(apps)} app(s).", file=sys.stderr)
    elif cmd == "list":
        apps = apps_list()
        if not apps:
            print("No apps found. Check the token can see your repositories, then try --refresh.")
        for i, a in enumerate(apps, 1):
            print(f"{i:>2}. {describe(a)}")
    elif cmd == "pick":           # pick [query] -> prints the chosen app as JSON (menu on stderr)
        apps = choose(apps_list(), args[0] if args else "")
        if not apps:
            sys.exit(f"No app matches '{args[0]}'. Try: deploy-apk.sh --list" if args else "No apps found.")
        if len(apps) == 1:
            print(json.dumps(apps[0])); sys.exit(0)
        for i, a in enumerate(apps, 1):
            print(f"{i:>2}. {describe(a)}", file=sys.stderr)
        tty = open("/dev/tty")
        sys.stderr.write("Which app? [1]: "); sys.stderr.flush()
        ans = tty.readline().strip() or "1"
        if not ans.isdigit() or not 1 <= int(ans) <= len(apps):
            sys.exit("No such number.")
        print(json.dumps(apps[int(ans) - 1]))
    elif cmd == "fetch":          # fetch JSON -> path of latest apk
        a = json.loads(args[0])
        path, new = fetch(a)
        print(f"{'Downloaded' if new else 'Already have'} {short(a['repo'])} {a['label']} ({a['branch']})", file=sys.stderr)
        print(path)
    elif cmd == "fetch-all":      # fetch-all [query]
        for a in choose(apps_list(), args[0] if args else ""):
            path, new = fetch(a)
            if new:
                print(f"{time.strftime('%F %T')} new: {short(a['repo'])} {a['label']} -> {path}")
    elif cmd == "info":           # info APK -> "package versionName" from the binary manifest
        def axml_info(blob):
            import struct
            def u16(o): return struct.unpack_from("<H", blob, o)[0]
            def u32(o): return struct.unpack_from("<I", blob, o)[0]
            strings, pos = [], 8
            while pos < len(blob):
                ctype, hsize, csize = u16(pos), u16(pos + 2), u32(pos + 4)
                if csize == 0:
                    break
                if ctype == 0x0001:                    # string pool
                    count, flags, sstart = u32(pos + 8), u32(pos + 16), u32(pos + 20)
                    utf8 = bool(flags & (1 << 8))
                    for i in range(count):
                        so = pos + sstart + u32(pos + hsize + 4 * i)
                        if utf8:
                            n = blob[so]; so += 2 if n & 0x80 else 1           # utf-16 length
                            n = blob[so]; so += 1
                            if n & 0x80:
                                n = ((n & 0x7F) << 8) | blob[so]; so += 1
                            strings.append(blob[so:so + n].decode("utf-8", "replace"))
                        else:
                            n = u16(so); so += 2
                            if n & 0x8000:
                                n = ((n & 0x7FFF) << 16) | u16(so); so += 2
                            strings.append(blob[so:so + 2 * n].decode("utf-16le", "replace"))
                elif ctype == 0x0102:                  # first start tag: <manifest ...>
                    acount = u16(pos + 28)
                    found = {}
                    for i in range(acount):
                        ao = pos + 36 + 20 * i
                        name, raw = u32(ao + 4), u32(ao + 8)
                        dtype, data = blob[ao + 15], u32(ao + 16)
                        key = strings[name] if name < len(strings) else ""
                        if raw != 0xFFFFFFFF and raw < len(strings):
                            found[key] = strings[raw]
                        elif dtype == 0x10:
                            found[key] = str(data)
                    return found.get("package", ""), found.get("versionName", "")
                pos += csize
            return "", ""
        try:
            with zipfile.ZipFile(args[0]) as z:
                pkg, ver = axml_info(z.read("AndroidManifest.xml"))
        except Exception:
            pkg, ver = "", ""
        print(f"{pkg} {ver}".strip())
    else:
        sys.exit(f"unknown helper command {cmd}")
except urllib.error.URLError as e:
    sys.exit(f"Couldn't reach GitHub: {e}")
PY
}

# ---------------------------------------------------------------- device side
ask() {   # ask "question" default -> answer
  local a
  if [ -n "$2" ]; then printf '%s [%s]: ' "$1" "$2" >&2; else printf '%s: ' "$1" >&2; fi
  read -r a
  printf '%s' "${a:-$2}"
}
is_ip()   { [[ "$1" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; }
is_port() { [[ "$1" =~ ^[0-9]{1,5}$ ]] && [ "$1" -ge 1 ] && [ "$1" -le 65535 ]; }

pick_device() {   # prints the adb serial to use
  local connected last_ip="" last_port="" last_pair="" ip pair code conn out
  # A device already connected (USB, or wireless from earlier)?
  mapfile -t connected < <(adb devices | awk 'NR > 1 && $2 == "device" {print $1}')
  if [ "${#connected[@]}" -gt 0 ]; then
    local i=1 d
    for d in "${connected[@]}"; do
      printf '%2d. %s (%s)\n' "$i" "$d" "$(adb -s "$d" shell getprop ro.product.model 2>/dev/null | tr -d '\r')" >&2
      i=$((i + 1))
    done
    printf '%2d. another device over wireless debugging\n' "$i" >&2
    local n; n="$(ask "Install on" 1)"
    if [[ "$n" =~ ^[0-9]+$ ]] && [ "$n" -ge 1 ] && [ "$n" -lt "$i" ]; then
      printf '%s' "${connected[$((n - 1))]}"; return
    fi
  fi
  # Known devices, most recent first: "ip connect-port pairing-port model".
  local -a known=() ; local line
  [ -f "$DEVICES" ] && mapfile -t known < <(grep -v '^\s*$' "$DEVICES")
  local kip="" kconn="" kpair="" kname=""
  if [ "${#known[@]}" -gt 0 ]; then
    echo "Known devices:" >&2
    local j=1
    for line in "${known[@]}"; do
      read -r kip kconn kpair kname <<<"$line"
      printf '%2d. %s  connect %s  pair %s  %s\n' "$j" "$kip" "${kconn:--}" "${kpair:--}" "${kname:-}" >&2
      j=$((j + 1))
    done
    printf '%2d. a new device\n' "$j" >&2
    local k; k="$(ask "Device" 1)"
    if [[ "$k" =~ ^[0-9]+$ ]] && [ "$k" -ge 1 ] && [ "$k" -lt "$j" ]; then
      read -r last_ip last_port last_pair _ <<<"${known[$((k - 1))]}"
    fi
  fi
  [ "$last_port" = "-" ] && last_port=""
  [ "${last_pair:-}" = "-" ] && last_pair=""
  ip="$(ask "Device IP address" "$last_ip")"
  is_ip "$ip" || die "That isn't an IP address: $ip"
  if [ "$ip" != "$last_ip" ]; then
    # Typed an address that's in the list? Use its saved ports.
    last_port=""; last_pair=""
    for line in "${known[@]}"; do
      read -r kip kconn kpair _ <<<"$line"
      if [ "$kip" = "$ip" ]; then
        last_port="${kconn#-}"; last_pair="${kpair#-}"; break
      fi
    done
  fi
  echo "Ports are on the device: Developer options -> Wireless debugging. Android changes them when" >&2
  echo "Wireless debugging is turned back on, so press Enter to reuse the saved ones or type the new ones." >&2
  if [ -n "${last_port}${last_pair:-}" ]; then
    # A device used before is normally still paired.
    local again; again="$(ask "Pair it again? (only if adb was reset or the device forgot this server) y/N" "N")"
    if [[ "$again" =~ ^[Yy] ]]; then
      pair="$(ask "Pairing port" "${last_pair:-}")"
    else
      pair=""
    fi
  else
    pair="$(ask "Pairing port (Enter to skip if this device is already paired)" "")"
  fi
  if [ -n "$pair" ]; then
    is_port "$pair" || die "That isn't a port: $pair"
    code="$(ask "Pairing code (6 digits)" "")"
    [[ "$code" =~ ^[0-9]{6}$ ]] || die "The pairing code is 6 digits"
    adb pair "$ip:$pair" "$code" >&2
  else
    pair="${last_pair:-}"
  fi
  conn="$(ask "Connection port" "$last_port")"
  is_port "$conn" || die "That isn't a port: $conn"
  out="$(adb connect "$ip:$conn" 2>&1)"; echo "$out" >&2
  case "$out" in *"connected to"*) ;; *) die "Couldn't connect. Is Wireless debugging still on, and the port right? (It changes each time Wireless debugging is switched on.)";; esac
  remember_device "$ip" "$conn" "$pair" "$(adb -s "$ip:$conn" shell getprop ro.product.model 2>/dev/null | tr -d '\r ' || true)"
  printf '%s' "$ip:$conn"
}

# remember_device IP CONNECT PAIR MODEL: moves it to the top of the list.
remember_device() {
  local tmp; tmp="$(mktemp)"
  printf '%s %s %s %s\n' "$1" "${2:--}" "${3:--}" "${4:-}" > "$tmp"
  [ -f "$DEVICES" ] && awk -v ip="$1" '$1 != ip' "$DEVICES" | head -n 19 >> "$tmp"
  mv "$tmp" "$DEVICES"
}

install_apk() {   # install_apk APK
  local apk="$1" dev info pkg ver out
  command -v adb >/dev/null || die "adb isn't installed: sudo apt install adb"
  info="$(py info "$apk" 2>/dev/null || true)"; pkg="${info%% *}"; ver="${info#* }"; [ "$ver" = "$info" ] && ver=""
  bold "APK: $(readlink -f "$apk")${pkg:+  ($pkg${ver:+ $ver})}"
  dev="$(pick_device)"
  adb -s "$dev" wait-for-device
  echo "Installing on $(adb -s "$dev" shell getprop ro.product.model | tr -d '\r') ..."
  if ! out="$(adb -s "$dev" install -r "$apk" 2>&1)"; then
    echo "$out"
    if [[ "$out" == *INSTALL_FAILED_UPDATE_INCOMPATIBLE* || "$out" == *"signatures do not match"* ]]; then
      [ -n "$pkg" ] || pkg="$(sed -nE 's/.*[Pp]ackage ([A-Za-z0-9_.]+) signatures.*/\1/p' <<<"$out" | head -n 1)"
      bold "The app on the device was signed with a different key, so it can't be updated in place."
      echo "Uninstalling it first deletes the app's data on that device (sign-ins, settings, vaults)."
      if [ -n "$pkg" ] && [ "$(ask "Uninstall $pkg and install fresh? (yes/no)" no)" = yes ]; then
        adb -s "$dev" uninstall "$pkg"
        adb -s "$dev" install "$apk"
      else
        die "Not installed."
      fi
    else
      die "adb install failed (above)."
    fi
  fi
  if [ -n "$pkg" ]; then
    ver="$(adb -s "$dev" shell dumpsys package "$pkg" | sed -n 's/.*versionName=//p' | head -n 1 | tr -d '\r')"
    bold "Installed $pkg${ver:+ $ver}"
    adb -s "$dev" shell monkey -p "$pkg" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1 || true
  else
    bold "Installed."
  fi
  [[ "$dev" == *:* ]] && adb disconnect "$dev" >/dev/null 2>&1 || true
}

# ---------------------------------------------------------------- main
case "${1:-}" in
  -h|--help) awk 'NR > 1 && !/^#/ {exit} NR > 1' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  --list)    gh_py list ;;
  --refresh) gh_py scan && gh_py list ;;
  --fetch)   gh_py fetch-all "${2:-}" ;;
  -*)        die "Unknown option $1 (see --help)" ;;
  *)
    if [ -n "${1:-}" ] && [ -f "$1" ]; then
      install_apk "$1"
    else
      app="$(gh_py pick "${1:-}")"
      apk="$(gh_py fetch "$app")"
      install_apk "$apk"
    fi ;;
esac
