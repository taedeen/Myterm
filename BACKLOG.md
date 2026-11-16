# MyTerm backlog

Ideas for MyTerm, grouped by area. ✅ = done (version), ⭐ = priority.

## Order (2026-11-16 review)

Now: Linux command line, change master password, host key alerts on every
device, `~/.ssh/config` import, port forwarding, scrollback search, confirm
before connect. Next: SFTP, FIDO2 keys, snippet library, audit export, health
card, Wake-on-LAN, Android widget. Later: recording, Proxmox/scan import,
Home Assistant actions, TPM keys on Windows, iOS.

## Staying connected
- ✅ 0.3.0 ⭐ **Auto-reattach.** "Keep session running" hosts run in tmux on
  the server; a dropped connection reconnects by itself and reattaches, and
  the same tab on another device continues the same session.
- ✅ 0.3.0 ⭐ **Jump hosts (ProxyJump).** "Connect via" another host, chains
  included, each hop with its own credentials and host key check.
- **Port forwarding.** Saved local tunnels per host (e.g. Home Assistant or
  Frigate's web UI on localhost), started with the session.
- **Wake-on-LAN** a host before connecting.

## Working faster
- ✅ 0.3.0 ⭐ **Type in several sessions at once (broadcast).**
- **Snippet library:** folders, per-host snippets, run-on-connect commands.
- **SFTP file browser:** browse, upload, download, view logs.
- **Search the scrollback,** highlighting errors and IP addresses.
- **Session recording** for replay and change records.

## Security
- ✅ 0.3.0 ⭐ **SSH certificates.** The sync server's CA signs short-lived
  certificates (8 h by default) for each device's own key; a lost device's
  access expires on its own.
- **FIDO2 / YubiKey SSH keys** (`ed25519-sk`, `ecdsa-sk`).
- **TPM-backed SSH keys on Windows.**
- **Host key change alerts on every device** through sync.
- **Audit export** of the hash-chained log to a SIEM or syslog.
- **Per-host "confirm before connect"** for production machines.

## Homelab
- **Inventory import** from `~/.ssh/config`, Proxmox, or a scan of a VLAN.
- **Host health card:** uptime, disk, load, pending updates on connect.
- **Home Assistant actions:** run a host's snippet from an HA dashboard or
  automation through the sync server.

## Platforms
- ⭐ **Linux command line (`myterm`).** One Rust binary on the shared crates:
  `myterm ssh` with autocomplete and the password key, `myterm agent` plus a
  generated `~/.ssh/config` so plain ssh/scp/rsync/git use MyTerm's keys,
  certificates and jump hosts, parallel `run` across tagged hosts, per-device
  sync scopes (servers get hosts and certificates, not passwords). Design:
  "MyTerm: Backlog review and Linux command-line design" (Claude doc).
- iOS app, if needed. (Linux desktop app and Mosh dropped for now: the
  command line and tmux reattach cover most of their value.)
- Android home-screen widget for favourite hosts; external keyboard and mouse
  on tablets.
- Earlier gaps: change the master password without revoking a device.
