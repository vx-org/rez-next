import json
import tempfile
import unittest
from pathlib import Path

from generate_results import history_record, update_history


class GenerateResultsHistoryTests(unittest.TestCase):
    def test_history_replaces_a_rerun_for_the_same_commit(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "history.json"
            update_history(
                path,
                history_record(
                    results={"solve": {"mean_ns": 3, "low_ns": 2, "high_ns": 4}},
                    regressions=[],
                    sha="abc123",
                    timestamp="2026-01-02T00:00:00+00:00",
                    platform_name="linux",
                    cpu="test",
                    python_version="3.14",
                ),
            )
            update_history(
                path,
                history_record(
                    results={"solve": {"mean_ns": 5, "low_ns": 4, "high_ns": 6}},
                    regressions=["solve", "solve"],
                    sha="abc123",
                    timestamp="2026-01-03T00:00:00+00:00",
                    platform_name="linux",
                    cpu="test",
                    python_version="3.14",
                ),
            )

            history = json.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(len(history["runs"]), 1)
            self.assertEqual(history["runs"][0]["benchmarks"]["solve"]["mean_ns"], 5)
            self.assertEqual(history["runs"][0]["regressions"], ["solve"])


if __name__ == "__main__":
    unittest.main()
