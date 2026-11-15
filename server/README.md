# MyTerm sync server

Keeps MyTerm's hosts, keys, credentials, snippets, trusted host keys and
command history in step across your devices. Self-hosted, one container,
no third-party Python packages.

**The server never sees your data.** Devices encrypt every record with a key
derived from the vault master key before uploading. The server stores opaque
record keys and ciphertext, and keeps nothing that can decrypt them.

## Run it (e.g. on `server1`)

The easy way is `upgrade.sh --setup myterm-sync` (see `deploy/README.md`): it
does all of the below and keeps the server up to date. By hand:

```sh
cd server
echo "MYTERM_SYNC_TOKEN=$(openssl rand -hex 24)" > .env   # keep this; you'll type it into the app
chmod 600 .env
mkdir -p data && sudo chown 10001:10001 data              # the container runs as uid 10001
docker compose up -d --build
curl http://localhost:7341/v1/health                       # {"ok": true, "protocol": 1}
```

Data lives in `./data/sync.db` (SQLite). Back that folder up; it is useless
without your master password.

Without Docker: `MYTERM_SYNC_TOKEN=... MYTERM_SYNC_DB=./data/sync.db python -m myterm_sync`.

## Connect your devices

1. **First device:** Settings → Sync → *New sync vault*. The server address
   defaults to `https://myterm.example.com` (the reverse proxy in front
   of this container); enter the server token and a vault name.
2. **Every other device:** Settings → Sync → *Join existing*. Same address,
   token and vault name, plus the **master password of the first device's
   vault**. The joining device's own hosts and credentials are kept and merged
   in, and from then on that master password unlocks it too.

After that, devices sync on unlock, a few seconds after each change, and every
minute while unlocked. Settings shows the last sync and any error.

## How it works

| Secret | Who has it | What it allows |
| --- | --- | --- |
| Master password | You | Unwraps the vault master key (Argon2id) |
| Vault master key (MK) | Each unlocked device | Encrypts every record; derives everything below |
| Server token (`MYTERM_SYNC_TOKEN`) | You, at setup | Creating a vault, and looking one up by name to join |
| Vault token = HKDF(MK, "sync-auth") | Each device (derived) | Reading and writing that vault's records. The server stores only its SHA-256 |
| Vault id = HKDF(MK, "sync-vault-id") | Each device (derived) | Addresses the vault on the server |

- **Joining** downloads the password-wrapped MK (the same blob each device
  keeps locally), so a new device needs the master password and nothing else
  secret. Anyone with the server token could fetch that blob and try to
  guess the master password offline; Argon2id, a 12+ character password and
  keeping the token private are what stand in the way. Lookups are rate limited.
- **Records:** each upload is one vault record, sealed with XChaCha20-Poly1305
  under MK. Its key on the server is SHA-256(HKDF(MK, "sync-record-key") ‖ record id),
  so even record ids stay hidden.
- **No lost edits:** the server keeps each device's latest copy of each record
  separately. Devices merge copies field by field (newest edit wins, ties by
  device id; deletions are tombstones; history counters are per device), so
  two devices editing at once never overwrite each other. Merging is
  order-independent.
- **Devices:** Settings → Sync → Devices lists what has synced. *Forget* a
  retired device to remove its copies. For a **lost or stolen** device, use
  *Revoke*: your device makes a new master key (optionally with a new master
  password), re-encrypts everything, and moves the vault to a successor on the
  server. The old vault's data is deleted and it only answers "moved". Your
  other devices then ask for the master password once to follow it. The lost
  device holds the old key only, and without the master password it can't
  join the new vault. If the password may be known, set a new one when revoking.
- **Never uploaded:** the audit log, each device's full local history (which
  includes hosts with history sync turned off), and credentials marked
  "This device only". Hardware-bound keys can't leave the device at all.

## API (protocol 1)

