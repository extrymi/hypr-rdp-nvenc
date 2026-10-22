#!/usr/bin/env python3
"""Verify the declared source/toolchain pins without network or hardware access."""

import argparse
import hashlib
import json
import re
import sys
import tomllib
from pathlib import Path


def verify(root: Path) -> None:
    manifest = json.loads((root / "ci/baseline.json").read_text())
    toolchain = tomllib.loads((root / "rust-toolchain.toml").read_text())["toolchain"]
    cargo = tomllib.loads((root / "Cargo.toml").read_text())
    lock = tomllib.loads((root / "Cargo.lock").read_text())
    workflow = (root / ".github/workflows/ci.yml").read_text()

    def require(condition: bool, message: str) -> None:
        if not condition:
            raise ValueError(message)

    require(manifest["schema_version"] == 1, "unsupported baseline schema")
    require(toolchain["channel"] == manifest["rust"]["channel"], "Rust toolchain pin differs")
    require(manifest["rust"]["target"] in toolchain["targets"], "missing target pin")
    require(cargo["package"]["version"] == manifest["upstream"]["version"], "baseline version differs")
    for path, key in (("Cargo.lock", "cargo_lock_sha256"), ("Cargo.toml", "cargo_toml_sha256")):
        actual = hashlib.sha256((root / path).read_bytes()).hexdigest()
        require(actual == manifest[key], f"{path} differs from declared baseline hash")

    revision = manifest["ironrdp_revision"]
    require(re.fullmatch(r"[0-9a-f]{40}", revision) is not None, "invalid IronRDP pin")
    expected_git = f"git+https://github.com/Devolutions/IronRDP.git?rev={revision}#{revision}"
    git_sources = {pkg["source"] for pkg in lock["package"] if pkg.get("source", "").startswith("git+")}
    require(git_sources == {expected_git}, "unexpected or unpinned Git dependency")
    for section in ("dependencies", "dev-dependencies", "build-dependencies"):
        for name, dep in cargo.get(section, {}).items():
            if isinstance(dep, dict) and "git" in dep:
                require(dep.get("rev") == revision, f"{name} has an unexpected Git revision")
                require(dep["git"] == "https://github.com/Devolutions/IronRDP.git", f"{name} has an unexpected Git URL")
    require(len(lock["package"]) == manifest["locked_packages"], "locked package count differs")
    for pkg in lock["package"]:
        if pkg.get("source", "").startswith("registry+"):
            require(re.fullmatch(r"[0-9a-f]{64}", pkg.get("checksum", "")) is not None,
                    f"registry checksum missing for {pkg['name']}")

    actions = re.findall(r"uses:\s*([^\s@]+)@([^\s#]+)", workflow)
    require(len(actions) == len(manifest["actions"]), "unexpected Actions dependency count")
    require({name for name, _ in actions} == set(manifest["actions"]), "Actions dependency set differs")
    require(f'RUSTUP_TOOLCHAIN: "{toolchain["channel"]}"' in workflow, "CI Rust toolchain differs")
    require(f'runs-on: {manifest["runner"]}' in workflow, "CI runner differs")
    for name, commit in actions:
        require(name in manifest["actions"], f"unexpected action: {name}")
        require(re.fullmatch(r"[0-9a-f]{40}", commit) is not None, f"{name} is not pinned to a commit")
        require(commit == manifest["actions"][name]["commit"], f"{name} pin differs")
    print(f"Verified baseline: {manifest['upstream']['commit']}, "
          f"Rust {toolchain['channel']}, {len(lock['package'])} locked packages.")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    try:
        verify(args.root)
    except (OSError, ValueError, KeyError, tomllib.TOMLDecodeError) as error:
        print(f"Baseline verification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
