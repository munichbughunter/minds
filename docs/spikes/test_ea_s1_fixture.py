"""Transport und Aufräumen der Wegwerf-Fixture, keine Claude-Isolationstests."""

import contextlib
import json
import select
import socket
import subprocess
import sys
import tempfile
import unittest
import urllib.error
import urllib.request
from pathlib import Path

FIXTURE = Path(__file__).with_name("ea_s1_fixture.py")


@contextlib.contextmanager
def process(root, port=0):
    child = subprocess.Popen(
        [sys.executable, str(FIXTURE), str(root), "--port", str(port)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        yield child
    finally:
        if child.poll() is None:
            child.terminate()
        try:
            child.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.communicate(timeout=5)


class FixtureTests(unittest.TestCase):
    def test_round_trips_capture_and_sigterm_cleanup(self):
        with tempfile.TemporaryDirectory(prefix="ea-s1-", dir="/tmp") as parent:
            root = Path(parent) / "fixture"
            with process(root) as child:
                self.assertTrue(select.select([child.stdout], [], [], 10)[0])
                line = child.stdout.readline()
                self.assertTrue(line, "fixture did not become ready")
                ready = json.loads(line)
                self.assertTrue(ready["ready"])
                state = root / "witness"
                self.assertEqual(state.stat().st_mode & 0o777, 0o700)
                self.assertEqual((state / "probe.txt").stat().st_mode & 0o777, 0o600)
                with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
                    client.settimeout(2)
                    client.connect(str(state / "run/witness.sock"))
                    client.sendall(b"ping\n")
                    self.assertEqual(client.recv(64), b"pong\n")

                # Lokale Verbindung unabhängig von Proxy-Umgebungsvariablen.
                opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                url = f"http://127.0.0.1:{ready['http_port']}/hook"
                for event in (
                    "SessionStart",
                    "UserPromptSubmit",
                    "PostToolUse",
                    "Stop",
                ):
                    payload = json.dumps(
                        {"hook_event_name": event, "session_id": "synthetic"}
                    ).encode()
                    request = urllib.request.Request(
                        url, data=payload, headers={"Content-Type": "application/json"}
                    )
                    with opener.open(request, timeout=2) as response:
                        self.assertEqual(response.read(), b"{}")
                captured = list((state / "http").glob("*.json"))
                self.assertEqual(len(captured), 4)
                self.assertEqual(
                    {json.loads(p.read_bytes())["hook_event_name"] for p in captured},
                    {"SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"},
                )
                with self.assertRaises(urllib.error.HTTPError) as error:
                    opener.open(urllib.request.Request(url, data=b"[]"), timeout=2)
                self.assertEqual(error.exception.code, 400)
                error.exception.close()
                self.assertEqual(len(list((state / "http").glob("*.json"))), 4)
            self.assertFalse(root.exists(), "SIGTERM must remove the entire fixture")

    def test_existing_directory_is_never_removed(self):
        with tempfile.TemporaryDirectory(prefix="ea-s1-", dir="/tmp") as parent:
            root = Path(parent)
            sentinel = root / "existing.txt"
            sentinel.write_text("keep")
            with process(root) as child:
                child.communicate(timeout=5)
                self.assertNotEqual(child.returncode, 0)
            self.assertEqual(sentinel.read_text(), "keep")

    def test_bind_failure_cleans_up_partial_fixture(self):
        with tempfile.TemporaryDirectory(prefix="ea-s1-", dir="/tmp") as parent:
            root = Path(parent) / "fixture"
            with socket.socket() as occupied:
                occupied.bind(("127.0.0.1", 0))
                occupied.listen()
                with process(root, occupied.getsockname()[1]) as child:
                    child.communicate(timeout=5)
                    self.assertNotEqual(child.returncode, 0)
            self.assertFalse(root.exists())


if __name__ == "__main__":
    unittest.main()
