#!/usr/bin/env python3
"""CPU ExecStart stand-in. The installed production ExecCondition is retained."""
import json
import os
from pathlib import Path
import signal
import subprocess

root = Path.home() / ".config/hypr-rdp"
mask = os.umask(0o077)
os.umask(mask)
if (root / "fail-oracle").exists():
    raise SystemExit(1)
child = subprocess.Popen(["sleep", "300"])

def stop(_signal, _frame):
    child.terminate()
    child.wait(timeout=5)
    raise SystemExit(0)

signal.signal(signal.SIGTERM, signal.SIG_IGN if (root / "ignore-stop").exists() else stop)
(root / "oracle.tmp").write_text(json.dumps({
    "pid": os.getpid(), "child": child.pid, "umask": mask,
    "keylog_present": "SSLKEYLOGFILE" in os.environ,
    "wayland": os.environ.get("WAYLAND_DISPLAY"),
    "instance": os.environ.get("HYPRLAND_INSTANCE_SIGNATURE"),
}))
(root / "oracle.tmp").replace(root / "oracle.json")
signal.pause()
