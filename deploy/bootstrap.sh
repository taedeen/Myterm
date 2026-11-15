#!/usr/bin/env bash
# bootstrap.sh: set up upgrade.sh and deploy-apk.sh on a server (once per server).
#
#   bash bootstrap.sh
#
# What it does, asking before anything that needs sudo:
#   1. checks the tools these scripts use (git, curl, python3, docker compose; sqlite3, adb optional),
#   2. creates a read-only deploy key for the Myterm repository and the SSH alias github-myterm,
#      and waits while you add the key on GitHub,
#   3. checks out just the repository's deploy/ folder into ~/.local/share/myterm-tools,
#   4. links upgrade.sh and deploy-apk.sh into ~/bin (so `upgrade.sh --self-update` keeps them current),
#   5. creates ~/.config/upgrade/apps.conf (empty list, with examples) if there isn't one,
#   6. optionally stores the GitHub token deploy-apk.sh uses.
# Safe to run again: each step is skipped when it's already done.
set -euo pipefail

REPO="taedeen/Myterm"
ALIAS="github-myterm"
BRANCH="${MYTERM_TOOLS_BRANCH:-main}"
TOOLS="$HOME/.local/share/myterm-tools"
REMOTE="${MYTERM_TOOLS_REMOTE:-git@$ALIAS:$REPO.git}"   # override only for testing
KEY="$HOME/.ssh/${ALIAS}_deploy"
HOST="$(hostname -s)"

bold() { printf '\n\033[1m%s\033[0m\n' "$*"; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }
die()  { printf '\n\033[31m%s\033[0m\n' "$*" >&2; exit 1; }
yes_no() { local a; read -r -p "$1 [y/N] " a; [[ "$a" =~ ^[Yy] ]]; }

# ------------------------------------------------------------------ 1. prerequisites
bold "1. Checking tools"
missing=()
for t in git curl python3 ssh ssh-keygen; do
  if command -v "$t" >/dev/null; then ok "$t"; else missing+=("$t"); warn "$t missing"; fi
done
if command -v docker >/dev/null && docker compose version >/dev/null 2>&1 || sudo -n docker compose version >/dev/null 2>&1; then
  ok "docker compose"
else
  warn "docker compose not found. upgrade.sh needs it for Docker apps: https://docs.docker.com/engine/install/"
fi
for t in sqlite3 adb; do
  command -v "$t" >/dev/null && ok "$t (optional)" || { warn "$t (optional) missing"; missing+=("$t"); }
done
if [ "${#missing[@]}" -gt 0 ] && command -v apt-get >/dev/null; then
  pkgs="${missing[*]}"; pkgs="${pkgs//ssh-keygen/}"; pkgs="${pkgs//ssh/openssh-client}"
  if yes_no "Install with apt: $pkgs ?"; then
    sudo apt-get update -qq && sudo apt-get install -y $pkgs
  fi
fi
for t in git curl python3 ssh ssh-keygen; do command -v "$t" >/dev/null || die "$t is required."; done

# ------------------------------------------------------------------ 2. deploy key
bold "2. Read-only access to $REPO"
mkdir -p "$HOME/.ssh" && chmod 700 "$HOME/.ssh"
if [ ! -f "$KEY" ]; then
  ssh-keygen -q -t ed25519 -N "" -C "$HOST myterm tools (read-only)" -f "$KEY"
  ok "created $KEY"
else
  ok "key exists: $KEY"
fi
if ! grep -qE "^Host[[:space:]]+$ALIAS\$" "$HOME/.ssh/config" 2>/dev/null; then
  printf '\nHost %s\n    HostName github.com\n    User git\n    IdentityFile %s\n    IdentitiesOnly yes\n' \
    "$ALIAS" "$KEY" >> "$HOME/.ssh/config"
  chmod 600 "$HOME/.ssh/config"
  ok "added Host $ALIAS to ~/.ssh/config"
else
  ok "Host $ALIAS already in ~/.ssh/config"
