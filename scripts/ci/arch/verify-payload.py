#!/usr/bin/env python3
"""Read-only byte comparison of an Arch package with its exact source commits.

Usage: python3 -I verify_package_payload.py PACKAGE REPO RUNTIME_SHA PACKAGING_SHA
Requires Python's compression.zstd. No extraction or executable invocation.
Unresolved notices are checked for honest accounting, not legal sufficiency.
"""
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tarfile
from compression import zstd


def verify(package, repo, runtime, packaging):
    def git(*args):
        return subprocess.run(["git", "-c", f"safe.directory={Path(repo).resolve()}",
                               "-C", str(repo), *args], check=True,
                              capture_output=True).stdout

    def blob(revision, path):
        return git("show", f"{revision}:{path}")

    with zstd.open(package, "rb") as raw:
        data = raw.read()
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        files = {}
        for member in archive:
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts:
                raise ValueError(f"unsafe archive path: {member.name}")
            name = str(path)
            if member.isfile():
                if name in files:
                    raise ValueError(f"duplicate archive file: {name}")
                files[name] = archive.extractfile(member).read()

    def same(name, expected):
        if files.get(name) != expected:
            raise ValueError(f"missing or different packaged file: {name}")

    doc = "usr/share/doc/hypr-rdp-nvenc/"
    licenses = "usr/share/licenses/hypr-rdp-nvenc/"
    dependencies = licenses + "dependency-licenses/"
    pairs = {
        doc + Path(path).name: (runtime, path)
        for path in ("README.md", "CHANGELOG.md", "docs/native-nvenc.md",
                     "docs/nvenc-integration.md", "docs/startup-validation.md")
    }
    pairs.update({
        "etc/pam.d/hypr-rdp": (runtime, "pkg/pam/hypr-rdp.arch"),
        licenses + "LICENSE": (runtime, "LICENSE"),
        doc + "arch-package.md": (packaging, "pkg/arch/arch-package.md"),
        doc + "config.example.toml": (packaging, "pkg/arch/config.example.toml"),
        "usr/lib/systemd/user/hypr-rdp-nvenc.service":
            (packaging, "pkg/arch/hypr-rdp-nvenc.service"),
        "usr/bin/hypr-rdp-tls": (packaging, "pkg/arch/hypr-rdp-tls"),
    })
    for path in git("ls-tree", "-r", "--name-only", runtime,
                    "vendor/nv-codec-headers").decode().splitlines():
        pairs[licenses + "nv-codec-headers/" + Path(path).name] = (runtime, path)
    for name, (revision, path) in pairs.items():
        same(name, blob(revision, path))

    spec = json.loads(blob(packaging, "pkg/arch/supplemental-licenses.json"))
    index = json.loads(files[dependencies + "index.json"])
    packages = index["packages"]
    if (not isinstance(packages, list) or not packages
            or not isinstance(spec["crates"], list)
            or not isinstance(spec["unresolved"], list)):
        raise ValueError("empty or unexpected dependency notice schema")
    resolved = {(p["name"], p["version"]): p for p in packages}
    if len(resolved) != len(packages):
        raise ValueError("ambiguous dependency package identities")
    resolved_names = {name for name, _ in resolved}
    for entry in spec["crates"] + spec["unresolved"]:
        key = (entry["name"], entry["version"])
        if key[0] in resolved_names and key not in resolved:
            raise ValueError(f"notice specification version differs from index: {key}")
    supplemented = 0
    for entry in spec["crates"]:
        key = (entry["name"], entry["version"])
        if key not in resolved:
            continue
        if not isinstance(entry["files"], list) or not entry["files"]:
            raise ValueError(f"empty or invalid supplemental files: {key}")
        prefix = f"{entry['name']}-{entry['version']}-supplemental/"
        for notice in entry["files"]:
            expected = notice["text"].encode()
            if hashlib.sha256(expected).hexdigest() != notice["sha256"]:
                raise ValueError(f"supplemental self-hash mismatch: {key}")
            name = prefix + notice["path"]
            same(dependencies + name, expected)
            if name not in resolved[key]["own_notices"]:
                raise ValueError(f"supplemental notice missing from index: {key}")
            supplemented += 1
    if spec["crates"] and not supplemented:
        raise ValueError("zero supplemental texts checked; not a valid verification")

    gaps = {(g["name"], g["version"]): g for g in spec["unresolved"]
            if (g["name"], g["version"]) in resolved}
    recorded = {(g["name"], g["version"]): g for g in index["unresolved_notices"]}
    if recorded != gaps:
        raise ValueError("unresolved records differ from the exact packaging spec")
    no_notices = []
    for key, entry in resolved.items():
        notices = entry["notices"]
        own = entry["own_notices"]
        if (not isinstance(notices, list) or not isinstance(own, list)
                or not all(isinstance(n, str) for n in notices + own)
                or not isinstance(entry["unresolved"], bool)):
            raise ValueError(f"invalid notice index schema: {key}")
        if not notices:
            no_notices.append(f"{key[0]}-{key[1]}")
        for notice in notices:
            if dependencies + notice not in files:
                raise ValueError(f"indexed notice is absent: {notice}")
        if not set(own).issubset(notices):
            raise ValueError(f"own notice absent from notices list: {key}")
        if entry["unresolved"] != (key in gaps) or bool(own) == (key in gaps):
            raise ValueError(f"unresolved/own-notice accounting inconsistent: {key}")
    if sorted(index["without_packaged_notice_files"]) != sorted(no_notices):
        raise ValueError("missing-notice accounting differs from package inventory")
    return {"source_payload_files": len(pairs), "resolved_packages": len(packages),
            "supplemental_texts_checked": supplemented,
            "unresolved": sorted(f"{n}-{v}" for n, v in gaps),
            "runtime": runtime, "packaging": packaging, "valid": True}


if __name__ == "__main__":
    try:
        print(json.dumps(verify(*sys.argv[1:]), indent=2))
    except (KeyError, ValueError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"Package verification failed: {error}") from None
