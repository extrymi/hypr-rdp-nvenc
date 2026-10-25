#!/usr/bin/env python3
"""Fail when this tree's runtime inputs differ from the package's runtime pin.

PKGBUILD builds the runtime from `_commit`; README/docs/PAM/LICENSE are also
installed from that tree. Every path outside the packaging-only set must be
identical between the pin and the revision under test. Runtime changes are
released by committing them first, then bumping `_commit` to that commit.
"""
import re
import subprocess
import sys
from pathlib import Path

PACKAGING_ONLY = ("pkg/arch", "scripts/ci", "ci", ".github")


def pinned_commit(recipe):
    pins = re.findall(r"^_commit=([0-9a-f]{40})$", recipe, re.M)
    if len(pins) != 1:
        raise SystemExit("PKGBUILD must pin exactly one full 40-hex _commit")
    return pins[0]


def check(root, revision="HEAD"):
    commit = pinned_commit((root / "pkg/arch/PKGBUILD").read_text())
    git = ["git", "-C", str(root)]

    def ok(*args):
        return subprocess.run(git + list(args), capture_output=True).returncode == 0

    if not ok("cat-file", "-e", f"{commit}^{{commit}}"):
        raise SystemExit(f"runtime pin {commit} is absent; fetch full history")
    if not ok("merge-base", "--is-ancestor", commit, revision):
        raise SystemExit(f"runtime pin {commit} is not an ancestor of {revision}")
    excludes = [f":(exclude){path}" for path in PACKAGING_ONLY]
    changed = subprocess.run(
        git + ["diff", "--name-only", commit, revision, "--", "."] + excludes,
        capture_output=True, text=True, check=True,
    ).stdout.split()
    if changed:
        raise SystemExit("runtime inputs differ from the package pin " + commit
                         + "; commit them as runtime and bump _commit:\n  " + "\n  ".join(changed))
    docs = (root / "pkg/arch/arch-package.md").read_text()
    stale = sorted(set(re.findall(r"\b[0-9a-f]{40}\b", docs)) - {commit})
    if stale:
        raise SystemExit(f"arch-package.md names another runtime commit: {stale}")
    return commit


if __name__ == "__main__":
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[2]
    print(f"Runtime inputs match package pin {check(root)}")
