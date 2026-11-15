#!/usr/bin/env bash
# upgrade.sh: install and upgrade dbb's Docker apps from their GitHub repositories.
#
#   upgrade.sh                    upgrade every app in the list (only those with something new)
#   upgrade.sh myjam              upgrade one app
#   upgrade.sh myjam --force      rebuild and restart even if already up to date
#   upgrade.sh --list             show the apps, their versions and whether anything is new
#   upgrade.sh --setup NAME       first install of an app: deploy key, clone, first start
#   upgrade.sh --self-update      update this script (and deploy-apk.sh) from GitHub
#
# The apps are listed in ~/.config/upgrade/apps.conf (one [section] per app; see the example
# that comes with this script). Adding a future app is a new section there; this script
# doesn't change.
#
# For each app it:
#   1. refuses to run if the app's own files were changed by hand (your .env and data are separate),
#   2. shows what's new on GitHub,
#   3. backs up the files listed in `backup` (SQLite databases copied consistently while running),
#   4. fast-forwards to the new version and rebuilds/restarts its containers,
#   5. waits for its health check, and prints how to go back if anything fails.
set -euo pipefail

CONF="${UPGRADE_CONF:-$HOME/.config/upgrade/apps.conf}"
BACKUPS="${UPGRADE_BACKUPS:-$HOME/upgrade-backups}"
KEEP="${UPGRADE_KEEP:-10}"
SUDO="${UPGRADE_SUDO-sudo}"           # set UPGRADE_SUDO= if your user is in the docker group

bold() { printf '\n\033[1m%s\033[0m\n' "$*"; }
note() { printf '  %s\n' "$*"; }
warn() { printf '\033[33m%s\033[0m\n' "$*" >&2; }
die()  { printf '\n\033[31m%s\033[0m\n' "$*" >&2; exit 1; }

# ------------------------------------------------------------------ config
# INI-style: [name] then key = value lines. '#' starts a comment. ~ and $HOME expand in paths.
declare -A CFG=()
APPS=()

