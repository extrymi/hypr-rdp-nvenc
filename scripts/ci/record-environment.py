#!/usr/bin/env python3
"""Record build provenance on the disposable CI runner; never dump its environment."""

import json
import os
import subprocess
from pathlib import Path


def capture(*args: str) -> str:
    return subprocess.run(args, check=True, text=True, capture_output=True).stdout.strip()


root = Path(__file__).resolve().parents[2]
artifacts = root / "artifacts"
artifacts.mkdir(exist_ok=True)
manifest = json.loads((root / "ci/baseline.json").read_text())
rust = capture("rustc", "-vV")
if f"release: {manifest['rust']['channel']}" not in rust.splitlines():
    raise SystemExit("Active rustc differs from the declared toolchain")
if f"host: {manifest['rust']['target']}" not in rust.splitlines():
    raise SystemExit("Active rustc host differs from the baseline target")
result = {
    "schema_version": 1,
    "source_commit": capture("git", "rev-parse", "HEAD"),
    "source_parents": capture("git", "show", "-s", "--format=%P", "HEAD"),
    "upstream_baseline": manifest["upstream"],
    "scope": manifest["scope"],
    "runner": {name: os.environ.get(name) for name in ("RUNNER_OS", "RUNNER_ARCH", "ImageOS", "ImageVersion")},
    "run": {name: os.environ.get(name) for name in ("GITHUB_REPOSITORY", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_SHA", "GITHUB_REF")},
    "os_release": Path("/etc/os-release").read_text(),
    "kernel": capture("uname", "-srmo"),
    "tools": {
        "rustc": rust,
        "cargo": capture("cargo", "-vV"),
        "cc": capture("cc", "--version"),
        "clang": capture("clang", "--version"),
        "cmake": capture("cmake", "--version"),
        "pkg_config": capture("pkg-config", "--version"),
        "linker": capture("ld", "--version"),
    },
}
(artifacts / "environment.json").write_text(json.dumps(result, indent=2) + "\n")
(artifacts / "system-packages.tsv").write_text(
    capture("dpkg-query", "-W", "-f=${binary:Package}\t${Version}\n") + "\n"
)
print("Recorded compiler, runner image and package provenance.")
