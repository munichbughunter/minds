"""EA-S2: isolierter Docker-Versuch; keine Witness-Implementierung."""

import argparse
import contextlib
import json
import math
import os
import platform
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

TEMPLATES = Path(__file__).resolve().parents[2] / "crates/minds-cli/templates/witness"
LATENCY_WRITES = 200
BURST_WRITES = 1000
CONTAINER_ROOT = "/workspaces/worktree"


def run(args, *, env=None, timeout=30):
    return subprocess.run(
        args, env=env, text=True, capture_output=True, check=True, timeout=timeout
    ).stdout.strip()


def percentile(samples, fraction):
    """Nearest-rank-Perzentil; fehlende Messungen sind niemals null Millisekunden."""
    if not samples:
        return None
    return sorted(samples)[math.ceil(len(samples) * fraction) - 1]


class Events:
    """NUL-getrennte Watcher-Ausgabe sofort mit der monotonen Host-Uhr erfassen."""

    def __init__(self, process):
        self.process = process
        self.seen = {}
        self.condition = threading.Condition()
        self.thread = threading.Thread(target=self.read, daemon=True)
        self.thread.start()

    def read(self):
        pending = b""
        descriptor = self.process.stdout.fileno()
        while block := os.read(descriptor, 65536):
            observed = time.monotonic_ns()
            parts = (pending + block).split(b"\0")
            pending = parts.pop()
            with self.condition:
                for part in parts:
                    self.seen.setdefault(os.fsdecode(part), observed)
                self.condition.notify_all()

    def wait_for(self, paths, timeout):
        deadline = time.monotonic() + timeout
        with self.condition:
            while not all(str(path) in self.seen for path in paths):
                remaining = deadline - time.monotonic()
                if remaining <= 0 or self.process.poll() is not None:
                    break
                self.condition.wait(min(remaining, 0.1))
            return {str(p): self.seen[str(p)] for p in paths if str(p) in self.seen}


@contextlib.contextmanager
def child(args, **kwargs):
    process = subprocess.Popen(args, **kwargs)
    try:
        yield process
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        for stream in (process.stdin, process.stdout, process.stderr):
            if stream is not None:
                stream.close()


@contextlib.contextmanager
def socket_probe(path):
    """Nur ein Ping/Echo-Test, ohne Witness-Protokoll oder echte Schlüsseldaten."""
    stop = threading.Event()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
        listener.bind(str(path))
        path.chmod(0o600)
        listener.listen()
        listener.settimeout(0.1)

        def serve():
            while not stop.is_set():
                try:
                    connection, _ = listener.accept()
                except TimeoutError:
                    continue
                with connection:
                    connection.settimeout(2)
                    try:
                        request = b""
                        while b"\n" not in request and len(request) < 128:
                            block = connection.recv(128)
                            if not block:
                                break
                            request += block
                        if request == b"ea-s2-ping\n":
                            connection.sendall(b"ea-s2-pong\n")
                    except (TimeoutError, OSError):
                        pass

        thread = threading.Thread(target=serve, daemon=True)
        thread.start()
        try:
            yield
        finally:
            stop.set()
            thread.join(timeout=3)


def watcher_command(root, system):
    if system == "Linux":
        return [
            "inotifywait",
            "-m",
            "-r",
            "-q",
            "-e",
            "close_write",
            "--format",
            "%w%f%0",
            "--no-newline",
            str(root),
        ]
    return [
        "fswatch",
        "--monitor=fsevents_monitor",
        "--latency=0.01",
        "-0",
        "-r",
        str(root),
    ]


def measure(compose, env, root, system, report):
    command = watcher_command(root, system)
    report["watcher_command"] = command
    # Verzeichnisse existieren bereits beim rekursiven Watcher-Start.
    for name in ("latency", "burst", "ready"):
        (root / name).mkdir()
    writer_command = compose + [
        "exec",
        "-T",
        "agent",
        "sh",
        "-eu",
        "-c",
        'while IFS= read -r file; do printf "x\\n" > "$file"; done',
    ]
    with (
        tempfile.TemporaryFile() as errors,
        child(command, stdout=subprocess.PIPE, stderr=errors) as watcher,
        child(
            writer_command,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL,
            stderr=errors,
        ) as writer,
    ):
        events = Events(watcher)

        def write(relative):
            started = time.monotonic_ns()
            writer.stdin.write(f"{CONTAINER_ROOT}/{relative}\n".encode())
            writer.stdin.flush()
            return started

        # Ein tatsächlich beobachtetes Ereignis ersetzt eine feste Startup-Pause.
        ready = False
        for attempt in range(20):
            path = root / "ready" / str(attempt)
            write(f"ready/{attempt}")
            if events.wait_for([path], 0.5):
                ready = True
                break
        if not ready:
            raise RuntimeError("No host watcher event after readiness writes")

        samples, missing = [], []
        for index in range(LATENCY_WRITES):
            relative = f"latency/{index:04}"
            path = root / relative
            started = write(relative)
            observed = events.wait_for([path], 2)
            if str(path) in observed:
                samples.append((observed[str(path)] - started) / 1_000_000)
            else:
                missing.append(relative)
        report["latency"] = {
            "requested": LATENCY_WRITES,
            "observed": len(samples),
            "missing": missing,
            "samples_ms": samples,
            "p50_ms": percentile(samples, 0.50),
            "p95_ms": percentile(samples, 0.95),
            "max_ms": max(samples, default=None),
            "method": "host monotonic command-send to event-read upper bound",
        }
        # Ein Container-Shell-Prozess, keine 1000 Docker-Starts oder Wartezyklen.
        run(
            compose
            + [
                "exec",
                "-T",
                "agent",
                "sh",
                "-eu",
                "-c",
                (
                    'i=0; while [ "$i" -lt 1000 ]; do '
                    'printf "x\\n" > "burst/$i"; i=$((i + 1)); done'
                ),
            ],
            env=env,
        )
        expected = [root / "burst" / str(i) for i in range(BURST_WRITES)]
        observed = events.wait_for(expected, 10)
        report["burst"] = {
            "requested": BURST_WRITES,
            "observed": len(observed),
            "missing": [p.name for p in expected if str(p) not in observed],
            "host_files": sum(p.is_file() for p in expected),
            "drain_timeout_seconds": 10,
        }
        if watcher.poll() is not None or writer.poll() is not None:
            raise RuntimeError("Watcher or writer exited during measurement")


