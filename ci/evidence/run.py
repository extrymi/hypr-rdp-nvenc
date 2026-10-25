#!/usr/bin/env python3
"""Run exact Rust tests on exact revisions and check the expected outcome.

A case is valid only when each listed test is selected exactly once by the
named target and ends with the expected libtest result. An expected failure
must also show every expected assertion fragment. Compile errors, a missing or
duplicated test, a different failure or a crash invalidate the evidence; they
are never counted as a reproduced defect.
"""
import json
import re
import subprocess
import sys
from pathlib import Path


def git(*args):
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout.strip()


def run(command, log, timeout=1800):
    try:
        proc = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired as expired:
        log.write_text(f"$ {' '.join(command)}\nTIMEOUT after {timeout}s\n{expired.stdout or ''}")
        return subprocess.CompletedProcess(command, 124, expired.stdout or "", expired.stderr or "")
    log.write_text(f"$ {' '.join(command)}\nexit={proc.returncode}\n"
                   f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    return proc


def summary(stdout):
    totals = re.findall(r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored", stdout, re.M)
    return [(r, int(p), int(f), int(i)) for r, p, f, i in totals]


def check_test(base, name, expect, log):
    proc = run(base + ["--", "--exact", name], log)
    outcome = re.findall(rf"^test {re.escape(name)} \.\.\. (ok|FAILED)$", proc.stdout, re.M)
    totals = summary(proc.stdout)
    # Fixtures that run copies of the test binary print their own summaries
    # first; the selecting harness prints the last one.
    final = totals[-1] if totals else None
    problems = []
    if len(outcome) != 1 or final is None or final[1] + final[2] != 1:
        problems.append(f"expected exactly one selected run, got outcome={outcome} totals={totals}")
    elif outcome[0] != expect["result"]:
        problems.append(f"result {outcome[0]} != expected {expect['result']}")
    if expect["result"] == "FAILED":
        if proc.returncode != 101:
            problems.append(f"exit {proc.returncode} != 101 (libtest failure)")
        missing = [m for m in expect.get("messages", []) if m not in proc.stdout + proc.stderr]
        if missing:
            problems.append(f"missing assertion text {missing}")
    elif proc.returncode != 0:
        problems.append(f"exit {proc.returncode} != 0")
    return {"test": name, "expected": expect["result"], "observed": outcome[0] if outcome else None,
            "exit": proc.returncode, "nested_summaries": max(len(totals) - 1, 0),
            "valid": not problems, "problems": problems, "log": log.name}


def check_suite(base, suite, log):
    proc = run(base + ["--", suite["filter"]], log)
    totals = summary(proc.stdout)
    passed = sum(t[1] for t in totals)
    failed = sum(t[2] for t in totals)
    problems = []
    if proc.returncode != 0 or failed:
        problems.append(f"exit {proc.returncode}, {failed} failed")
    if passed < suite["min_passed"]:
        problems.append(f"only {passed} passed < {suite['min_passed']}")
    return {"suite": suite["filter"], "passed": passed, "failed": failed, "exit": proc.returncode,
            "valid": not problems, "problems": problems, "log": log.name}


def main(spec_path, out):
    spec = json.loads(Path(spec_path).read_text())
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    report = []
    for case in spec["cases"]:
        git("checkout", "--quiet", "--force", "--detach", case["revision"])
        head = git("rev-parse", "HEAD")
        if head != case["revision"]:
            raise SystemExit(f"checked out {head}, expected {case['revision']}")
        git("clean", "-fdq", "-e", "target", "-e", "artifacts")
        subprocess.run(["cargo", "fetch", "--locked"], check=True)
        base = ["cargo", "test", "--frozen", *case["cargo"]]
        listing = run(base + ["--", "--list"], out / f"{case['id']}-list.log")
        names = [line[: -len(": test")] for line in listing.stdout.splitlines() if line.endswith(": test")]
        entry = {"id": case["id"], "revision": head, "cargo": case["cargo"], "list_exit": listing.returncode,
                 "tests": [], "suites": []}
        if listing.returncode != 0:
            entry["valid"] = False
            entry["problems"] = ["test listing failed (compile error or bad target); not a reproduction"]
        else:
            for name, expect in case.get("tests", {}).items():
                count = names.count(name)
                if count != 1:
                    entry["tests"].append({"test": name, "valid": False,
                                           "problems": [f"selected {count} times by --list"]})
                    continue
                entry["tests"].append(check_test(base, name, expect, out / f"{case['id']}-{name.replace('::', '.')}.log"))
            for index, suite in enumerate(case.get("suites", [])):
                entry["suites"].append(check_suite(base, suite, out / f"{case['id']}-suite{index}.log"))
            entry["valid"] = all(t["valid"] for t in entry["tests"] + entry["suites"])
        report.append(entry)
        print(json.dumps(entry, indent=2))
    (out / "evidence.json").write_text(json.dumps(report, indent=2) + "\n")
    if not all(case["valid"] for case in report):
        raise SystemExit("evidence does not match the expected outcomes")
    print("All evidence cases matched their expected outcomes")


if __name__ == "__main__":
    main(*sys.argv[1:])
