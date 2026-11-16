"""End-to-end tests: a real server on a random port, real HTTP requests."""

import base64
import hashlib
import http.client
import json
import os
import sys
import threading
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import shutil  # noqa: E402
import subprocess  # noqa: E402
import tempfile  # noqa: E402

from myterm_sync.server import App, CertificateAuthority, RateLimiter, Store, make_server  # noqa: E402

SERVER_TOKEN = "s" * 32
VAULT_TOKEN = "ab" * 32
VAULT_ID = "0123456789abcdef0123456789abcdef"
WRAPPED = {
    "password_wrapped": base64.b64encode(b"x" * 72).decode(),
    "kdf_salt": base64.b64encode(b"\x01" * 16).decode(),
    "kdf_m_kib": 65536,
    "kdf_t": 3,
    "kdf_p": 1,
}


def key(n: int) -> str:
    return hashlib.sha256(str(n).encode()).hexdigest()


def b64(b: bytes) -> str:
    return base64.b64encode(b).decode()


class ServerTest(unittest.TestCase):
    def setUp(self):
        self.app = App(Store(":memory:"), SERVER_TOKEN, RateLimiter(limit=1000))
        self.httpd = make_server(self.app, "127.0.0.1", 0)
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.httpd.shutdown()
        self.httpd.server_close()

    def req(self, method, path, body=None, server=False, vault=None, device=None, name=None):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        headers = {"Content-Type": "application/json"}
        if device:
            headers["X-MyTerm-Device"] = device
        if name:
            headers["X-MyTerm-Device-Name"] = name
        if server:
            headers["Authorization"] = f"Bearer {SERVER_TOKEN}"
        if vault is not None:
            headers["X-MyTerm-Vault-Token"] = vault
        c.request(method, path, json.dumps(body) if body is not None else None, headers)
        r = c.getresponse()
        data = json.loads(r.read() or b"{}")
        c.close()
        return r.status, data

    def create(self, name="home"):
        return self.req(
            "POST",
            "/v1/vaults",
            {"name": name, "vault_id": VAULT_ID, "auth_hash": hashlib.sha256(VAULT_TOKEN.encode()).hexdigest(), "wrapped": WRAPPED},
            server=True,
        )

    def push(self, device, items):
        return self.req(
            "POST",
            f"/v1/vaults/{VAULT_ID}/records",
            {"device_id": device, "records": [{"key": k, "data": b64(d)} for k, d in items]},
            vault=VAULT_TOKEN,
        )

    def pull(self, since=0, limit=500):
        return self.req("GET", f"/v1/vaults/{VAULT_ID}/records?since={since}&limit={limit}", vault=VAULT_TOKEN)

    # ---------------------------------------------------------------- tests

    def test_health(self):
        self.assertEqual(self.req("GET", "/v1/health"), (200, {"ok": True, "protocol": 1}))

    def test_create_requires_server_token_and_rejects_duplicates(self):
        status, _ = self.req("POST", "/v1/vaults", {"name": "x"}, server=False)
        self.assertEqual(status, 401)
        self.assertEqual(self.create()[0], 201)
        self.assertEqual(self.create()[0], 409)

    def test_create_validates_input(self):
        bad = {"name": "A!", "vault_id": VAULT_ID, "auth_hash": "0" * 64, "wrapped": WRAPPED}
        self.assertEqual(self.req("POST", "/v1/vaults", bad, server=True)[0], 400)
        weak = dict(WRAPPED, kdf_m_kib=1)
        bad = {"name": "ok-name", "vault_id": VAULT_ID, "auth_hash": "0" * 64, "wrapped": weak}
        self.assertEqual(self.req("POST", "/v1/vaults", bad, server=True)[0], 400)

    def test_lookup_by_name(self):
        self.create()
        status, body = self.req("GET", "/v1/vaults/lookup?name=HOME", server=True)
        self.assertEqual(status, 200)
        self.assertEqual(body, {"vault_id": VAULT_ID, "wrapped": WRAPPED})
        self.assertEqual(self.req("GET", "/v1/vaults/lookup?name=nobody", server=True)[0], 404)
        self.assertEqual(self.req("GET", "/v1/vaults/lookup?name=home")[0], 401, "needs server token")

    def test_vault_token_required(self):
        self.create()
        self.assertEqual(self.req("GET", f"/v1/vaults/{VAULT_ID}/records", vault="wrong")[0], 401)
        self.assertEqual(self.req("GET", f"/v1/vaults/{VAULT_ID}/records")[0], 401)
        self.assertEqual(self.req("GET", f"/v1/vaults/{'f' * 32}/records", vault=VAULT_TOKEN)[0], 401)
        self.assertEqual(self.req("GET", "/v1/vaults/not-hex/records", vault=VAULT_TOKEN)[0], 404)

    def test_push_pull_keeps_each_devices_copy(self):
        self.create()
        self.assertEqual(self.push("phone", [(key(1), b"phone-v1"), (key(2), b"phone-other")])[1]["seq"], 2)
        self.assertEqual(self.push("desk", [(key(1), b"desk-v1")])[1]["seq"], 3)
        # Same device + key replaces its own copy only.
        self.assertEqual(self.push("phone", [(key(1), b"phone-v2")])[1]["seq"], 4)

        status, body = self.pull()
        self.assertEqual(status, 200)
        got = {(r["device_id"], r["key"]): base64.b64decode(r["data"]) for r in body["records"]}
        self.assertEqual(
            got,
            {("phone", key(1)): b"phone-v2", ("phone", key(2)): b"phone-other", ("desk", key(1)): b"desk-v1"},
        )
        self.assertEqual(body["cursor"], 4)
        self.assertFalse(body["more"])

        _, body = self.pull(since=3)
        self.assertEqual([r["seq"] for r in body["records"]], [4])
        _, body = self.pull(since=4)
        self.assertEqual(body, {"records": [], "cursor": 4, "more": False})

    def test_paging(self):
        self.create()
        self.push("phone", [(key(i), b"x") for i in range(7)])
        seen, since = [], 0
        while True:
            _, body = self.pull(since=since, limit=3)
            seen += [r["seq"] for r in body["records"]]
            since = body["cursor"]
            if not body["more"]:
                break
        self.assertEqual(seen, list(range(1, 8)))

    def test_push_validation(self):
        self.create()
        self.assertEqual(self.push("bad device!", [(key(1), b"x")])[0], 400)
        self.assertEqual(self.push("phone", [("short", b"x")])[0], 400)
        self.assertEqual(self.push("phone", [(key(1), b"")])[0], 400)
        status, body = self.push("phone", [])
        self.assertEqual((status, body["stored"]), (200, 0))

    def test_update_wrapped_and_delete(self):
        self.create()
        new = dict(WRAPPED, kdf_t=4)
        self.assertEqual(self.req("PUT", f"/v1/vaults/{VAULT_ID}/wrapped", {"wrapped": new}, vault=VAULT_TOKEN)[0], 200)
        self.assertEqual(self.req("GET", "/v1/vaults/lookup?name=home", server=True)[1]["wrapped"]["kdf_t"], 4)
        self.push("phone", [(key(1), b"x")])
        self.assertEqual(self.req("DELETE", f"/v1/vaults/{VAULT_ID}", vault=VAULT_TOKEN)[0], 200)
        self.assertEqual(self.req("GET", "/v1/vaults/lookup?name=home", server=True)[0], 404)
        self.assertEqual(self.create()[0], 201, "name free again")
        self.assertEqual(self.pull()[1]["records"], [], "records went with the vault")

    def test_rate_limit_on_failed_auth(self):
        self.httpd.RequestHandlerClass.app.limiter = RateLimiter(limit=3)
        self.create()
        codes = [self.req("GET", f"/v1/vaults/{VAULT_ID}/records", vault="wrong")[0] for _ in range(5)]
        self.assertEqual(codes, [401, 401, 401, 429, 429])

    def test_garbage_is_rejected_not_crashing(self):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        c.request("POST", "/v1/vaults", b"{not json", {"Authorization": f"Bearer {SERVER_TOKEN}"})
        r = c.getresponse()
        self.assertEqual(r.status, 400)
        c.close()
        self.assertEqual(self.req("GET", "/nope")[0], 404)

    def test_rate_limit_per_forwarded_client_when_trusting_proxy(self):
        app = self.httpd.RequestHandlerClass.app
        app.limiter = RateLimiter(limit=2)
        app.trust_proxy = True
        self.create()

        def bad(ip):
            c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
            c.request("GET", f"/v1/vaults/{VAULT_ID}/records", headers={"X-MyTerm-Vault-Token": "x", "X-Forwarded-For": ip})
            r = c.getresponse()
            r.read()
            c.close()
            return r.status

        self.assertEqual([bad("10.0.0.1") for _ in range(3)], [401, 401, 429])
        self.assertEqual(bad("10.0.0.2"), 401, "a different client has its own budget")

    def test_device_list_and_forget(self):
        self.create()
        v = f"/v1/vaults/{VAULT_ID}"
        self.req("POST", f"{v}/records", {"device_id": "phone-1", "records": [{"key": key(1), "data": b64(b"a")}]},
                 vault=VAULT_TOKEN, device="phone-1", name="Pixel 8")
        self.req("GET", f"{v}/records?since=0", vault=VAULT_TOKEN, device="pc-1", name="DESKTOP\x07-PC")
        status, body = self.req("GET", f"{v}/devices", vault=VAULT_TOKEN)
        self.assertEqual(status, 200)
        names = {d["device_id"]: (d["name"], d["records"]) for d in body["devices"]}
        self.assertEqual(names, {"phone-1": ("Pixel 8", 1), "pc-1": ("DESKTOP-PC", 0)})
        # A later request without a name keeps the stored one.
        self.req("GET", f"{v}/records?since=0", vault=VAULT_TOKEN, device="phone-1")
        _, body = self.req("GET", f"{v}/devices", vault=VAULT_TOKEN)
        self.assertIn(("phone-1", "Pixel 8"), [(d["device_id"], d["name"]) for d in body["devices"]])

        self.assertEqual(self.req("DELETE", f"{v}/devices/phone-1", vault=VAULT_TOKEN)[0], 200)
        _, body = self.req("GET", f"{v}/devices", vault=VAULT_TOKEN)
        self.assertEqual([d["device_id"] for d in body["devices"]], ["pc-1"])
        self.assertEqual(self.pull()[1]["records"], [], "the forgotten device's records are gone")
        self.assertEqual(self.req("DELETE", f"{v}/devices/phone-1", vault=VAULT_TOKEN)[0], 404)
        self.assertEqual(self.req("GET", f"{v}/devices")[0], 401)

    def test_revoke_moves_vault_to_successor(self):
        self.create()
        self.push("phone-1", [(key(1), b"old")])
        new_token = "cd" * 32
        new_id = "f" * 32
        succ = {"vault_id": new_id, "auth_hash": hashlib.sha256(new_token.encode()).hexdigest(), "wrapped": WRAPPED}
        self.assertEqual(self.req("POST", f"/v1/vaults/{VAULT_ID}/successor", succ)[0], 401, "needs the vault token")
        self.assertEqual(self.req("GET", f"/v1/vaults/{VAULT_ID}/successor", vault=VAULT_TOKEN)[0], 404)
        self.assertEqual(self.req("POST", f"/v1/vaults/{VAULT_ID}/successor", succ, vault=VAULT_TOKEN)[0], 201)

        # The old vault: data gone, everything answers 410 except the pointer.
        status, body = self.pull()
        self.assertEqual((status, body.get("moved")), (410, True))
        self.assertEqual(self.push("phone-1", [(key(2), b"x")])[0], 410)
        status, body = self.req("GET", f"/v1/vaults/{VAULT_ID}/successor", vault=VAULT_TOKEN)
        self.assertEqual((status, body["vault_id"], body["name"], body["wrapped"]), (200, new_id, "home", WRAPPED))
        self.assertEqual(self.req("GET", f"/v1/vaults/{VAULT_ID}/successor", vault="00" * 32)[0], 401)

        # The name now finds the successor, which works with its own token only.
        status, body = self.req("GET", "/v1/vaults/lookup?name=home", server=True)
        self.assertEqual((status, body["vault_id"]), (200, new_id))
        self.assertEqual(self.req("GET", f"/v1/vaults/{new_id}/records?since=0", vault=VAULT_TOKEN)[0], 401)
        status, body = self.req("GET", f"/v1/vaults/{new_id}/records?since=0", vault=new_token)
        self.assertEqual((status, body["records"]), (200, []))

        # Moving again (another revocation) keeps the first old vault pointing at the newest.
        newer = {"vault_id": "e" * 32, "auth_hash": hashlib.sha256(("ef" * 32).encode()).hexdigest(), "wrapped": WRAPPED}
        self.assertEqual(self.req("POST", f"/v1/vaults/{new_id}/successor", newer, vault=new_token)[0], 201)
        _, body = self.req("GET", f"/v1/vaults/{VAULT_ID}/successor", vault=VAULT_TOKEN)
        self.assertEqual(body["vault_id"], "e" * 32)

    def test_old_database_gains_moved_to_column(self):
        import sqlite3
        import tempfile

        path = os.path.join(tempfile.mkdtemp(), "old.db")
        db = sqlite3.connect(path)
        db.executescript(
            "CREATE TABLE vaults (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, auth_hash TEXT NOT NULL,"
            " wrapped TEXT NOT NULL, seq INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL);"
        )
        db.close()
        store = Store(path)
        cols = [r[1] for r in store.db.execute("PRAGMA table_info(vaults)")]
        self.assertIn("moved_to", cols)

    def test_certificates_off_by_default(self):
        self.assertEqual(self.req("GET", "/v1/ssh-ca.pub")[0], 404)
        self.create()
        status, body = self.req("POST", f"/v1/vaults/{VAULT_ID}/ssh-cert", {"public_key": "ssh-ed25519 AAAA"}, vault=VAULT_TOKEN)
        self.assertEqual((status, "not enabled" in body["error"]), (404, True))

    @unittest.skipUnless(shutil.which("ssh-keygen"), "needs ssh-keygen")
    def test_certificates_signed_for_vault_devices(self):
        d = tempfile.mkdtemp()
        self.app.ca = CertificateAuthority(os.path.join(d, "ca"), ["alice", "root"], 8)
        # The CA's public key, as plain text for TrustedUserCAKeys.
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        c.request("GET", "/v1/ssh-ca.pub")
        r = c.getresponse()
        ca_pub = r.read().decode()
        c.close()
        self.assertEqual(r.status, 200)
        self.assertTrue(ca_pub.startswith("ssh-ed25519 ") and "myterm-user-ca" in ca_pub)

        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", os.path.join(d, "user")], check=True)
        with open(os.path.join(d, "user.pub")) as f:
            user_pub = f.read().strip()
        self.create()
        path = f"/v1/vaults/{VAULT_ID}/ssh-cert"
        self.assertEqual(self.req("POST", path, {"public_key": user_pub})[0], 401, "needs the vault token")
        self.assertEqual(self.req("POST", path, {"public_key": "ssh-rsa AAAA"}, vault=VAULT_TOKEN)[0], 400)
        status, body = self.req("POST", path, {"public_key": user_pub}, vault=VAULT_TOKEN, device="pc-1", name="Office PC")
        self.assertEqual(status, 200)
        self.assertTrue(body["certificate"].startswith("ssh-ed25519-cert-v01@openssh.com "))
        with open(os.path.join(d, "user-cert.pub"), "w") as f:
            f.write(body["certificate"] + "\n")
        shown = subprocess.run(["ssh-keygen", "-L", "-f", os.path.join(d, "user-cert.pub")],
                               check=True, capture_output=True, text=True).stdout
        self.assertIn("myterm:01234567:Office_PC", shown)
        self.assertIn("alice", shown)
        self.assertIn("root", shown)

    def test_short_server_token_refused(self):
        with self.assertRaises(ValueError):
            App(Store(":memory:"), "short")


if __name__ == "__main__":
    unittest.main()
