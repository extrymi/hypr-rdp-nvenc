#!/usr/bin/env python3
"""Check package source integrity and CI pins without executing a PKGBUILD."""
import hashlib
import json
from pathlib import Path
import re
import shlex

root = Path(__file__).resolve().parents[2]
recipe = root / "pkg/arch"
text = (recipe / "PKGBUILD").read_text()
manifest = json.loads((root / "ci/arch.json").read_text())
baseline = json.loads((root / "ci/baseline.json").read_text())
assert re.fullmatch(r"archlinux:base-devel@sha256:[0-9a-f]{64}", manifest["image"])
assert re.fullmatch(r"archlinux:base@sha256:[0-9a-f]{64}", manifest["runtime_image"])
assert re.fullmatch(r"[0-9]{4}/[0-9]{2}/[0-9]{2}", manifest["snapshot"])
assert f'_commit={manifest["runtime_commit"]}' in text
assert manifest["rust"] == baseline["rust"]["channel"]
hashes = shlex.split(re.search(r"sha256sums=\((.*?)\)", text, re.S)[1])
assert hashes[0] == "SKIP" and len(hashes) == 6
for name, expected in zip(("hypr-rdp-nvenc.service", "config.example.toml", "hypr-rdp-tls", "arch-package.md", "collect-licenses.py"), hashes[1:]):
    assert hashlib.sha256((recipe / name).read_bytes()).hexdigest() == expected, name
workflow = (root / ".github/workflows/arch-package.yml").read_text()
actions = re.findall(r"uses:\s*([^\s@]+)@([^\s#]+)", workflow)
assert len(actions) == 2
for name, commit in actions:
    assert commit == baseline["actions"][name]["commit"], name
assert 'runs-on: ubuntu-24.04' in workflow
print("Verified Arch image/snapshot/toolchain/runtime pins and all local source checksums")
