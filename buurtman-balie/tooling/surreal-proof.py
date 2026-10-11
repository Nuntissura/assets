"""Disposable SurrealKV proof; only owns its own child process and private test data.

Requires an explicitly supplied SurrealDB executable. Never connects to a host DB.
--query is the test-only Rust transport; production hosts supply their own SDK port.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
MAX_WIRE = 4 * 1024 * 1024


def http(path, body, authorized=True):
    headers = {"Accept": "application/json", "Content-Type": "application/json",
               "Surreal-NS": "main", "Surreal-DB": "main"}
    if authorized:
        credential = "proof:" + os.environ["BB_SURREAL_PASSWORD"]
        headers["Authorization"] = "Basic " + base64.b64encode(credential.encode()).decode()
    if len(body) > MAX_WIRE:
        raise ValueError("encoded request exceeds wire budget")
    req = urllib.request.Request(os.environ["BB_SURREAL_ENDPOINT"] + path, body, headers)
    with urllib.request.urlopen(req, timeout=8) as response:
        data = response.read(MAX_WIRE + 1)
    if len(data) > MAX_WIRE:
        raise ValueError("response exceeds wire budget")
    return json.loads(data)


def values(statements):
    errors = [str(row.get("result", "")) for row in statements if row.get("status") != "OK"]
    if errors:
        if any("BB_STALE_REVISION" in e or "read or write conflict" in e.lower()
               or "transaction conflict" in e.lower() for e in errors):
            return {"error": "conflict"}
        # Class and first diagnostic are returned only to local test code; never payloads or credentials.
        return {"error": "storage", "diagnostic": errors[0][:300]}
    return {"values": [row["result"] for row in statements]}


def query():
    try:
        incoming = json.load(sys.stdin)
        body = json.dumps({"id": 1, "method": "query",
                           "params": [incoming["sql"], {"params": incoming["params"]}]}).encode()
        result = http("/rpc", body)
        if "error" in result:
            answer = {"error": "storage", "diagnostic": "RPC query rejected"}
        else:
            answer = values(result["result"])
    except (OSError, ValueError, KeyError) as error:
        answer = {"error": "storage", "diagnostic": type(error).__name__}
    print(json.dumps(answer))


class Server:
    def __init__(self, executable, directory):
        self.executable, self.directory, self.child = executable, directory, None

    def start(self):
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        os.environ["BB_SURREAL_ENDPOINT"] = f"http://127.0.0.1:{port}"
        env = os.environ.copy()
        # Inherited host Surreal configuration must not redirect this disposable process.
        env = {k: v for k, v in env.items() if not k.startswith("SURREAL_")}
        env.update(SURREAL_USER="proof", SURREAL_PASS=os.environ["BB_SURREAL_PASSWORD"])
        log = (self.directory / "server.log").open("ab")
        flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
        self.child = subprocess.Popen([str(self.executable), "start", "--no-banner", "--log", "warn",
                                      "--bind", f"127.0.0.1:{port}", "--query-timeout", "5s",
                                      "--transaction-timeout", "5s", "surrealkv:proof-db?sync=every"],
                                     cwd=self.directory, env=env, stdout=log, stderr=log,
                                     creationflags=flags)
        log.close()
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self.child.poll() is not None:
                raise RuntimeError(f"disposable server exited; inspect {self.directory / 'server.log'}")
            try:
                result = http("/sql", b"RETURN true;")
                if values(result).get("values") == [True]:
                    return
            except (OSError, ValueError):
                pass
            time.sleep(0.2)
        raise RuntimeError("disposable server authentication/startup timed out")

    def stop(self):
        if self.child is not None and self.child.poll() is None:
            # Intentional abrupt exit proves process-crash recovery; never targets unrelated PIDs.
            self.child.kill()
            self.child.wait(timeout=10)
        self.child = None


def canonical():
    result = values(http("/sql", b"SELECT layout_version,revision,payload FROM bb_workspaces:business;"))
    assert "values" in result, result
    row = result["values"][0][0]
    snapshot = json.loads(row["payload"])
    assert row["layout_version"] == 1 and row["revision"] == str(snapshot["revision"])
    assert snapshot["schema_version"] == 2 and snapshot["revision"] == 6
    assert snapshot["records"]["client"]["name"] == "Committed without response"
    assert len(snapshot["receipts"]) == 6 and "uncertain" in snapshot["receipts"]
    receipt = snapshot["receipts"]["uncertain"]
    assert receipt["actor"] == "staff" and receipt["request"]["operation_id"] == "uncertain"
    assert receipt["outcome"] == {"workspace_revision": 6, "record_revision": 2}
    assert len(snapshot["fields"]) == 1 and len(snapshot["pricing_tiers"]) == 1
    assert len(snapshot["relations"]) == 1
    return snapshot


def run_test(name):
    subprocess.run(["cargo", "test", "--locked", "--offline", "--features", "surreal",
                    "--test", "surreal", name, "--", "--ignored", "--exact", "--nocapture"],
                   cwd=ROOT, check=True)


def proof(args):
    executable = Path(args.executable).resolve(strict=True)
    version = subprocess.check_output([str(executable), "version"], text=True).strip()
    digest = hashlib.sha256(executable.read_bytes()).hexdigest()
    directory = ROOT / ".local" / ("surreal-proof-" + secrets.token_hex(8))
    directory.mkdir(parents=True)
    race = directory / "race"
    race.mkdir()
    for round_name in ("create", "update"):
        (race / round_name).mkdir()
    os.environ["BB_SURREAL_RACE_DIR"] = str(race)
    os.environ["BB_SURREAL_PASSWORD"] = secrets.token_urlsafe(32)
    os.environ["BB_TEST_PYTHON"] = sys.executable
    server = Server(executable, directory)
    try:
        server.start()
        provision = values(http("/sql", b"DEFINE TABLE IF NOT EXISTS bb_workspaces SCHEMALESS PERMISSIONS NONE;"))
        assert "values" in provision, provision
        run_test("live_seed_boundary")
        race_rows = values(http("/sql", b"SELECT revision,payload FROM bb_workspaces:race;"))["values"][0]
        assert len(race_rows) == 1 and race_rows[0]["revision"] == "2"
        raced = json.loads(race_rows[0]["payload"])
        assert raced["revision"] == 2 and len(raced["fields"]) == len(raced["receipts"]) == 2
        assert set(raced["fields"]) == set(raced["receipts"])
        for key, receipt in raced["receipts"].items():
            assert receipt["request"]["command"]["DefineField"] == raced["fields"][key]
            revision = 1 if key.startswith("create-") else 2
            assert receipt["actor"] == "staff" and receipt["request"]["operation_id"] == key
            assert receipt["outcome"] == {"workspace_revision": revision, "record_revision": None}
        run_test("live_invalid_boundary")
        invalid = values(http("/sql", b"SELECT * FROM bb_workspaces:invalid;"))
        assert invalid["values"] == [[]], invalid
        wide = values(http("/sql", b"SELECT revision,payload FROM bb_workspaces:wide;"))["values"][0][0]
        assert wide["revision"] == str(2**64 - 1)
        assert json.loads(wide["payload"])["revision"] == 2**64 - 1
        # RETURN ends execution early on this server; place THROW before RETURN.
        aborted = values(http("/sql", b"BEGIN; UPSERT bb_workspaces:aborted CONTENT {marker: true}; "
                              b"THROW 'proof-abort'; RETURN true; COMMIT;"))
        assert aborted.get("error") == "storage", aborted
        assert values(http("/sql", b"SELECT * FROM bb_workspaces:aborted;"))["values"] == [[]]
        assert values([{"status": "OK", "result": True},
                       {"status": "ERR", "result": "commit failed"}]).get("error") == "storage"
        # Reject the encoded wire size before touching the network, even if snapshots individually fit.
        try:
            http("/rpc", b"x" * (MAX_WIRE + 1))
            raise AssertionError("oversized encoded request accepted")
        except ValueError as error:
            assert str(error) == "encoded request exceeds wire budget"
        before = canonical()
        # Authentication is exercised against the actual server, separately from CRM actor grants.
        try:
            result = http("/sql", b"SELECT * FROM bb_workspaces;", authorized=False)
            assert "error" in values(result), "unauthenticated query unexpectedly succeeded"
        except urllib.error.HTTPError as error:
            assert error.code in (401, 403), f"unexpected auth HTTP status {error.code}"
        server.stop()
        server.start()
        assert canonical() == before
        run_test("live_restart_boundary")
        server.stop()
        restored = directory / "restored"
        restored.mkdir()
        shutil.copytree(directory / "proof-db", restored / "proof-db")
        server = Server(executable, restored)
        server.start()
        assert canonical() == before
        result = {"result": "passed", "server_version": version, "sha256": digest,
                          "database": str(directory), "proof": ["independent canonical reread",
                          "disk server crash/restart", "separate closed-database restore",
                          "two-process create/update CAS with atomic receipts", "stale rejection",
                          "committed Storage response-loss replay", "auth denial",
                          "malformed/layout/schema/revision/capacity rejection", "u64 revision boundary",
                          "late transaction failure rollback", "fields/pricing/links preservation"]}
        (directory / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(result))
    finally:
        server.stop()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--query", action="store_true")
    parser.add_argument("--executable")
    arguments = parser.parse_args()
    if arguments.query:
        query()
    elif arguments.executable:
        proof(arguments)
    else:
        parser.error("--executable is required for disposable database proof")
