#!/usr/bin/env python3
"""Preserve dependency notices from the exact Cargo-resolved source trees."""
import hashlib
import json
from pathlib import Path
import shutil
import sys

metadata = json.loads(Path(sys.argv[1]).read_text())
destination = Path(sys.argv[2])
# Upstream texts for crates whose published .crate omits them (see the bundle's
# repository/commit/sha256 records). Every resolved crate must end with a text.
supplemental = {}
for crate in json.loads(Path(sys.argv[3]).read_text())["crates"]:
    for file in crate["files"]:
        if hashlib.sha256(file["text"].encode()).hexdigest() != file["sha256"]:
            raise ValueError(f'supplemental text changed: {crate["name"]} {file["path"]}')
    supplemental[(crate["name"], crate["version"])] = crate
destination.mkdir(parents=True, exist_ok=True)
bundles = {}
index = []
missing = []
critical = {"openh264-sys2", "aws-lc-sys", "ring"}
for package in sorted(metadata["packages"], key=lambda p: p["id"]):
    source = package.get("source")
    if source is None:
        continue  # The project's own MIT notice is installed separately.
    root = Path(package["manifest_path"]).parent.resolve()
    if source.startswith("git+"):
        root = next(p for p in (root, *root.parents) if (p / ".git").exists())
    if root not in bundles:
        identifier = f'{package["name"]}-{package["version"]}-' + hashlib.sha256(source.encode()).hexdigest()[:8]
        notices = []
        for candidate in sorted(root.rglob("*")):
            if ".git" in candidate.relative_to(root).parts or candidate.is_symlink():
                continue
            if candidate.is_file() and candidate.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT", "PATENTS", "AUTHORS")):
                notices.append(candidate)
        declared = package.get("license_file")
        if declared:
            path = Path(declared)
            if not path.is_absolute():
                path = Path(package["manifest_path"]).parent / path
            path = path.resolve(strict=True)
            if not path.is_relative_to(root):
                raise ValueError(f'license_file escapes source tree: {package["name"]}')
            if path not in notices:
                notices.append(path)
        files = []
        for notice in notices:
            relative = notice.relative_to(root)
            output = destination / identifier / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(notice, output)
            files.append(str(Path(identifier) / relative))
        bundles[root] = files
    files = bundles[root]
    supplement = supplemental.pop((package["name"], package["version"]), None)
    if not files and supplement:
        identifier = f'{package["name"]}-{package["version"]}-supplemental'
        for file in supplement["files"]:
            output = destination / identifier / file["path"]
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(file["text"])
            files.append(str(Path(identifier) / file["path"]))
    elif supplement:
        raise ValueError(f'supplemental text is redundant: {package["name"]}-{package["version"]}')
    if not files:
        missing.append(f'{package["name"]}-{package["version"]}')
    if package["name"] in critical:
        if not files:
            raise ValueError(f'missing bundled native library notices: {package["name"]}')
        critical.remove(package["name"])
    index.append({"name": package["name"], "version": package["version"],
                  "source": source, "license": package.get("license"),
                  "authors": package.get("authors", []), "notices": files})
if critical:
    raise ValueError(f"critical dependencies absent from metadata: {critical}")
resolved = {package["name"] for package in metadata["packages"]}
if drifted := sorted(key for key in supplemental if key[0] in resolved):
    raise ValueError(f"supplemental texts for other crate versions: {drifted}")
if supplemental:
    # Not in this target's resolve (for example a platform-specific crate).
    print(f"Unused supplemental texts: {sorted(supplemental)}")
(destination / "index.json").write_text(json.dumps({
    "scope": "Cargo-resolved dependencies for x86_64-unknown-linux-gnu, including build/dev dependencies; not a linked-code inventory or legal opinion",
    "packages": index, "without_packaged_notice_files": missing,
}, indent=2) + "\n")
print(f"Preserved notices for {len(index)} resolved packages")
if missing:
    raise ValueError("resolved crates without any license text: " + ", ".join(missing))
