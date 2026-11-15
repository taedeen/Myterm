# Changelog

## 0.3.0

- **Keep session running (auto-reattach):** a per-host option that runs the
  shell in tmux on the server. If the connection drops (Wi-Fi to mobile data,
  sleep, server restart) MyTerm reconnects by itself and you're back where
  you were. The same tab on another device opens the same session. Falls back
  to a plain shell where tmux isn't installed.
- **Jump hosts:** "Connect via" another host. Jump hosts may themselves go
  through others; each hop uses its own saved credentials and host key check.
- **Broadcast:** type in several sessions at once (the tower button). Typing,
  key bar, paste, snippets and suggestions go to every ticked session; stored
  passwords never do.
- **SSH certificates:** the sync server can sign short-lived certificates for
  each device's own key (Settings → SSH certificate). See server/README.md.
- Backlog of further ideas in BACKLOG.md.

## 0.2.0

The first release of the whole suite: Android app, Windows app, sync server
and server tools.

**Windows app (new)**
- Installer: per-user (no admin prompt), Start menu entry, optional desktop
  shortcut, upgrades in place, clean uninstall. A portable zip is included too.
- Signed by "MyTerm" (installer, app, uninstaller and DLLs, with timestamps).
  Run `Trust-MyTerm.ps1` once per PC to trust the certificate.
- Windows Hello unlock (vault key kept with DPAPI).
- Mouse: select to copy, right-click to paste. Ctrl + = / - / 0 for text size.

**Both apps**
- Split view: two sessions side by side or top and bottom.
- Session tabs, copy and paste, compact bars, text size setting.
- Autocomplete: one tap (or Tab, then Enter) runs a suggestion; combined
  `cd a/b` suggestions; history shared across hosts and kept across restarts.
- Key bar defaults to Basic and remembers your choice.
- Passwords on hosts, port defaults to 22, trusted host keys remembered.
- Audit log kept across restarts; records syncs and errors.

**Sync**
- Self-hosted sync server (Docker), end-to-end encrypted.
- Devices list: see what syncs, rename this device, forget retired devices.
- Revoke a lost or stolen device: the vault gets a new key (optionally a new
  master password), the server moves to it, and other devices follow with
  the master password.
- Changes are sent before the app locks; sync on unlock and on return to the
  app; recovers when the server's database is reset; shows each device's
  vault id so mismatched vaults are easy to spot.

**Server tools**
- `upgrade.sh` for any app in your GitHub repositories (Docker or not),
  `--self-update`; `bootstrap.sh` sets up a new server.
- `deploy-apk.sh` installs the newest APK from any repository and remembers
  every device (IP, connection and pairing ports).
