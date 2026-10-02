"""Auswertung unabhängig von Docker gegen fehlende und doppelte Events prüfen."""

import copy
import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import ea_s2_probe as probe


class ProbeTests(unittest.TestCase):
    def test_nearest_rank_and_empty_samples(self):
        self.assertIsNone(probe.percentile([], 0.95))
        self.assertEqual(probe.percentile(list(range(200, 0, -1)), 0.50), 100)
        self.assertEqual(probe.percentile(list(range(200, 0, -1)), 0.95), 190)

    def test_duplicate_split_events_and_missing_paths(self):
        read_fd, write_fd = os.pipe()
        with (
            os.fdopen(read_fd, "rb") as reader,
            os.fdopen(write_fd, "wb", buffering=0) as writer,
        ):
            events = probe.Events(SimpleNamespace(stdout=reader, poll=lambda: None))
            writer.write(b"/repo/first\0/repo/sec")
            first = events.wait_for(["/repo/first"], 1)["/repo/first"]
            writer.write(b"ond\0/repo/first\0")
            seen = events.wait_for(
                ["/repo/first", "/repo/second", "/repo/missing"], 0.1
            )
            self.assertEqual(set(seen), {"/repo/first", "/repo/second"})
            self.assertEqual(seen["/repo/first"], first)
            writer.close()
            events.thread.join(timeout=1)
            self.assertFalse(events.thread.is_alive())

    def test_no_pass_for_incomplete_or_slow_run(self):
        complete = {
            "checks": {
                name: {"ok": True}
                for name in (
                    "claude",
                    "uid",
                    "isolation",
                    "socket_roundtrip",
                    "post_commit",
                )
            },
            "latency": {"observed": 200, "max_ms": 999},
            "burst": {"observed": 1000, "host_files": 1000},
        }
        self.assertTrue(probe.passed(complete))
        for section, key, value in (
            ("latency", "observed", 199),
            ("latency", "max_ms", 1000),
            ("burst", "observed", 999),
            ("burst", "host_files", 999),
            ("checks", "socket_roundtrip", {"ok": False}),
            ("checks", "isolation", {"ok": False}),
        ):
            with self.subTest(section=section, key=key):
                failed = copy.deepcopy(complete)
                failed[section][key] = value
                self.assertFalse(probe.passed(failed))
        self.assertFalse(probe.passed({"checks": {}}))
        self.assertFalse(
            probe.passed(dict(complete, cleanup_errors=["failed cleanup"]))
        )

    def test_output_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "result.json"
            output.write_text("existing measurement")
            with (
                patch("sys.argv", ["probe", "--output", str(output)]),
                self.assertRaises(FileExistsError),
            ):
                probe.main()
            self.assertEqual(output.read_text(), "existing measurement")


if __name__ == "__main__":
    unittest.main()
