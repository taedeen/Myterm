"""MyTerm sync server.

A small, self-hosted store for end-to-end encrypted vault data. Devices
upload records that they have already encrypted with a key derived from the
vault master key; the server never sees hostnames, commands, passwords or
record ids, only opaque keys and ciphertext.

Two secrets protect it:

* The **server token** (``MYTERM_SYNC_TOKEN``) gates creating a vault and
  looking one up by name. Only people you give it to can use the server.
* Each vault's **vault token** gates reading and writing that vault's
  records. Devices derive it from the vault master key; the server stores
  only its SHA-256 hash.

Each device's latest copy of each record is kept separately, so concurrent
edits from two devices never overwrite each other on the server; clients
merge them (field-level last-writer-wins, see crates/core/src/sync.rs).

Devices identify themselves (X-MyTerm-Device, X-MyTerm-Device-Name) so the
vault's device list can show what is syncing. A retired device can be
forgotten (its copies removed). A lost device is revoked by moving the vault:
a remaining device makes a new master key and registers the successor vault
with the old vault's token; the old vault's data is deleted and it answers
410 Gone, pointing other devices at the successor, which they join with the
master password (which the lost device's thief doesn't have).

Standard library only, so it runs anywhere Python 3.11+ runs.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import hmac
import json
import logging
import os
import re
import sqlite3
import ssl
import threading
import time
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

PROTOCOL_VERSION = 1
MAX_BODY = 16 * 1024 * 1024
MAX_RECORD = 4 * 1024 * 1024
MAX_RECORDS_PER_PUSH = 1000
MAX_PAGE = 500

NAME_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{2,63}$")
VAULT_ID_RE = re.compile(r"^[0-9a-f]{32}$")
HASH_RE = re.compile(r"^[0-9a-f]{64}$")
KEY_RE = re.compile(r"^[0-9a-f]{64}$")
DEVICE_RE = re.compile(r"^[A-Za-z0-9-]{1,64}$")

log = logging.getLogger("myterm-sync")

SCHEMA = """
CREATE TABLE IF NOT EXISTS vaults (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    auth_hash  TEXT NOT NULL,
    wrapped    TEXT NOT NULL,
    seq        INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS records (
    vault_id   TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    key        TEXT NOT NULL,
    device_id  TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    data       BLOB NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, key, device_id)
);
CREATE INDEX IF NOT EXISTS records_by_seq ON records (vault_id, seq);
CREATE TABLE IF NOT EXISTS devices (
    vault_id   TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    device_id  TEXT NOT NULL,
    name       TEXT NOT NULL DEFAULT '',
    first_seen INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL,
    PRIMARY KEY (vault_id, device_id)
);
"""

DEVICE_NAME_MAX = 64


class ApiError(Exception):
    def __init__(self, status: HTTPStatus, message: str, **extra):
        super().__init__(message)
        self.status = status
        self.message = message
        self.extra = extra


class Store:
    """SQLite storage. One connection guarded by a lock: simple and plenty
    fast for a handful of devices."""

    def __init__(self, path: str):
        if path != ":memory:":
            os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
        self.db = sqlite3.connect(path, check_same_thread=False, isolation_level=None)
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA foreign_keys=ON")
        self.db.executescript(SCHEMA)
        cols = [r[1] for r in self.db.execute("PRAGMA table_info(vaults)")]
        if "moved_to" not in cols:  # databases from before device revocation
            self.db.execute("ALTER TABLE vaults ADD COLUMN moved_to TEXT")
        self.lock = threading.Lock()

    def create_vault(self, vault_id: str, name: str, auth_hash: str, wrapped: dict) -> None:
        with self.lock:
            try:
                self.db.execute(
                    "INSERT INTO vaults (id, name, auth_hash, wrapped, created_at) VALUES (?, ?, ?, ?, ?)",
                    (vault_id, name, auth_hash, json.dumps(wrapped), int(time.time())),
                )
            except sqlite3.IntegrityError:
                raise ApiError(HTTPStatus.CONFLICT, "a vault with that name or id already exists")

    def lookup(self, name: str) -> dict | None:
        with self.lock:
            row = self.db.execute("SELECT id, wrapped FROM vaults WHERE name = ?", (name,)).fetchone()
        if row is None:
            return None
        return {"vault_id": row[0], "wrapped": json.loads(row[1])}

    def auth_hash(self, vault_id: str) -> tuple[str, str | None] | None:
        """(auth_hash, moved_to) of a vault, or None."""
        with self.lock:
            row = self.db.execute("SELECT auth_hash, moved_to FROM vaults WHERE id = ?", (vault_id,)).fetchone()
        return (row[0], row[1]) if row else None

    def successor(self, vault_id: str) -> dict | None:
        with self.lock:
            row = self.db.execute(
                "SELECT n.id, n.name, n.wrapped FROM vaults o JOIN vaults n ON n.id = o.moved_to WHERE o.id = ?",
                (vault_id,),
            ).fetchone()
        return {"vault_id": row[0], "name": row[1], "wrapped": json.loads(row[2])} if row else None

    def move_vault(self, old_id: str, new_id: str, auth_hash: str, wrapped: dict) -> None:
        """Replaces a vault with a successor under the same name; the old
        one keeps only a pointer (its records and devices are deleted)."""
        now = int(time.time())
        with self.lock:
            self.db.execute("BEGIN IMMEDIATE")
            try:
                (name,) = self.db.execute("SELECT name FROM vaults WHERE id = ?", (old_id,)).fetchone()
                self.db.execute(
                    "UPDATE vaults SET name = ?, moved_to = ? WHERE id = ?", (f"{name}~moved~{old_id}", new_id, old_id)
                )
                # Anything that pointed at the old vault now points at the new one.
                self.db.execute("UPDATE vaults SET moved_to = ? WHERE moved_to = ?", (new_id, old_id))
                self.db.execute(
                    "INSERT INTO vaults (id, name, auth_hash, wrapped, created_at) VALUES (?, ?, ?, ?, ?)",
                    (new_id, name, auth_hash, json.dumps(wrapped), now),
                )
                self.db.execute("DELETE FROM records WHERE vault_id = ?", (old_id,))
                self.db.execute("DELETE FROM devices WHERE vault_id = ?", (old_id,))
                self.db.execute("COMMIT")
            except sqlite3.IntegrityError:
                self.db.execute("ROLLBACK")
                raise ApiError(HTTPStatus.CONFLICT, "that successor vault already exists")
            except BaseException:
                self.db.execute("ROLLBACK")
                raise

    def touch_device(self, vault_id: str, device_id: str, name: str | None) -> None:
        now = int(time.time())
        with self.lock:
            self.db.execute(
                "INSERT INTO devices (vault_id, device_id, name, first_seen, last_seen) VALUES (?, ?, ?, ?, ?) "
                "ON CONFLICT (vault_id, device_id) DO UPDATE SET last_seen = excluded.last_seen, "
                "name = CASE WHEN excluded.name != '' THEN excluded.name ELSE devices.name END",
                (vault_id, device_id, name or "", now, now),
            )

    def devices(self, vault_id: str) -> list[dict]:
        with self.lock:
            rows = self.db.execute(
                "SELECT d.device_id, d.name, d.first_seen, d.last_seen, "
                "(SELECT COUNT(*) FROM records r WHERE r.vault_id = d.vault_id AND r.device_id = d.device_id) "
                "FROM devices d WHERE d.vault_id = ? ORDER BY d.last_seen DESC",
                (vault_id,),
            ).fetchall()
        return [
            {"device_id": d, "name": n, "first_seen": f, "last_seen": l, "records": c} for d, n, f, l, c in rows
        ]

    def forget_device(self, vault_id: str, device_id: str) -> bool:
        with self.lock:
            self.db.execute("BEGIN IMMEDIATE")
            try:
                cur = self.db.execute("DELETE FROM devices WHERE vault_id = ? AND device_id = ?", (vault_id, device_id))
                self.db.execute("DELETE FROM records WHERE vault_id = ? AND device_id = ?", (vault_id, device_id))
                self.db.execute("COMMIT")
            except BaseException:
                self.db.execute("ROLLBACK")
                raise
        return cur.rowcount > 0

    def set_wrapped(self, vault_id: str, wrapped: dict) -> None:
        with self.lock:
            self.db.execute("UPDATE vaults SET wrapped = ? WHERE id = ?", (json.dumps(wrapped), vault_id))

    def push(self, vault_id: str, device_id: str, records: list[tuple[str, bytes]]) -> int:
        now = int(time.time())
        with self.lock:
            self.db.execute("BEGIN IMMEDIATE")
            try:
                (seq,) = self.db.execute("SELECT seq FROM vaults WHERE id = ?", (vault_id,)).fetchone()
                for key, data in records:
                    seq += 1
                    self.db.execute(
                        "INSERT INTO records (vault_id, key, device_id, seq, data, updated_at) VALUES (?, ?, ?, ?, ?, ?) "
                        "ON CONFLICT (vault_id, key, device_id) DO UPDATE SET seq = excluded.seq, "
                        "data = excluded.data, updated_at = excluded.updated_at",
                        (vault_id, key, device_id, seq, data, now),
                    )
                self.db.execute("UPDATE vaults SET seq = ? WHERE id = ?", (seq, vault_id))
                self.db.execute("COMMIT")
            except BaseException:
                self.db.execute("ROLLBACK")
                raise
        return seq

    def pull(self, vault_id: str, since: int, limit: int) -> tuple[list[dict], int, bool]:
        with self.lock:
            rows = self.db.execute(
                "SELECT seq, key, device_id, data FROM records WHERE vault_id = ? AND seq > ? ORDER BY seq LIMIT ?",
                (vault_id, since, limit + 1),
            ).fetchall()
            (head,) = self.db.execute("SELECT seq FROM vaults WHERE id = ?", (vault_id,)).fetchone()
        more = len(rows) > limit
        rows = rows[:limit]
        out = [
            {"seq": s, "key": k, "device_id": d, "data": base64.b64encode(data).decode()} for s, k, d, data in rows
        ]
        # Cursor for the next call: the last row returned, or the head when
        # the client is up to date.
        cursor = rows[-1][0] if rows else head
        return out, cursor, more

    def head(self, vault_id: str) -> int:
        with self.lock:
            (seq,) = self.db.execute("SELECT seq FROM vaults WHERE id = ?", (vault_id,)).fetchone()
        return seq

    def delete_vault(self, vault_id: str) -> None:
        with self.lock:
            self.db.execute("DELETE FROM vaults WHERE id = ?", (vault_id,))

    def stats(self) -> dict:
        with self.lock:
            (vaults,) = self.db.execute("SELECT COUNT(*) FROM vaults").fetchone()
        return {"vaults": vaults}


class RateLimiter:
    """Per-client sliding window for unauthenticated lookups and failed auth."""

    def __init__(self, limit: int = 30, window: float = 60.0):
        self.limit = limit
        self.window = window
        self.hits: dict[str, list[float]] = {}
        self.lock = threading.Lock()

    def allow(self, client: str) -> bool:
        now = time.monotonic()
        with self.lock:
            hits = [t for t in self.hits.get(client, []) if now - t < self.window]
            allowed = len(hits) < self.limit
            if allowed:
                hits.append(now)
            self.hits[client] = hits
            if len(self.hits) > 10_000:  # bound memory
                self.hits = {c: h for c, h in self.hits.items() if h}
            return allowed


def sha256_hex(s: str) -> str:
    return hashlib.sha256(s.encode()).hexdigest()


def validate_wrapped(w: object) -> dict:
    if not isinstance(w, dict):
        raise ApiError(HTTPStatus.BAD_REQUEST, "wrapped must be an object")
    try:
        out = {
            "password_wrapped": w["password_wrapped"],
            "kdf_salt": w["kdf_salt"],
            "kdf_m_kib": int(w["kdf_m_kib"]),
            "kdf_t": int(w["kdf_t"]),
            "kdf_p": int(w["kdf_p"]),
        }
        base64.b64decode(out["password_wrapped"], validate=True)
        if len(base64.b64decode(out["kdf_salt"], validate=True)) != 16:
            raise ValueError("salt length")
    except (KeyError, ValueError, TypeError) as e:
        raise ApiError(HTTPStatus.BAD_REQUEST, f"invalid wrapped key: {e}")
    if not (8 * 1024 <= out["kdf_m_kib"] <= 4 * 1024 * 1024 and 1 <= out["kdf_t"] <= 64 and 1 <= out["kdf_p"] <= 64):
        raise ApiError(HTTPStatus.BAD_REQUEST, "KDF parameters out of range")
    return out


class App:
    def __init__(
        self,
        store: Store,
        server_token: str,
        limiter: RateLimiter | None = None,
        trust_proxy: bool = False,
    ):
        if len(server_token) < 24:
            raise ValueError("server token must be at least 24 characters")
        self.store = store
        self.server_token_hash = sha256_hex(server_token)
        self.limiter = limiter or RateLimiter()
        # Behind a reverse proxy every request comes from the proxy; trust its
        # X-Forwarded-For only when told to, or clients could spoof it.
        self.trust_proxy = trust_proxy

    # ---- auth

    def require_server_token(self, h: "Handler") -> None:
        auth = h.headers.get("Authorization", "")
        token = auth[7:] if auth.startswith("Bearer ") else ""
        if not hmac.compare_digest(sha256_hex(token), self.server_token_hash):
            self.limit(h)
            raise ApiError(HTTPStatus.UNAUTHORIZED, "server token required")

    def require_vault(self, h: "Handler", vault_id: str) -> str | None:
        """Checks the vault token. Returns the successor's id if the vault
        has moved (the caller decides what a moved vault may still do)."""
        if not VAULT_ID_RE.match(vault_id):
            raise ApiError(HTTPStatus.NOT_FOUND, "no such vault")
        found = self.store.auth_hash(vault_id)
        stored = found[0] if found else None
        token = h.headers.get("X-MyTerm-Vault-Token", "")
        # Compare even when the vault is missing, so timing doesn't reveal which ids exist.
        ok = hmac.compare_digest(sha256_hex(token), stored or "0" * 64) and stored is not None
        if not ok:
            self.limit(h)
            raise ApiError(HTTPStatus.UNAUTHORIZED, "vault token rejected")
        return found[1] if found else None

    def device_header(self, h: "Handler") -> tuple[str | None, str | None]:
        device = h.headers.get("X-MyTerm-Device", "")
        if not DEVICE_RE.match(device):
            return None, None
        name = "".join(c for c in h.headers.get("X-MyTerm-Device-Name", "") if c.isprintable()).strip()
        return device, name[:DEVICE_NAME_MAX]

    def limit(self, h: "Handler") -> None:
        if not self.limiter.allow(h.client_ip()):
            raise ApiError(HTTPStatus.TOO_MANY_REQUESTS, "too many attempts, slow down")

    # ---- routing

    def handle(self, h: "Handler", method: str) -> tuple[HTTPStatus, dict]:
        url = urlparse(h.path)
        parts = [p for p in url.path.split("/") if p]
        query = parse_qs(url.query)

        if parts == ["v1", "health"] and method == "GET":
            return HTTPStatus.OK, {"ok": True, "protocol": PROTOCOL_VERSION}

        if parts == ["v1", "vaults"] and method == "POST":
            self.require_server_token(h)
            body = h.json_body()
            name = str(body.get("name", "")).strip().lower()
            vault_id = str(body.get("vault_id", ""))
            auth_hash = str(body.get("auth_hash", ""))
            if not NAME_RE.match(name):
                raise ApiError(HTTPStatus.BAD_REQUEST, "name: 3-64 chars, a-z 0-9 . _ -")
            if not VAULT_ID_RE.match(vault_id) or not HASH_RE.match(auth_hash):
                raise ApiError(HTTPStatus.BAD_REQUEST, "invalid vault_id or auth_hash")
            self.store.create_vault(vault_id, name, auth_hash, validate_wrapped(body.get("wrapped")))
            log.info("vault created")
            return HTTPStatus.CREATED, {"vault_id": vault_id}

        if parts == ["v1", "vaults", "lookup"] and method == "GET":
            self.require_server_token(h)
            self.limit(h)  # every lookup counts: it hands out a password-wrapped key
            name = (query.get("name") or [""])[0].strip().lower()
            found = self.store.lookup(name) if NAME_RE.match(name) else None
            if found is None:
                raise ApiError(HTTPStatus.NOT_FOUND, "no vault with that name")
            return HTTPStatus.OK, found

        if len(parts) >= 3 and parts[:2] == ["v1", "vaults"]:
            vault_id = parts[2]
            rest = parts[3:]
            moved_to = self.require_vault(h, vault_id)

            if rest == ["successor"] and method == "GET":
                found = self.store.successor(vault_id) if moved_to else None
                if found is None:
                    raise ApiError(HTTPStatus.NOT_FOUND, "this vault has not moved")
                return HTTPStatus.OK, found
            if moved_to:
                # Revoked: the data is gone. Point at the successor, whose
                # key needs the master password.
                raise ApiError(HTTPStatus.GONE, "vault moved: its key was changed to revoke a device", moved=True)

            device, device_name = self.device_header(h)
            if device:
                self.store.touch_device(vault_id, device, device_name)

            if rest == ["successor"] and method == "POST":
                body = h.json_body()
                new_id = str(body.get("vault_id", ""))
                auth_hash = str(body.get("auth_hash", ""))
                if not VAULT_ID_RE.match(new_id) or not HASH_RE.match(auth_hash) or new_id == vault_id:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "invalid vault_id or auth_hash")
                self.store.move_vault(vault_id, new_id, auth_hash, validate_wrapped(body.get("wrapped")))
                log.info("vault moved to a successor (device revoked)")
                return HTTPStatus.CREATED, {"vault_id": new_id}

            if rest == ["devices"] and method == "GET":
                return HTTPStatus.OK, {"devices": self.store.devices(vault_id)}

            if len(rest) == 2 and rest[0] == "devices" and method == "DELETE":
                if not DEVICE_RE.match(rest[1]):
                    raise ApiError(HTTPStatus.NOT_FOUND, "no such device")
                if not self.store.forget_device(vault_id, rest[1]):
                    raise ApiError(HTTPStatus.NOT_FOUND, "no such device")
                log.info("device forgotten")
                return HTTPStatus.OK, {"ok": True}

            if rest == ["records"] and method == "POST":
                body = h.json_body()
                device_id = str(body.get("device_id", ""))
                if not DEVICE_RE.match(device_id):
                    raise ApiError(HTTPStatus.BAD_REQUEST, "invalid device_id")
                items = body.get("records")
                if not isinstance(items, list) or len(items) > MAX_RECORDS_PER_PUSH:
                    raise ApiError(HTTPStatus.BAD_REQUEST, f"records: list of at most {MAX_RECORDS_PER_PUSH}")
                parsed: list[tuple[str, bytes]] = []
                for it in items:
                    key = str(it.get("key", "")) if isinstance(it, dict) else ""
                    if not KEY_RE.match(key):
                        raise ApiError(HTTPStatus.BAD_REQUEST, "invalid record key")
                    try:
                        data = base64.b64decode(it.get("data", ""), validate=True)
                    except ValueError:
                        raise ApiError(HTTPStatus.BAD_REQUEST, "record data must be base64")
                    if not data or len(data) > MAX_RECORD:
                        raise ApiError(HTTPStatus.BAD_REQUEST, "record data size")
                    parsed.append((key, data))
                seq = self.store.push(vault_id, device_id, parsed) if parsed else self.store.head(vault_id)
                return HTTPStatus.OK, {"seq": seq, "stored": len(parsed)}

            if rest == ["records"] and method == "GET":
                try:
                    since = int((query.get("since") or ["0"])[0])
                    limit = min(int((query.get("limit") or [str(MAX_PAGE)])[0]), MAX_PAGE)
                except ValueError:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "since and limit must be integers")
                records, cursor, more = self.store.pull(vault_id, max(since, 0), max(limit, 1))
                return HTTPStatus.OK, {"records": records, "cursor": cursor, "more": more}

            if rest == ["wrapped"] and method == "PUT":
                self.store.set_wrapped(vault_id, validate_wrapped(h.json_body().get("wrapped")))
                return HTTPStatus.OK, {"ok": True}

            if rest == [] and method == "DELETE":
                self.store.delete_vault(vault_id)
                log.info("vault deleted")
                return HTTPStatus.OK, {"ok": True}

        raise ApiError(HTTPStatus.NOT_FOUND, "not found")


class Handler(BaseHTTPRequestHandler):
    app: App
    server_version = "MyTermSync/1"
    sys_version = ""
    protocol_version = "HTTP/1.1"

    _raw: bytes = b""

    def client_ip(self) -> str:
        if self.app.trust_proxy:
            fwd = self.headers.get("X-Forwarded-For", "")
            if fwd:
                return fwd.split(",")[0].strip()
        return self.client_address[0]

    def _read_body(self) -> None:
        """Reads the whole body up front, so a request rejected early (bad
        auth, validation) never leaves unread bytes on a kept-alive connection."""
        length = int(self.headers.get("Content-Length") or 0)
        if length > MAX_BODY:
            self.close_connection = True
            raise ApiError(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, "body too large")
        self._raw = self.rfile.read(length) if length else b""

    def json_body(self) -> dict:
        try:
            body = json.loads(self._raw or b"{}")
        except json.JSONDecodeError:
            raise ApiError(HTTPStatus.BAD_REQUEST, "body must be JSON")
        if not isinstance(body, dict):
            raise ApiError(HTTPStatus.BAD_REQUEST, "body must be a JSON object")
        return body

    def _dispatch(self, method: str) -> None:
        try:
            self._read_body()
            status, payload = self.app.handle(self, method)
        except ApiError as e:
            status, payload = e.status, {"error": e.message, **e.extra}
        except ValueError:  # bad Content-Length
            self.close_connection = True
            status, payload = HTTPStatus.BAD_REQUEST, {"error": "bad request"}
        except Exception:  # never leak internals
            log.exception("unhandled error")
            status, payload = HTTPStatus.INTERNAL_SERVER_ERROR, {"error": "internal error"}
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        if self.close_connection:
            self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:  # noqa: N802
        self._dispatch("GET")

    def do_POST(self) -> None:  # noqa: N802
        self._dispatch("POST")

    def do_PUT(self) -> None:  # noqa: N802
        self._dispatch("PUT")

    def do_DELETE(self) -> None:  # noqa: N802
        self._dispatch("DELETE")

    def log_message(self, fmt: str, *args) -> None:
        # Method, path without query string, and status only. No tokens, no bodies.
        if len(args) >= 2 and isinstance(args[0], str):
            line = args[0].split("?")[0]
            log.info("%s %s %s", self.client_ip(), line, args[1])


def make_server(app: App, host: str, port: int, tls_cert: str | None = None, tls_key: str | None = None):
    handler = type("BoundHandler", (Handler,), {"app": app})
    httpd = ThreadingHTTPServer((host, port), handler)
    httpd.daemon_threads = True
    if tls_cert and tls_key:
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.minimum_version = ssl.TLSVersion.TLSv1_2
        ctx.load_cert_chain(tls_cert, tls_key)
        httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
    return httpd


def main() -> None:
    p = argparse.ArgumentParser(description="MyTerm sync server")
    p.add_argument("--host", default=os.environ.get("MYTERM_SYNC_HOST", "0.0.0.0"))
    p.add_argument("--port", type=int, default=int(os.environ.get("MYTERM_SYNC_PORT", "7341")))
    p.add_argument("--db", default=os.environ.get("MYTERM_SYNC_DB", "/data/sync.db"))
    p.add_argument("--tls-cert", default=os.environ.get("MYTERM_SYNC_TLS_CERT"))
    p.add_argument("--tls-key", default=os.environ.get("MYTERM_SYNC_TLS_KEY"))
    args = p.parse_args()

    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    token = os.environ.get("MYTERM_SYNC_TOKEN", "")
    if len(token) < 24:
        raise SystemExit("Set MYTERM_SYNC_TOKEN to a random string of at least 24 characters "
                         "(e.g. `openssl rand -hex 24`).")
    trust_proxy = os.environ.get("MYTERM_SYNC_TRUST_PROXY", "").lower() in ("1", "true", "yes")
    app = App(Store(args.db), token, trust_proxy=trust_proxy)
    httpd = make_server(app, args.host, args.port, args.tls_cert, args.tls_key)
    scheme = "https" if args.tls_cert else "http"
    log.info("MyTerm sync listening on %s://%s:%d (db %s)", scheme, args.host, args.port, args.db)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