load_conf() {
  [ -f "$CONF" ] || die "No app list at $CONF. Copy apps.conf.example there and edit it."
  local section="" line key val
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line%%#*}"                                  # strip comments
    line="${line#"${line%%[![:space:]]*}"}"             # trim left
    line="${line%"${line##*[![:space:]]}"}"             # trim right
    [ -z "$line" ] && continue
    if [[ "$line" =~ ^\[([A-Za-z0-9._-]+)\]$ ]]; then
      section="${BASH_REMATCH[1]}"
      APPS+=("$section")
      continue
    fi
    [ -n "$section" ] || die "$CONF: '$line' is outside any [app] section"
    [[ "$line" == *=* ]] || die "$CONF: expected 'key = value', got '$line'"
    key="${line%%=*}"; key="${key%"${key##*[![:space:]]}"}"
    val="${line#*=}";  val="${val#"${val%%[![:space:]]*}"}"
    CFG["$section.$key"]="$val"
  done < "$CONF"
  [ "${#APPS[@]}" -gt 0 ] || die "$CONF lists no apps."
}

get() {   # get APP KEY [DEFAULT]
  local v="${CFG[$1.$2]-${3-}}"
  v="${v/#\~/$HOME}"
  printf '%s' "${v//\$HOME/$HOME}"
}

need() {  # need APP KEY -> value or die
  local v; v="$(get "$1" "$2")"
  [ -n "$v" ] || die "[$1] in $CONF has no '$2'."
  printf '%s' "$v"
}

# ------------------------------------------------------------------ per-app helpers
app_dir()     { need "$1" dir; }
compose_dir() {  # the app's folder, or its `subdir` inside it
  local d s; d="$(app_dir "$1")"; s="$(get "$1" subdir)"
  if [ -n "$s" ]; then printf '%s/%s' "$d" "$s"; else printf '%s' "$d"; fi
}
branch()      { get "$1" branch main; }

version_of() {  # version from `version = path:regex` (first capture group), else the commit
  local app="$1" spec file re
  spec="$(get "$app" version)"
  if [ -n "$spec" ]; then
    file="${spec%%:*}"; re="${spec#*:}"
    if [ -f "$(app_dir "$app")/$file" ]; then
      local v; v="$(sed -nE "s/^${re}\$/\\1/p" "$(app_dir "$app")/$file")"
      printf '%s' "${v%%$'\n'*}"
      return
    fi
  fi
  git -C "$(app_dir "$app")" rev-parse --short HEAD
}

compose() {     # compose APP ARGS... (runs in the app's compose folder)
  local cd; cd="$(compose_dir "$1")"; shift
  (cd "$cd" && $SUDO docker compose "$@")
}

backup() {      # backup APP LABEL -> path of the archive (or nothing when there's nothing to back up)
  local app="$1" label="$2" dir items tmp stamp out f
  dir="$(app_dir "$app")"
  items="$(get "$app" backup)"
  [ -n "$items" ] || return 0
  mkdir -p "$BACKUPS/$app"
  tmp="$(mktemp -d)"
  # Copy what exists, keeping relative paths.
  (cd "$dir" && for f in $items; do [ -e "$f" ] && $SUDO cp -a --parents "$f" "$tmp/"; done; true)
  # Re-copy SQLite databases with sqlite3's online backup, so they're consistent while the app runs.
  if command -v sqlite3 >/dev/null; then
    while IFS= read -r -d '' f; do
      local rel="${f#"$tmp"/}"
      $SUDO rm -f "$f-wal" "$f-shm"
      $SUDO sqlite3 "$dir/$rel" ".backup '$f'" 2>/dev/null || warn "  couldn't snapshot $rel with sqlite3; kept a plain copy"
    done < <(find "$tmp" -type f -name '*.db' -print0)
  fi
  stamp="$(date +%Y-%m-%d_%H%M)"
  out="$BACKUPS/$app/$app-$label-$stamp.tgz"
  $SUDO tar czf "$out" -C "$tmp" .
  $SUDO chown "$(id -u):$(id -g)" "$out"
  $SUDO rm -rf "$tmp"
  ls -1t "$BACKUPS/$app"/"$app"-*.tgz 2>/dev/null | tail -n +$((KEEP + 1)) | xargs -r rm --
  printf '%s' "$out"
}

health() {      # health APP VERSION SINCE -> 0 when healthy (waits up to `health_wait` seconds)
  local app="$1" ver="$2" since="$3" spec wait kind rest svc pat
  spec="$(get "$app" health)"
  wait="$(get "$app" health_wait 120)"
  [ -n "$spec" ] || { note "(no health check configured)"; return 0; }
  kind="${spec%%:*}"; rest="${spec#*:}"
  for _ in $(seq 1 $((wait / 2))); do
    case "$kind" in
      http)  curl -fsS -m 5 -k "$rest" >/dev/null 2>&1 && return 0 ;;
      log)   svc="${rest%%:*}"; pat="${rest#*:}"; pat="${pat//\{version\}/$ver}"
             # Capture first: `logs | grep -q` fails under pipefail when grep stops early.
             [[ "$(compose "$app" logs --no-color --since "$since" "$svc" 2>/dev/null || true)" == *"$pat"* ]] && return 0 ;;
      container) # container:NAME -> Docker's own healthcheck says healthy
             [ "$($SUDO docker inspect -f '{{.State.Health.Status}}' "$rest" 2>/dev/null)" = healthy ] && return 0 ;;
      *)     die "[$app] health must be http:URL, log:SERVICE:TEXT or container:NAME (got '$spec')" ;;
    esac
    sleep 2
  done
  return 1
}

