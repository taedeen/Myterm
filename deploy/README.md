# Server tools: upgrade.sh, deploy-apk.sh, bootstrap.sh

| Script | What it does |
| --- | --- |
| `upgrade.sh` | Installs and upgrades a server's Docker apps from their GitHub repos (MyJam, the MyTerm sync server, anything added later) |
| `deploy-apk.sh` | Finds every Android app your GitHub repos build, downloads the newest, and installs it on a phone, tablet or Fire TV over adb |
| `bootstrap.sh` | Sets both up on a server, once |

All three live in this folder. `bootstrap.sh` checks out just this folder on a
server and links the scripts into `~/bin`, so `upgrade.sh --self-update`
keeps them current.

## Public or private repository

The scripts work either way and check for themselves:

| | Private repository | Public repository |
| --- | --- | --- |
| Get `bootstrap.sh` | Copy it from a server that has it, or download it with your token (`curl -H "Authorization: Bearer TOKEN" -H "Accept: application/vnd.github.raw" -o bootstrap.sh https://api.github.com/repos/taedeen/Myterm/contents/deploy/bootstrap.sh`) | `curl -fsSLO https://raw.githubusercontent.com/taedeen/Myterm/main/deploy/bootstrap.sh` |
| Scripts and Docker apps | Fetched over SSH with a read-only deploy key per server | Fetched over HTTPS, no key |
| Release binaries (`myterm`) | Need the GitHub token | No token (GitHub allows 60 requests an hour without one) |
| `deploy-apk.sh` | Token: every repository it can see, releases and build artifacts | No token: release APKs of the repositories in `~/.config/apk/public-repos` (default `taedeen/Myterm`) |

Servers set up while the repository was private keep using their deploy key
after it goes public; that keeps working.

## Set up a server (server1 or any other)

You need SSH access to the server, a user with `sudo`, and Docker if it will
run Docker apps. Each server gets **its own** read-only deploy key: GitHub
allows a deploy key on one repository only, and a lost server's key can then
be removed without touching the others.

**1. Copy `bootstrap.sh` to the server.** The repository is private, so the
server can't download it before it has a key. From a machine that has it
(server1, after its own setup):
```sh
scp ~/.local/share/myterm-tools/deploy/bootstrap.sh NEWSERVER:
```
Or open `deploy/bootstrap.sh` on GitHub, copy it, and paste it into
`nano bootstrap.sh` on the server.

**2. Run it on the server:**
```sh
bash bootstrap.sh
```
It walks through six steps and asks before anything that needs sudo:
1. Checks for git, curl, python3, ssh and docker compose, and offers to
   `apt install` what's missing (plus sqlite3 and adb, which are optional).
2. Creates the key `~/.ssh/github-myterm_deploy` and the SSH alias
   `github-myterm`, prints the public key, and waits while you add it at
   https://github.com/taedeen/Myterm/settings/keys/new. Title it with the
   server's name and leave *Allow write access* unticked.
3. Checks out `deploy/` into `~/.local/share/myterm-tools`.
4. Links `upgrade.sh` and `deploy-apk.sh` into `~/bin`. An existing plain
   copy is kept as `upgrade.sh.old-<date>`.
5. Creates an empty `~/.config/upgrade/apps.conf` (an existing one is kept).
6. Optionally stores the GitHub token `deploy-apk.sh` needs (see below).

Running it again is safe: finished steps are skipped.

**3. List the server's apps** in `~/.config/upgrade/apps.conf`. Copy sections
from `~/.local/share/myterm-tools/deploy/apps.conf.example` (MyJam and the
MyTerm sync server are there, with every key explained), then install each:
```sh
upgrade.sh --setup myterm-sync
```
For an app from another private repository, give it its own SSH alias in
`remote` (`git@github-NAME:owner/repo.git`). `--setup` creates that
repository's deploy key and SSH entry, and tells you where to add the key.

**On server1**, which already has `upgrade.sh` in `~/bin` and the `github-myterm`
key: run `bash bootstrap.sh` as well. It reuses the key, keeps your
`apps.conf`, and turns `~/bin/upgrade.sh` into a self-updating link. Your
old `~/bin/fetch-apk.sh` and `deploy-apk.sh` aren't touched; the new
`deploy-apk.sh` replaces both.

## upgrade.sh

