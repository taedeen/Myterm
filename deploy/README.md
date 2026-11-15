# upgrade.sh: dbb's apps, installed and upgraded from GitHub

One script for every Docker app on dbb that comes from a GitHub repository:
MyJam, the MyTerm sync server, and anything added later. The apps are listed
in `~/.config/upgrade/apps.conf`; a new app is a new section there.

```sh
upgrade.sh                  # upgrade every app that has something new
upgrade.sh myterm-sync      # just one
upgrade.sh myjam --force    # rebuild and restart even if up to date
upgrade.sh --list           # versions, and what's new on GitHub
upgrade.sh --setup NAME     # first install: deploy key, clone, first start
```

For each app it refuses to run if its files were edited by hand, shows what's
new, backs up the files listed in `backup` (SQLite databases snapshotted
consistently), fast-forwards, rebuilds, and waits for the health check. A
failed build leaves the app on its old version. A failed health check prints
the commands to go back. Backups go to `~/upgrade-backups/<app>/`, keeping 10.

## Install on dbb (once)

**1. A read-only deploy key for this repository** (each repo needs its own):
```sh
ssh-keygen -t ed25519 -f ~/.ssh/github-myterm_deploy -N "" -C "dbb myterm (read-only)"
cat ~/.ssh/github-myterm_deploy.pub
```
Add it at https://github.com/taedeen/Myterm/settings/keys/new with *Allow
write access* unticked. Then:
```sh
cat >> ~/.ssh/config <<'CFG'

Host github-myterm
    HostName github.com
    User git
    IdentityFile ~/.ssh/github-myterm_deploy
    IdentitiesOnly yes
CFG
chmod 600 ~/.ssh/config
ssh -T github-myterm        # "Hi taedeen/Myterm! You've successfully authenticated…"
```

**2. Install the script and the app list:**
```sh
git clone -q --depth 1 -b dev git@github-myterm:taedeen/Myterm.git /tmp/myterm-tools
mkdir -p ~/bin ~/.config/upgrade
install -m 755 /tmp/myterm-tools/deploy/upgrade.sh ~/bin/upgrade.sh
cp -n /tmp/myterm-tools/deploy/apps.conf.example ~/.config/upgrade/apps.conf
rm -rf /tmp/myterm-tools
```
`~/bin` is on your PATH after your next login (Debian/Ubuntu add it if it exists).

**3. Point MyJam's old script at the new one**, so `./upgrade.sh` in the MyJam
folder keeps working:
```sh
printf '#!/bin/sh\nexec "$HOME/bin/upgrade.sh" myjam "$@"\n' > ~/docker_volumes/MyJam/upgrade.sh
```
(MyJam's backups now go to `~/upgrade-backups/myjam/`; the old `~/myjam-backups` stays as it is.)

**4. Install the sync server:**
```sh
upgrade.sh --setup myterm-sync
```
It checks out only the repo's `server/` folder into `~/docker_volumes/myterm-sync`,
creates `server/.env` with a random token, gives the data folder to the
container's user, starts it and waits for its health check. The token you'll
type into the app is in `~/docker_volumes/myterm-sync/server/.env`.

Then point your reverse proxy at `http://<dbb>:7341`. If the proxy runs on
dbb itself, keep the port off the network with a `docker-compose.override.yml`
in `~/docker_volumes/myterm-sync/server/`. It isn't tracked by Git, so
`upgrade.sh` leaves it alone (`!override` needs Docker Compose 2.24 or later):
```yaml
services:
  myterm-sync:
    ports: !override
      - "127.0.0.1:7341:7341"
```

## Adding a future app

Add a section to `~/.config/upgrade/apps.conf` (the example file documents
every key), then `upgrade.sh --setup NAME`. For a private repo, give it its
own SSH alias (`git@github-NAME:owner/repo.git`); `--setup` creates the deploy
key and the SSH entry, and tells you where to add the key on GitHub.

## Notes

- Docker runs through `sudo`, as before. If your user is in the `docker`
  group, `export UPGRADE_SUDO=` skips it.
- `myterm-sync` follows the `dev` branch until pull request #1 is merged
  into `main`; then change `branch = main` in `apps.conf`.
- The script runs only when you run it, so nothing changes on dbb without you seeing it first.