| Method and path | Auth | Purpose |
| --- | --- | --- |
| `GET /v1/health` | none | Liveness |
| `POST /v1/vaults` | server token | Register `{name, vault_id, auth_hash, wrapped}` |
| `GET /v1/vaults/lookup?name=` | server token | `{vault_id, wrapped}` for joining |
| `POST /v1/vaults/{id}/records` | vault token | Upload `{device_id, records: [{key, data}]}` |
| `GET /v1/vaults/{id}/records?since=&limit=` | vault token | Changes since a cursor: `{records, cursor, more}` |
| `PUT /v1/vaults/{id}/wrapped` | vault token | Replace the wrapped MK (master password change) |
| `DELETE /v1/vaults/{id}` | vault token | Delete the vault and all its records |
| `GET /v1/vaults/{id}/devices` | vault token | Devices that have synced: `{devices: [{device_id, name, first_seen, last_seen, records}]}` |
| `DELETE /v1/vaults/{id}/devices/{device}` | vault token | Forget a retired device and remove its copies |
| `POST /v1/vaults/{id}/successor` | vault token | Revoke: move to `{vault_id, auth_hash, wrapped}` under a new MK |
| `POST /v1/vaults/{id}/ssh-cert` | vault token | Sign `{public_key}`: `{certificate, principals, hours}` |
| `GET /v1/ssh-ca.pub` | none | The CA public key (text), for `TrustedUserCAKeys` |
| `GET /v1/vaults/{id}/successor` | old vault token | After a move: `{vault_id, name, wrapped}` to rejoin with the master password |

Server token: `Authorization: Bearer …`. Vault token: `X-MyTerm-Vault-Token: …`.
Devices also send `X-MyTerm-Device` (id) and `X-MyTerm-Device-Name` for the
device list. A moved vault answers everything else with `410 {"moved": true}`.

## SSH certificates (optional)

The sync server can act as an SSH certificate authority for your devices.
Each device has its own key (never synced) and gets a short-lived certificate
for it, so you don't copy public keys to servers, and a lost device's access
ends when its last certificate expires (revoke it first so it can't renew).

1. Turn it on in `server/.env` (then `upgrade.sh myterm-sync`):
   ```
   MYTERM_SYNC_CERT_PRINCIPALS=youruser,root   # login names certificates allow
   MYTERM_SYNC_CERT_HOURS=8                   # 1-168
   ```
   The CA key is created as `data/ssh_user_ca` on first start. Back it up
   with the rest of `data/`; anyone with it can sign logins.
2. On each server that should accept them (Settings → SSH certificate →
   Server setup in the app shows the same with your address filled in):
   ```
   curl -fsS https://myterm.example.com/v1/ssh-ca.pub | sudo tee /etc/ssh/myterm_user_ca.pub
   echo "TrustedUserCAKeys /etc/ssh/myterm_user_ca.pub" | sudo tee /etc/ssh/sshd_config.d/myterm-ca.conf
   sudo sshd -t && (sudo systemctl reload ssh || sudo systemctl reload sshd)
   ```
3. In the app, turn on **Sign in with MyTerm certificate** for those hosts.
   Devices renew the certificate when it has under 2 hours left.

Certificates carry the key id `myterm:<vault>:<device name>`, so the server's
auth log shows which device logged in.

## Security notes

- **Behind a reverse proxy:** point it at `http://<host>:7341`. The
  container needs no TLS of its own. If the proxy runs on the same host, you
  can publish the port on localhost only (`"127.0.0.1:7341:7341"` in
  `docker-compose.yml`) so nothing bypasses the proxy.
  `MYTERM_SYNC_TRUST_PROXY=true` (on in the compose file) makes rate limits
  and logs use the client IP from `X-Forwarded-For`; turn it off if clients
  can reach port 7341 directly, since they could forge that header.
- **Use TLS beyond your LAN.** Plain http is fine on a home network because
  records are end-to-end encrypted, but the tokens travel in the clear. For
  anything internet-facing, put it behind a reverse proxy with a real
  certificate (Caddy, Traefik, nginx), or set `MYTERM_SYNC_TLS_CERT` and
  `MYTERM_SYNC_TLS_KEY`.
- The container runs as a non-root user with a read-only filesystem, no
  capabilities and `no-new-privileges`.
- Logs contain client IP, method, path (no query string) and status, never
  tokens or bodies.
- **Not yet built:** revoking a device (needs rotating MK and re-encrypting),
  and changing the master password from the app.
