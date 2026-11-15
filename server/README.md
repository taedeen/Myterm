# MyTerm sync server

Keeps MyTerm's hosts, keys, credentials, snippets, trusted host keys and
command history in step across your devices. Self-hosted, one container,
no third-party Python packages.

**The server never sees your data.** Devices encrypt every record with a key
derived from the vault master key before uploading. The server stores opaque
record keys and ciphertext, and keeps nothing that can decrypt them.

## Run it (e.g. on `dbb`)

```sh
cd server
echo "MYTERM_SYNC_TOKEN=$(openssl rand -hex 24)" > .env   # keep this; you'll type it into the app
docker compose up -d --build
curl http://localhost:7341/v1/health                       # {"ok": true, "protocol": 1}
```

Data lives in `./data/sync.db` (SQLite). Back that folder up; it is useless
without your master password.

Without Docker: `MYTERM_SYNC_TOKEN=... MYTERM_SYNC_DB=./data/sync.db python -m myterm_sync`.

## Connect your devices

1. **First device:** Settings → Sync → *New sync vault*. Enter the server
   address (`http://192.168.0.17:7341`), the server token and a vault name.
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

Server token: `Authorization: Bearer …`. Vault token: `X-MyTerm-Vault-Token: …`.

## Security notes

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