def experiment(base, system, report):
    root, state = base / "worktree", base / "witness"
    root.mkdir()
    state.mkdir(mode=0o700)
    (state / "run").mkdir(mode=0o700)
    # Synthetische Platzhalter: nie echte Journale, Salts oder Schlüssel öffnen.
    for name in (
        "witness.json",
        "journal/probe",
        "evidence/state/probe",
        "key/probe",
        "ledger",
    ):
        path = state / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("EA-S2 synthetic canary\n")
    config = root / ".devcontainer"
    config.mkdir()
    for name in ("compose.yaml", "devcontainer.json"):
        shutil.copyfile(TEMPLATES / name, config / name)
    env = dict(
        os.environ,
        MINDS_HOST_UID=str(os.getuid()),
        MINDS_HOST_GID=str(os.getgid()),
        MINDS_WORKSPACE_ROOT=str(root),
        MINDS_WORKSPACE_NAME=root.name,
        MINDS_WITNESS_HOME=str(state),
    )
    compose = [
        "docker",
        "compose",
        "--project-name",
        base.name,
        "--file",
        str(config / "compose.yaml"),
    ]
    report["path_map"] = {CONTAINER_ROOT: str(root)}
    report["uid"] = os.getuid()
    report["gid"] = os.getgid()
    report["docker_version"] = run(["docker", "version", "--format", "{{json .}}"])
    report["compose_version"] = run(["docker", "compose", "version", "--short"])
    report["watcher_version"] = run(
        [
            "inotifywait" if system == "Linux" else "fswatch",
            "--help" if system == "Linux" else "--version",
        ]
    ).splitlines()[0]

    def execute(script, *args):
        return run(
            compose
            + ["exec", "-T", "agent", "sh", "-eu", "-c", script, "probe", *args],
            env=env,
        )

    def check(name, action):
        try:
            report["checks"][name] = {"ok": True, "detail": action()}
        except (subprocess.SubprocessError, OSError, RuntimeError) as error:
            report["checks"][name] = {"ok": False, "detail": str(error)}

    with socket_probe(state / "run/witness.sock"):
        builder = base.name
        builder_image = "moby/buildkit:buildx-stable-1"
        image_existed = (
            subprocess.run(
                ["docker", "image", "inspect", builder_image],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=30,
                check=False,
            ).returncode
            == 0
        )
        base_image = "debian:bookworm-slim"
        base_image_existed = (
            subprocess.run(
                ["docker", "image", "inspect", base_image],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=30,
                check=False,
            ).returncode
            == 0
        )
        builder_created = False
        try:
            run(compose + ["config", "--quiet"], env=env)
            # Eigener Builder: auch Build-Cache und heruntergeladene Basis-Layer
            # verschwinden wieder, ohne einen bestehenden Cache zu löschen.
            run(
                [
                    "docker",
                    "buildx",
                    "create",
                    "--name",
                    builder,
                    "--driver",
                    "docker-container",
                    "--driver-opt",
                    f"image={builder_image}",
                    "--driver-opt",
                    "default-load=true",
                ]
            )
            builder_created = True
            run(compose + ["build", "--builder", builder], env=env, timeout=900)
            run(compose + ["up", "--no-build", "--detach"], env=env, timeout=60)
            check("claude", lambda: execute("claude --version"))
            check(
                "uid",
                lambda: execute(
                    'test "$(id -u)" = "$1"; test "$(id -g)" = "$2"; id',
                    str(os.getuid()),
                    str(os.getgid()),
                ),
            )
            check(
                "isolation",
                lambda: execute(
                    'test -z "${MINDS_WITNESS_HOME+x}"; '
                    'test ! -e "$1"; test ! -e /run/minds-witness/../witness.json; '
                    'test "$(ls -A /run/minds-witness)" = witness.sock; '
                    'test -S "$MINDS_WITNESS_SOCKET"; '
                    "test ! -e /var/run/docker.sock; "
                    'ln -s "$1/key/probe" isolation-link; test ! -r isolation-link',
                    str(state),
                ),
            )
            check(
                "socket_roundtrip",
                lambda: execute(
                    'reply=$(printf "ea-s2-ping\\n" | socat -T 3 - "UNIX-CONNECT:$MINDS_WITNESS_SOCKET"); '
                    'test "$reply" = ea-s2-pong; printf "%s" "$reply"'
                ),
            )
            # Commit nur im Wegwerf-Repo; weder globale Git-Konfiguration noch echte Hooks.
            execute(
                'git init -q; git config user.name "EA-S2 probe"; '
                'git config user.email "ea-s2@example.invalid"; '
                "git config core.hooksPath .git/hooks"
            )
            hook = root / ".git/hooks/post-commit"
            hook.write_text(
                '#!/bin/sh\nset -eu\nprintf "%s\\n" "$MINDS_WITNESS_SOCKET" > .git/ea-s2-hook\n'
            )
            hook.chmod(0o755)

            def commit():
                execute(
                    'git -c commit.gpgsign=false commit -q --allow-empty -m "EA-S2 probe"'
                )
                marker = (root / ".git/ea-s2-hook").read_text().strip()
                if marker != "/run/minds-witness/witness.sock":
                    raise RuntimeError(
                        "Post-commit hook did not inherit the socket environment"
                    )
                return marker

            check("post_commit", commit)
            # Dateimessung bleibt auch bei fehlgeschlagenem Socket-Test aussagekräftig.
            measure(compose, env, root, system, report)
        finally:

            def remove_if_new(image, existed):
                if existed:
                    return
                present = (
                    subprocess.run(
                        ["docker", "image", "inspect", image],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        timeout=30,
                        check=False,
                    ).returncode
                    == 0
                )
                if present:
                    run(["docker", "image", "rm", image], timeout=60)

            cleanup = [
                lambda: run(
                    compose
                    + ["down", "--volumes", "--remove-orphans", "--rmi", "local"],
                    env=env,
                    timeout=60,
                ),
                lambda: remove_if_new(builder_image, image_existed),
                lambda: remove_if_new(base_image, base_image_existed),
            ]
            if builder_created:
                cleanup.insert(
                    1, lambda: run(["docker", "buildx", "rm", builder], timeout=60)
                )
            for cleanup_action in cleanup:
                try:
                    cleanup_action()
                except (subprocess.SubprocessError, OSError) as error:
                    report.setdefault("cleanup_errors", []).append(str(error))