fi
reply() { ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 -T "$ALIAS" 2>&1 | tail -n 1 || true; }
if [[ "$(reply)" != *"successfully authenticated"* ]]; then
  echo
  echo "  Add this key at https://github.com/$REPO/settings/keys/new"
  echo "  Title: $HOST. Leave 'Allow write access' unticked."
  echo
  cat "$KEY.pub"
  echo
  read -r -p "  Press Enter once it's added… " _
  r="$(reply)"
  [[ "$r" == *"successfully authenticated"* ]] || die "GitHub still refuses the key: $r"
fi
ok "GitHub accepts the key"

# ------------------------------------------------------------------ 3. the scripts
bold "3. Fetching the scripts"
if [ -d "$TOOLS/.git" ]; then
  git -C "$TOOLS" pull -q --ff-only
  ok "updated $TOOLS"
else
  mkdir -p "$TOOLS"
  git -C "$TOOLS" init -q -b "$BRANCH"
  git -C "$TOOLS" remote add -t "$BRANCH" origin "$REMOTE"   # only this branch
  git -C "$TOOLS" sparse-checkout set --cone deploy
  git -C "$TOOLS" fetch -q --depth 20 origin "$BRANCH"
  git -C "$TOOLS" checkout -q -B "$BRANCH" "origin/$BRANCH"
  git -C "$TOOLS" branch -q --set-upstream-to "origin/$BRANCH"
  ok "checked out $REPO deploy/ ($BRANCH) into $TOOLS"
fi

# ------------------------------------------------------------------ 4. ~/bin links
bold "4. Installing into ~/bin"
mkdir -p "$HOME/bin"
for s in upgrade.sh deploy-apk.sh; do
  target="$HOME/bin/$s"
  if [ -e "$target" ] && [ ! -L "$target" ]; then
    mv "$target" "$target.old-$(date +%F)"
    warn "kept your old $s as $target.old-$(date +%F)"
  fi
  ln -sfn "$TOOLS/deploy/$s" "$target"
  ok "$target -> $TOOLS/deploy/$s"
done
case ":$PATH:" in *":$HOME/bin:"*) ;; *) warn "~/bin isn't on your PATH yet; log out and in (or add it in ~/.profile)";; esac

# ------------------------------------------------------------------ 5. app list
bold "5. App list for upgrade.sh"
mkdir -p "$HOME/.config/upgrade"
if [ -f "$HOME/.config/upgrade/apps.conf" ]; then
  ok "keeping your ~/.config/upgrade/apps.conf"
else
  {
    echo "# Apps upgrade.sh installs and upgrades on $HOST. One [section] per app."
    echo "# Every key is explained in $TOOLS/deploy/apps.conf.example, which also has"
    echo "# ready-made sections (MyJam, the MyTerm sync server) to copy here."
    echo "# Then: upgrade.sh --setup NAME"
  } > "$HOME/.config/upgrade/apps.conf"
  ok "created an empty ~/.config/upgrade/apps.conf"
fi

# ------------------------------------------------------------------ 6. token for deploy-apk.sh
bold "6. GitHub token for deploy-apk.sh (optional)"
TOKEN_FILE="$HOME/.config/apk/github.token"
if [ -s "$TOKEN_FILE" ]; then
  ok "token already in $TOKEN_FILE"
elif yes_no "Set up deploy-apk.sh now (needs a read-only GitHub token)?"; then
  echo "  Create one at https://github.com/settings/personal-access-tokens/new :"
  echo "    Repository access: All repositories. Permissions: Actions = Read-only, Contents = Read-only."
  echo "    Give it an expiry date and note it."
  mkdir -p "$(dirname "$TOKEN_FILE")" && chmod 700 "$(dirname "$TOKEN_FILE")"
  read -r -s -p "  Paste the token (hidden): " tok; echo
  [ -n "$tok" ] || die "No token entered."
  ( umask 077; printf '%s' "$tok" > "$TOKEN_FILE" )
  ok "saved to $TOKEN_FILE (readable only by you)"
else
  warn "skipped; run bootstrap.sh again later, or see deploy/README.md"
fi

bold "Done on $HOST."
cat <<EOF
  upgrade.sh --list              apps on this server (add them to ~/.config/upgrade/apps.conf)
  upgrade.sh --setup NAME        install an app from the list
  upgrade.sh                     upgrade everything
  upgrade.sh --self-update       update upgrade.sh and deploy-apk.sh themselves
  deploy-apk.sh                  install an Android app from your repositories on a device
EOF
