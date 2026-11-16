#!/usr/bin/env python3
"""End-to-end test of the myterm command line, run in CI.

A sync server with SSH certificates on, two devices (A creates the vault and
adds a host, B joins with the "certs" scope), the local sshd made to trust
MyTerm certificates, then B connects interactively (host key prompt, shell),
runs a command, and uses `myterm run`.

    python3 crates/cli/tests/e2e.py target/release/myterm
Needs: pexpect, openssh-server running on 127.0.0.1:22, passwordless sudo.
"""

import getpass
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.request

import pexpect

BIN = os.path.abspath(sys.argv[1])
ROOT = tempfile.mkdtemp(prefix="myterm-e2e-")
USER = getpass.getuser()
TOKEN = "e2e-server-token-0123456789abcdef"
PASSWORD = "correct horse battery staple"
URL = "http://127.0.0.1:7341"
A = os.path.join(ROOT, "device-a")
B = os.path.join(ROOT, "device-b")


def env(home):
    e = dict(os.environ, MYTERM_HOME=home, TERM="xterm-256color")
    e.pop("XDG_RUNTIME_DIR", None)
    return e


def interact(home, args, steps, timeout=90):
    """Runs myterm in a pseudo-terminal: steps = [(expect, send), ...]."""
    print(f"\n$ myterm {' '.join(args)}", flush=True)
    child = pexpect.spawn(BIN, args, env=env(home), encoding="utf-8", timeout=timeout, dimensions=(30, 120))
    child.logfile_read = sys.stdout
    for want, send in steps:
        child.expect(want)
        if send is not None:
            child.send(send)
    child.expect(pexpect.EOF)
    child.close()
    assert child.exitstatus == 0, f"myterm {args} exited {child.exitstatus}"
    return child.before


def run(home, args):
    print(f"\n$ myterm {' '.join(args)}", flush=True)
    r = subprocess.run([BIN, *args], env=env(home), capture_output=True, text=True, timeout=120, stdin=subprocess.DEVNULL)
    print(r.stdout, r.stderr, sep="", flush=True)
    assert r.returncode == 0, f"myterm {args} exited {r.returncode}"
    return r.stdout


def main():
    data = os.path.join(ROOT, "server")
    os.makedirs(data)
    server = subprocess.Popen(
        [sys.executable, "server/myterm_sync/server.py", "--db", os.path.join(data, "sync.db"), "--host", "127.0.0.1"],
        env=dict(os.environ, MYTERM_SYNC_TOKEN=TOKEN, MYTERM_SYNC_CERT_PRINCIPALS=USER, MYTERM_SYNC_CERT_HOURS="1"),
    )
    try:
        for _ in range(50):
            try:
                urllib.request.urlopen(URL + "/v1/health", timeout=1)
                break
            except OSError:
                time.sleep(0.2)

        # Device A starts the vault and adds a host from an OpenSSH config.
        interact(A, ["join", URL, "--name", "e2e", "--scope", "full", "--create"], [
            ("Server token", TOKEN + "\r"),
            ("Master password: ", PASSWORD + "\r"),
            ("Master password again: ", PASSWORD + "\r"),
            ("Joined vault e2e", None),
        ])
        cfg = os.path.join(ROOT, "ssh_config")
        with open(cfg, "w") as f:
            f.write(f"Host loc\n    HostName 127.0.0.1\n    User {USER}\n")
        run(A, ["import", cfg])
        run(A, ["set", "loc", "--certificate", "on", "--tags", "e2e"])
        run(A, ["sync"])

        # Device B joins with the default server scope and gets a certificate.
        out = interact(B, ["join", URL, "--name", "e2e", "--scope", "certs"], [
            ("Server token", TOKEN + "\r"),
            ("Master password: ", PASSWORD + "\r"),
            ("SSH certificate issued", None),
        ])
        hosts = json.loads(run(B, ["hosts", "--json"]))
        assert [h["label"] for h in hosts] == ["loc"] and hosts[0]["certificate"], hosts
        status = run(B, ["status"])
        assert "certs" in status and "Certificate: myterm:" in status, status

        # This runner's sshd trusts the sync server's CA.
        run(B, ["cert", "--trust"])

        # Interactive session: host key prompt, a shell, a command, exit.
        interact(B, ["ssh", "loc"], [
            ("Trust this host key\\?", "y\r"),
            ("myterm │ loc", "echo E2E-$((40+2))\r"),
            ("E2E-42", "exit\r"),
        ])

        # The host key is remembered: non-interactive use works now.
        out = run(B, ["ssh", "loc", "--", "echo", "run-ok"])
        assert "run-ok" in out, out
        out = run(B, ["run", "echo from-$(hostname)", "--tag", "e2e"])
        assert "── loc (exit 0) ──" in out, out

        # History recorded on B reaches A through the sync server.
        run(B, ["sync"])
        run(A, ["sync"])
        print("\nE2E OK", flush=True)
    finally:
        server.terminate()


if __name__ == "__main__":
    main()
