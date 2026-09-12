import argparse
import json
import math
from pathlib import Path
from statistics import median


NOISE_FLOOR = 0.20


def observations(trials, count):
    if type(count) is not int or count < 3 or len(trials) != count:
        raise ValueError("incomplete trial set")
    indexes = [trial.get("index") for trial in trials]
    if any(type(index) is not int for index in indexes) or set(indexes) != set(range(count)):
        raise ValueError("missing or duplicate trial indexes")
    for trial in trials:
        if trial.get("correct") is not True:
            raise ValueError("trial failed correctness")
        for field in ("ready_ms", "cleared_ms"):
            value = trial.get(field)
            if type(value) not in (int, float) or not math.isfinite(value) or value <= 0:
                raise ValueError(f"invalid {field}")
        if trial["cleared_ms"] < trial["ready_ms"]:
            raise ValueError("cleanup precedes input readiness")
    return [trial["ready_ms"] for trial in trials]


def compare(result):
    count = result["count"]
    baseline = median(observations(result["trials"]["baseline"], count))
    candidate = median(observations(result["trials"]["candidate"], count))
    change = candidate / baseline - 1
    outcome = "no clear change"
    if change > NOISE_FLOOR:
        outcome = "regressed"
    elif change < -NOISE_FLOOR:
        outcome = "improved"
    return {"outcome": outcome, "baseline_ms": baseline, "candidate_ms": candidate,
            "change_fraction": change, "noise_floor": NOISE_FLOOR}


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("result", type=Path)
    args = parser.parse_args()
    print(json.dumps(compare(json.loads(args.result.read_text())), indent=2))
