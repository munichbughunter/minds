"""Wegwerf-Fixture für EA-S1; kein Witness und keine Isolationseinstufung.

Start mit `uv run --no-project docs/spikes/ea_s1_fixture.py /tmp/minds-ea-s1`.
Nur synthetische Hooks verwenden: empfangene Payloads werden unverändert gespeichert.
Das Ziel muss neu sein. Ctrl-C, SIGTERM und Startfehler räumen die Fixture auf.
Benötigte Messdaten vor dem Beenden außerhalb der Fixture sichern.
"""

import argparse
import contextlib
import json
import os
import shutil
import signal
import socketserver
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


@contextlib.contextmanager
def scratch(root):
    """Nur das selbst angelegte Verzeichnis aufräumen, auch bei Startfehlern."""
    root.mkdir(mode=0o700)
    try:
        yield
    finally:
        shutil.rmtree(root)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--port", type=int, default=18765)
    args = parser.parse_args()
    root = args.root.resolve()
    signal.signal(signal.SIGTERM, stop)
    with scratch(root):
        serve(root, args.port)


def stop(_signum, _frame):
    raise SystemExit(0)


def serve(root, port):
    os.umask(0o077)
    state = root / "witness"
    (state / "run").mkdir(parents=True)
    (state / "http").mkdir()
    (state / "command").mkdir()
    (root / "worktree").mkdir()
    (state / "probe.txt").write_text("ea-s1-canary\n")
    (root / "fixture.pid").write_text(str(os.getpid()) + "\n")

    class Ping(socketserver.BaseRequestHandler):
        def handle(self):
            self.request.settimeout(2)
            if self.request.recv(64).strip() == b"ping":
                self.request.sendall(b"pong\n")

    class Capture(BaseHTTPRequestHandler):
        def do_POST(self):
            self.connection.settimeout(2)
            if self.path != "/hook":
                self.send_error(404)
                return
            try:
                size = int(self.headers.get("Content-Length", "0"))
                if not 0 < size <= 1024 * 1024:
                    self.send_error(413)
                    return
                raw = self.rfile.read(size)
                payload = json.loads(raw)
                if not isinstance(payload, dict) or not isinstance(
                    payload.get("hook_event_name"), str
                ):
                    self.send_error(400)
                    return
            except (ValueError, OSError):
                self.send_error(400)
                return
            (state / "http" / f"{uuid.uuid4()}.json").write_bytes(raw)
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", "2")
            self.end_headers()
            self.wfile.write(b"{}")

        def log_message(self, *_args):
            # Keine Payloads oder fremden Request-Zeilen ins Terminal schreiben.
            pass

    with (
        socketserver.ThreadingUnixStreamServer(
            str(state / "run/witness.sock"), Ping
        ) as unix,
        ThreadingHTTPServer(("127.0.0.1", port), Capture) as http,
    ):
        unix.daemon_threads = True
        threading.Thread(target=unix.serve_forever, daemon=True).start()
        print(
            json.dumps(
                {
                    "root": str(root),
                    "pid": os.getpid(),
                    "uid": os.getuid(),
                    "http_port": http.server_port,
                    "ready": True,
                }
            ),
            flush=True,
        )
        try:
            http.serve_forever()
        except KeyboardInterrupt:
            pass
        finally:
            unix.shutdown()


if __name__ == "__main__":
    main()