```sh
upgrade.sh                  # upgrade every app that has something new
upgrade.sh myterm-sync      # just one
upgrade.sh myjam --force    # rebuild and restart even if up to date
upgrade.sh --list           # versions, and what's new on GitHub
upgrade.sh --setup NAME     # first install: deploy key, clone, first start
upgrade.sh --self-update    # update upgrade.sh and deploy-apk.sh themselves
```

For each app it:
- refuses to run if the app's files were edited by hand;
- shows what's new;
- backs up the files listed in `backup`, with SQLite databases snapshotted
  consistently;
- fast-forwards, rebuilds, and waits for the health check.

If the build fails, the app stays on its old version. If the health check
fails, it prints the commands to go back. Backups go to
`~/upgrade-backups/<app>/`, keeping 10.

To keep the old habit in MyJam's folder:
```sh
printf '#!/bin/sh\nexec "$HOME/bin/upgrade.sh" myjam "$@"\n' > ~/docker_volumes/MyJam/upgrade.sh
```

### Release binaries (the myterm command line)

Apps with `kind = release` in `apps.conf` are programs from a repository's
GitHub releases rather than Docker apps. The `[myterm-cli]` section in
`apps.conf.example` installs the Linux command line:

```
upgrade.sh --self-update          # get the newest scripts
upgrade.sh --add myterm-cli       # adds [myterm-cli] to apps.conf and installs it
myterm join                       # then join your vault
```

It uses the same read-only GitHub token as deploy-apk.sh, checks the download
against the release's `SHA256SUMS.txt`, unpacks it to
`~/.local/share/myterm-cli/<version>` (keeping the previous version) and links
`~/bin/myterm`. A plain `upgrade.sh` updates it with everything else. A Raspberry
Pi without Docker can list only `[myterm-cli]`.

## deploy-apk.sh

```sh
deploy-apk.sh               # choose an app, download its newest build, install it
deploy-apk.sh myterm        # the app whose repo name contains "myterm"
deploy-apk.sh --list        # every app found, newest build, branch, date, size
deploy-apk.sh --fetch       # download new builds only (for cron), no install
deploy-apk.sh --refresh     # look through your repositories again
deploy-apk.sh some.apk      # install a file you already have
```

**Finding apps:** it checks every repository the token can see for:
- GitHub Release assets ending in `.apk` (last-litre-android), and
- GitHub Actions build artifacts with an APK inside (MyJam-Android, Myterm).

It looks inside each kind of build once and remembers the answer, so new
repositories with APKs appear by themselves. Builds from a repo's default
branch are preferred; if there are none, the newest from any branch is used
and `--list` says which branch. Downloads are kept in `~/apks/<repo>/`
(newest as `<repo>-latest.apk`, plus 3 older).

**Installing:**
- **Picking the device:** choose a device that's already connected (USB, or
  wireless from earlier), one from the **known devices** list, or a new one.
  Every device you install to is remembered in `~/.config/apk/adb-devices`
  (IP, connection port, pairing port, model), most recent first, so pressing
  Enter reuses its address and ports. A known device skips pairing unless you
  ask to pair again (then its saved pairing port is offered). Android changes
  the ports when Wireless debugging is turned back on; type the new ones and
  they're saved. The device saved by older versions (`~/.config/myjam/adb-last`)
  is brought in on first run.
- **Signed with a different key:** if the device's copy was signed with
  another key, it explains that an in-place update isn't possible. It
  uninstalls first only if you type `yes`, because that deletes the app's
  data on the device.

**The token** (`~/.config/apk/github.token`, mode 600): create a fine-grained
token at https://github.com/settings/personal-access-tokens/new with
*Repository access: All repositories* and *Actions* and *Contents* read-only.
Give it an expiry date. The old MyJam-only token in
`~/.config/myjam/github-apk.token` still works as a fallback, but it only
sees MyJam-Android.

To keep the newest builds on the server every hour (replaces `fetch-apk.sh`):
```
17 * * * * $HOME/bin/deploy-apk.sh --fetch >> $HOME/apks/fetch.log 2>&1
```

## Notes

- Docker runs through `sudo`. If your user is in the `docker` group,
  `export UPGRADE_SUDO=` skips it.
- The scripts follow the Myterm repo's `main` branch (released versions).
  `upgrade.sh --self-update` moves a copy set up from `dev` over to `main`.
  `MYTERM_TOOLS_BRANCH=dev` follows the latest work instead.
- Nothing runs on a timer unless you add the cron line yourself, so nothing
  changes on a server without you seeing it first.