# ------------------------------------------------------------------ upgrade one app
upgrade_app() {
  local app="$1" force="$2" dir br old_commit old_ver new new_ver since out
  dir="$(app_dir "$app")"
  br="$(branch "$app")"
  bold "== $app"
  [ -d "$dir/.git" ] || { warn "  not installed yet ($dir). Run: $0 --setup $app"; return 1; }
  cd "$dir"

  if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    git status --short --untracked-files=no
    warn "  These files were changed by hand. Keep a copy if you need it, undo with 'git -C $dir checkout -- <file>', then run again."
    return 1
  fi

  old_commit="$(git rev-parse --short HEAD)"
  old_ver="$(version_of "$app")"
  note "installed: $old_ver ($old_commit), branch $br; checking GitHub…"
  git fetch --quiet origin "$br" || { warn "  couldn't reach GitHub for $app"; return 1; }

  new="$(git rev-list --count "HEAD..origin/$br")"
  if [ "$new" -eq 0 ] && [ "$force" != 1 ]; then
    note "up to date"
    return 0
  fi
  if [ "$new" -gt 0 ]; then
    note "what's new ($new change(s)):"
    git log --oneline --no-decorate "HEAD..origin/$br" | sed 's/^/    /'
  fi

  out="$(backup "$app" "$old_ver")"
  [ -n "$out" ] && note "backed up to $out"

  git merge --ff-only --quiet "origin/$br" || { warn "  can't fast-forward (history changed upstream?)"; return 1; }
  new_ver="$(version_of "$app")"
  note "updating $old_ver → $new_ver ($(git rev-parse --short HEAD))"

  since="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if ! compose "$app" up -d --build --remove-orphans; then
    # The old containers are still running; put the files back to match them.
    git reset --quiet --hard "$old_commit"
    warn "  The build failed, so $app stays on $old_ver (its files are back at $old_commit)."
    warn "  The next run tries $new_ver again; fix or wait for a newer version."
    return 1
  fi

  note "waiting for $app to report healthy…"
  if health "$app" "$new_ver" "$since"; then
    bold "$app $new_ver is up."
    return 0
  fi
  compose "$app" logs --tail 30 2>/dev/null || true
  warn "  $app $new_ver didn't report healthy in time (log above). To go back to $old_ver:"
  warn "    git -C $dir reset --hard $old_commit"
  warn "    cd $(compose_dir "$app") && $SUDO docker compose up -d --build"
  [ -n "$out" ] && warn "  Settings and data from before the upgrade: $out"
  return 1
}

# ------------------------------------------------------------------ first install
# `ssh -T` to GitHub always exits 1 (no shell access), even when the key works, so
# read its reply instead of its exit status (with pipefail, `ssh | grep` would fail).
github_reply() {
  ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 -T "$1" 2>&1 | tail -n 1 || true
}
github_ok() { [[ "$(github_reply "$1")" == *"successfully authenticated"* ]]; }

setup_app() {
  local app="$1" dir remote br sub repo key alias since
  dir="$(app_dir "$app")"; remote="$(need "$app" remote)"; br="$(branch "$app")"; sub="$(get "$app" subdir)"
  bold "== Setting up $app in $dir"
  [ -e "$dir/.git" ] && die "$dir is already a Git checkout. Use: $0 $app"
  [ -e "$dir" ] && [ -n "$(ls -A "$dir" 2>/dev/null)" ] && die "$dir exists and isn't empty; move it aside first."

  # Read-only deploy key per repository (GitHub allows one repo per deploy key).
  if [[ "$remote" =~ ^git@([^:]+):([^/]+/[^.]+)(\.git)?$ ]]; then
    alias="${BASH_REMATCH[1]}"; repo="${BASH_REMATCH[2]}"
    if [ "$alias" != github.com ]; then
      key="$HOME/.ssh/${alias}_deploy"
      if [ ! -f "$key" ]; then
        ssh-keygen -q -t ed25519 -N "" -C "dbb $app (read-only)" -f "$key"
      fi
      if ! grep -qE "^Host[[:space:]]+$alias\$" "$HOME/.ssh/config" 2>/dev/null; then
        printf '\nHost %s\n    HostName github.com\n    User git\n    IdentityFile %s\n    IdentitiesOnly yes\n' \
          "$alias" "$key" >> "$HOME/.ssh/config"
        chmod 600 "$HOME/.ssh/config"
      fi
      if ! github_ok "$alias"; then
        bold "Add this read-only deploy key to $repo:"
        note "https://github.com/$repo/settings/keys/new   (leave 'Allow write access' unticked)"
        echo; cat "$key.pub"; echo
        read -r -p "Press Enter once it's added… " _
        github_ok "$alias" || die "GitHub still refuses the key ($(github_reply "$alias")). Check it was added to $repo."
      fi
    fi
  fi

  mkdir -p "$dir"
  git -C "$dir" init -q -b "$br"
  git -C "$dir" remote add -t "$br" origin "$remote"
  if [ -n "$sub" ]; then
    # Only the app's folder of a bigger repository.
    git -C "$dir" sparse-checkout set --cone "$sub"
  fi
  git -C "$dir" fetch --quiet --depth 50 origin "$br"
  git -C "$dir" checkout -q -B "$br" "origin/$br"
  git -C "$dir" branch -q --set-upstream-to "origin/$br"
  note "checked out $(git -C "$dir" rev-parse --short HEAD) on $br"

  local init; init="$(get "$app" init)"
  if [ -n "$init" ]; then
    note "first-time setup: $init"
    (cd "$dir" && SUDO="$SUDO" bash -c "$init")
  fi

  since="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  compose "$app" up -d --build
  if health "$app" "$(version_of "$app")" "$since"; then
    bold "$app is installed and running."
  else
    compose "$app" logs --tail 30 || true
    die "$app started but didn't report healthy (log above)."
  fi
}

