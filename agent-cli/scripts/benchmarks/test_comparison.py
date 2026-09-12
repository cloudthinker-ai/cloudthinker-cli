import copy
import json
from pathlib import Path
import unittest

from comparison import compare


def result(candidate=100):
    return {"count": 3, "trials": {
        label: [{"index": i, "ready_ms": value, "cleared_ms": value + 1, "correct": True} for i in range(3)]
        for label, value in [("baseline", 100), ("candidate", candidate)]
    }}


class ComparisonTests(unittest.TestCase):
    def test_ca_ad_9_complete_correct_trials_are_compared(self):
        for value, expected in [(100, "no clear change"), (140, "regressed"), (60, "improved")]:
            with self.subTest(value=value):
                self.assertEqual(compare(result(value))["outcome"], expected)

    def test_ca_ad_9_incomplete_or_incorrect_trials_are_never_wins(self):
        bad_sets = []
        for field, value in [("index", 1), ("ready_ms", None), ("ready_ms", float("nan")),
                             ("ready_ms", float("inf")), ("ready_ms", -1), ("correct", False),
                             ("cleared_ms", 1)]:
            bad = result(50)
            bad["trials"]["candidate"][0][field] = value
            bad_sets.append(bad)
        bad = result(50)
        bad["trials"]["candidate"].pop()
        bad_sets.append(bad)
        for bad in bad_sets:
            with self.subTest(trials=bad):
                with self.assertRaises(ValueError):
                    compare(copy.deepcopy(bad))

    def test_ca_ad_9_recorded_same_binary_controls_are_not_regressions(self):
        calibration = json.loads(Path(__file__).with_name("calibration.json").read_text())
        self.assertEqual(calibration["baseline_sha256"], calibration["candidate_sha256"])
        self.assertEqual(compare(calibration)["outcome"], "no clear change")


if __name__ == "__main__":
    unittest.main()