def passed(report):
    latency, burst = report.get("latency", {}), report.get("burst", {})
    return (
        not report.get("error")
        and not report.get("cleanup_errors")
        and all(
            report["checks"].get(name, {}).get("ok", False)
            for name in (
                "claude",
                "uid",
                "isolation",
                "socket_roundtrip",
                "post_commit",
            )
        )
        and latency.get("observed") == LATENCY_WRITES
        and latency.get("max_ms", float("inf")) < 1000
        and burst.get("observed") == BURST_WRITES
        and burst.get("host_files") == BURST_WRITES
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        required=True,
        type=Path,
        help="New JSON result file (never overwritten)",
    )
    parser.add_argument(
        "--scratch-parent",
        type=Path,
        default=Path("/tmp"),
        help="Docker-shared host directory; keep path short for Unix sockets",
    )
    args = parser.parse_args()
    system = platform.system()
    report = {
        "schema": 1,
        "host": platform.platform(),
        "checks": {},
        "status": "blocked",
        "date_utc": datetime.now(timezone.utc).isoformat(),
    }
    # Datei vor jeder Mutation exklusiv reservieren; bestehende Messungen erhalten.
    with args.output.open("x") as output:
        try:
            if system not in ("Linux", "Darwin"):
                raise RuntimeError("Only Linux and macOS hosts are supported")
            if os.getuid() == 0:
                raise RuntimeError("Run as a non-root host user")
            required = ["docker", "inotifywait" if system == "Linux" else "fswatch"]
            missing = [name for name in required if not shutil.which(name)]
            if missing:
                raise RuntimeError("Missing prerequisites: " + ", ".join(missing))
            with tempfile.TemporaryDirectory(
                prefix="ea-s2-", dir=args.scratch_parent
            ) as directory:
                report["status"] = "failed"
                experiment(Path(directory).resolve(), system, report)
                report["status"] = "passed" if passed(report) else "failed"
        except (
            OSError,
            RuntimeError,
            subprocess.SubprocessError,
            KeyboardInterrupt,
        ) as error:
            report["error"] = str(error)
        json.dump(report, output, indent=2)
        output.write("\n")
    print(f"EA-S2 {report['status']}: {args.output}")
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":

    def interrupt(_signal, _frame):
        raise KeyboardInterrupt("Interrupted; cleaning up temporary resources")

    signal.signal(signal.SIGTERM, interrupt)
    raise SystemExit(main())
