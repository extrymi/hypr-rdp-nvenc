#!/usr/bin/env python3
"""Read-only source integrity verification; no compiler or GPU is needed."""
import hashlib
import json
from pathlib import Path

root = Path(__file__).resolve().parents[2] / "vendor/nv-codec-headers"
manifest = json.loads((root / "source.json").read_text())
assert manifest["commit"] == "0a6fba9a2820628b8103464f4c8753ee05838baa"
assert manifest["tag"] == "n13.1.15.0"
assert {f["path"] for f in manifest["files"]} == {"nvEncodeAPI.h", "dynlink_cuda.h"}
for item in manifest["files"]:
    data = (root / item["path"]).read_bytes()
    blob = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
    assert blob == item["git_blob"], item["path"]
    assert hashlib.sha256(data).hexdigest() == item["sha256"], item["path"]
    assert b"Permission is hereby granted" in data and b"copyright notice" in data
print("NVENC/CUDA headers match pinned upstream blobs and checksums; notices retained.")
