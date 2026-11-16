# myterm: MyTerm on the Linux command line

Another device on your MyTerm vault: the same hosts, groups, jump hosts,
snippets, command history and trusted host keys as the Android and Windows
apps, through your sync server.

## Install

```
tar xzf myterm-<version>-linux-<arch>.tar.gz
install -m 755 myterm-<version>-linux-<arch>/myterm ~/bin/myterm
# optional: completions
mkdir -p ~/.local/share/bash-completion/completions
cp myterm-<version>-linux-<arch>/completions/myterm.bash ~/.local/share/bash-completion/completions/myterm
```

Or with `upgrade.sh --setup myterm-cli` (see deploy/README.md), which keeps
it up to date. x86_64 and aarch64 (Raspberry Pi 4/5) builds; glibc 2.35+
(Debian 12, Ubuntu 22.04, Raspberry Pi OS bookworm and later).

## First run

```
myterm join                 # server token, vault name, master password, scope
myterm                      # pick a host and connect
```

**Scope** decides what this machine takes from the vault:

| Scope | Gets | For |
|---|---|---|
| full | everything, stored passwords and keys included | your own laptop |
| certs (default) | hosts, snippets, history, known hosts, an SSH certificate; no stored passwords or keys | servers such as dbb |
| hosts | hosts, snippets, history, known hosts | shared machines |

## Commands

| Command | |
|---|---|
| `myterm` | host picker: type to filter, Enter connects |
| `myterm ssh dbb` | connect (jump hosts, certificates, saved credentials; asks for a password otherwise) |
| `myterm ssh dbb --tmux` | run in tmux on the server and reattach after a drop (`myterm-1` = the app's first tab) |
| `myterm ssh dbb -- uptime` | run one command |
| `myterm run 'df -h /' -t homelab` | run on every host tagged homelab, in parallel, output per host |
| `myterm multi dbb pi` | tmux panes on this machine, typing sent to all |
| `myterm hosts [words] [--json]` | list hosts |
| `myterm import [~/.ssh/config]` | add hosts from an OpenSSH config |
| `myterm cert [--renew] [--trust]` | this device's SSH certificate; `--trust` makes this machine's sshd accept MyTerm certificates |
| `myterm unlock` / `lock` | keep the vault unlocked for 15 minutes of inactivity / lock now |
| `myterm sync`, `status`, `devices`, `rename`, `audit` | sync now, state, the vault's devices, this device's name, audit log |
| `myterm completions bash` | shell completions |

## In a session

The bottom line is MyTerm's: the host, and autocomplete suggestions from your
shared history and snippets as you type. Press **Ctrl+]** then:

- **1-6** run that suggestion
- **p** type the host's stored password (only at a password prompt; **P** anyway)
- **.** disconnect
- **]** send Ctrl+] itself

## Security

- The vault file and settings live in `~/.local/share/myterm` (mode 0700/0600),
  encrypted exactly as in the apps.
- Unlocking asks for the master password, never takes it from arguments or the
  environment, and keeps the key in a small background process for 15 minutes
  of inactivity (`MYTERM_UNLOCK_MINUTES`, 0 = never keep it). That process holds
  32 bytes, locked in memory and not dumpable, and answers only your own user
  on a socket in `$XDG_RUNTIME_DIR`.
- Root on the machine can read an unlocked key, which is why servers default to
  the certs scope: the most anyone gets there is a short-lived certificate.
