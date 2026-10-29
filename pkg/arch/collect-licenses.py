#!/usr/bin/env python3
"""Preserve dependency notices from the exact Cargo-resolved source trees.

A crate's own licence text is a LICENSE/LICENCE/COPYING/COPYRIGHT/NOTICE file
at its source root, or its declared license_file. Notices deeper in the tree
belong to vendored components: they are kept, but never count as the crate's
own. A crate without its own text needs a verified upstream file from the
supplemental bundle or an explicit unresolved record there; no text is ever
invented, and any other gap fails the build.
"""
import hashlib
import json
from pathlib import Path
import shutil
import sys

NOTICE = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT", "PATENTS", "AUTHORS")
OWN = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT")

metadata = json.loads(Path(sys.argv[1]).read_text())
destination = Path(sys.argv[2])
bundle = json.loads(Path(sys.argv[3]).read_text())
# Upstream texts for crates whose published .crate omits them (see the bundle's
# repository/commit/sha256 records), and the crates for which none exists.
supplemental = {}
for crate in bundle["crates"]:
    for file in crate["files"]:
        if hashlib.sha256(file["text"].encode()).hexdigest() != file["sha256"]:
            raise ValueError(f'supplemental text changed: {crate["name"]} {file["path"]}')
    supplemental[(crate["name"], crate["version"])] = crate
unresolved = {(crate["name"], crate["version"]): crate for crate in bundle["unresolved"]}
if overlap := sorted(supplemental.keys() & unresolved.keys()):
    raise ValueError(f"crates both supplemented and unresolved: {overlap}")
destination.mkdir(parents=True, exist_ok=True)
bundles = {}
index = []
missing = []
gaps = []
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
            if candidate.is_file() and candidate.name.upper().startswith(NOTICE):
                notices.append(candidate)
        own = [n for n in notices if n.parent == root and n.name.upper().startswith(OWN)]
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
            if path not in own:
                own.append(path)
        files = {}
        for notice in notices:
            relative = notice.relative_to(root)
            output = destination / identifier / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(notice, output)
            files[notice] = str(Path(identifier) / relative)
        bundles[root] = (list(files.values()), [files[n] for n in own])
    files, own = (list(part) for part in bundles[root])
    key = (package["name"], package["version"])
    supplement = supplemental.pop(key, None)
    gap = unresolved.pop(key, None)
    if supplement:
        if own:
            raise ValueError(f'supplemental text is redundant: {package["name"]}-{package["version"]}')
        identifier = f'{package["name"]}-{package["version"]}-supplemental'
        for file in supplement["files"]:
            output = destination / identifier / file["path"]
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(file["text"])
            files.append(str(Path(identifier) / file["path"]))
            own.append(files[-1])
    if gap and own:
        raise ValueError(f'unresolved record is stale: {package["name"]}-{package["version"]}')
    if not own and not gap:
        raise ValueError(f'no own licence text and no unresolved record: {package["name"]}-{package["version"]}')
    if not files:
        missing.append(f'{package["name"]}-{package["version"]}')
    if package["name"] in critical:
        if not files:
            raise ValueError(f'missing bundled native library notices: {package["name"]}')
        critical.remove(package["name"])
    if gap:
        gaps.append(gap)
    index.append({"name": package["name"], "version": package["version"],
                  "source": source, "license": package.get("license"),
                  "authors": package.get("authors", []), "notices": files,
                  "own_notices": own, "unresolved": bool(gap)})
if critical:
    raise ValueError(f"critical dependencies absent from metadata: {critical}")
resolved = {package["name"] for package in metadata["packages"]}
for name, records in (("supplemental texts", supplemental), ("unresolved records", unresolved)):
    if drifted := sorted(key for key in records if key[0] in resolved):
        raise ValueError(f"{name} for other crate versions: {drifted}")
    if records:
        # Not in this target's resolve (for example a platform-specific crate).
        print(f"Unused {name}: {sorted(records)}")
(destination / "index.json").write_text(json.dumps({
    "scope": "Cargo-resolved dependencies for x86_64-unknown-linux-gnu, including build/dev dependencies; not a linked-code inventory or legal opinion",
    "packages": index, "without_packaged_notice_files": missing,
    "unresolved_notices": gaps,
}, indent=2) + "\n")
print(f"Preserved notices for {len(index)} resolved packages")
if gaps:
    print("Unresolved licence notices (no verified text; distribution blocked): "
          + ", ".join(f'{gap["name"]}-{gap["version"]}' for gap in gaps))