# ------------------------------------------------------------------ list
list_apps() {
  local app dir br n
  printf '%-14s %-12s %-10s %s\n' APP VERSION BRANCH STATUS
  for app in "${APPS[@]}"; do
    dir="$(app_dir "$app")"; br="$(branch "$app")"
    if [ ! -d "$dir/.git" ]; then
      printf '%-14s %-12s %-10s %s\n' "$app" - "$br" "not installed (--setup $app)"
      continue
    fi
    if git -C "$dir" fetch --quiet origin "$br" 2>/dev/null; then
      n="$(git -C "$dir" rev-list --count "HEAD..origin/$br")"
      [ "$n" -eq 0 ] && n="up to date" || n="$n new change(s)"
    else
      n="can't reach GitHub"
    fi
    (cd "$dir" && printf '%-14s %-12s %-10s %s\n' "$app" "$(version_of "$app")" "$br" "$n")
  done
}

# ------------------------------------------------------------------ self-update
self_update() {
  # bootstrap.sh links ~/bin/upgrade.sh into a checkout of the Myterm repo's deploy/ folder.
  local real tools before
  real="$(readlink -f "$0")"
  tools="$(cd "$(dirname "$real")/.." && pwd)"
  git -C "$tools" rev-parse --git-dir >/dev/null 2>&1 \
    || die "This copy of upgrade.sh isn't linked to GitHub. Run deploy/bootstrap.sh once to set that up."
  before="$(git -C "$tools" rev-parse --short HEAD)"
  # Follow the released branch (main), also for copies set up from dev.
  local branch="${MYTERM_TOOLS_BRANCH:-main}" cur
  cur="$(git -C "$tools" rev-parse --abbrev-ref HEAD)"
  if [ "$cur" != "$branch" ]; then
    git -C "$tools" remote set-branches --add origin "$branch"
    if git -C "$tools" fetch -q --depth 20 origin "$branch" 2>/dev/null; then
      git -C "$tools" checkout -q -B "$branch" "origin/$branch"
      bold "The scripts now follow the $branch branch (were on $cur)."
    else
      echo "Couldn't fetch the $branch branch; staying on $cur." >&2
    fi
  fi
  git -C "$tools" pull -q --ff-only || die "Couldn't update $tools (changed by hand? see git -C $tools status)"
  if [ "$before" = "$(git -C "$tools" rev-parse --short HEAD)" ]; then
    bold "upgrade.sh and deploy-apk.sh are up to date ($before)."
  else
    bold "Updated the scripts:"
    git -C "$tools" log --oneline --no-decorate "$before..HEAD" -- deploy | sed 's/^/  /'
  fi
}

# ------------------------------------------------------------------ main
main() {
  local force=0 setup="" list=0 targets=() a
  for a in "$@"; do
    case "$a" in
      --force) force=1 ;;
      --list) list=1 ;;
      --self-update) self_update; exit 0 ;;
      --setup) setup=next ;;
      -h|--help) sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
      -*) die "Unknown option $a (see --help)" ;;
      *) if [ "$setup" = next ]; then setup="$a"; else targets+=("$a"); fi ;;
    esac
  done
  load_conf

  if [ "$setup" = next ]; then die "--setup needs an app name: ${APPS[*]}"; fi
  if [ -n "$setup" ]; then
    [ -n "${CFG[$setup.dir]-}" ] || die "No [$setup] in $CONF (apps: ${APPS[*]})"
    setup_app "$setup"; exit 0
  fi
  if [ "$list" = 1 ]; then list_apps; exit 0; fi

  [ "${#targets[@]}" -gt 0 ] || targets=("${APPS[@]}")
  local failed=() ok=0
  for a in "${targets[@]}"; do
    [ -n "${CFG[$a.dir]-}" ] || die "No [$a] in $CONF (apps: ${APPS[*]})"
    if (upgrade_app "$a" "$force"); then ok=$((ok + 1)); else failed+=("$a"); fi
  done
  $SUDO docker image prune -f >/dev/null 2>&1 || true    # tidy unused image layers
  if [ "${#failed[@]}" -gt 0 ]; then
    die "Not upgraded: ${failed[*]} (details above)."
  fi
  bold "Done."
}

main "$@"
