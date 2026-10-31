#!/usr/bin/env python3
"""Old/new package transactions and real ExecStart in a disposable CPU root.

No compositor or client is simulated. The actual server is expected to fail
boundedly on the missing Hyprland socket after its real ExecCondition succeeds.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

UNIT = "hypr-rdp-nvenc.service"
DESKTOP_HOME = Path("/home/tester")
CONFIG_DIR = DESKTOP_HOME / ".config/hypr-rdp"
OUTPUT = Path("/out")
CHECKS = []


def run(*args, ok=True, timeout=30):
    result = subprocess.run(list(map(str, args)), capture_output=True, text=True,
                            timeout=timeout)
    if ok and result.returncode:
        raise AssertionError(f"{args}: {result.returncode}\n{result.stdout}\n{result.stderr}")
    return result


def user(*args, **kwargs):
    return run("runuser", "-u", "tester", "--", "env",
               "XDG_RUNTIME_DIR=/run/user/1100",
               "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1100/bus", *args, **kwargs)


def ctl(*args, **kwargs):
    return user("systemctl", "--user", *args, **kwargs)


def prop(name):
    return ctl("show", UNIT, "--property", name, "--value").stdout.strip()


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fingerprint(path):
    info = path.stat()
    return (digest(path), info.st_mode, info.st_uid, info.st_gid)


def package_binary_digest(package):
    data = subprocess.run(["bsdtar", "-xOf", str(package), "usr/bin/hypr-rdp"],
                          check=True, capture_output=True).stdout
    assert data.startswith(b"\x7fELF")
    return hashlib.sha256(data).hexdigest()


def write_user(path, content):
    path.write_text(content)
    path.chmod(0o600)
    os.chown(path, 1100, 1100)


def record(name):
    CHECKS.append(name)
    print("PASS:", name, flush=True)


assert Path("/.dockerenv").exists()
assert Path("/proc/1/comm").read_text().strip() == "systemd"
assert os.geteuid() == 0 and not Path("/dev/nvidia0").exists()
assert not Path("/usr/lib/libnvidia-encode.so.1").exists()
old = OUTPUT / "old-upgrade/hypr-rdp-nvenc-0.1.7.nvenc1-1-x86_64.pkg.tar.zst"
found = list(OUTPUT.glob("hypr-rdp-nvenc-*-1-x86_64.pkg.tar.zst"))
assert len(found) == 1, found
new = found[0]
new_version = new.name.removeprefix("hypr-rdp-nvenc-").removesuffix("-x86_64.pkg.tar.zst")
old_provenance = json.loads((OUTPUT / "old-upgrade/OLD-BUILD-PROVENANCE.json").read_text())
new_provenance = json.loads((OUTPUT / "ARCH-BUILD-PROVENANCE.json").read_text())
assert old_provenance["runtime_source"] == "fd4b702b7e7454d98eda0d155a75c4b0c56c2092"
assert new_provenance["runtime_source"] != old_provenance["runtime_source"]
assert digest(old) == old_provenance["package_sha256"]
assert package_binary_digest(old) == old_provenance["binary_sha256"]
new_binary = package_binary_digest(new)
assert new_binary != old_provenance["binary_sha256"]
packages_before = run("pacman", "-Q").stdout
listeners_before = run("ss", "-H", "-lnt").stdout
assert not CONFIG_DIR.exists()

run("pacman", "-U", "--noconfirm", old)
assert run("pacman", "-Q", "hypr-rdp-nvenc").stdout.strip().endswith("0.1.7.nvenc1-1")
old_binary = digest(Path("/usr/bin/hypr-rdp"))
assert old_binary == old_provenance["binary_sha256"]
assert not CONFIG_DIR.exists()
user("install", "-d", "-m700", CONFIG_DIR)
user("hypr-rdp-tls", "--dns", "rdp-ci.invalid", "--ip", "127.0.0.1")
write_user(CONFIG_DIR / "password", "ci-upgrade-password\n")
write_user(CONFIG_DIR / "config.toml", "\n".join([
    'bind = "127.0.0.1:3389"', 'auth_mode = "configured"',
    'username = "ci-upgrade-user"',
    f'password_file = "{CONFIG_DIR}/password"',
    'output = "ci-existing-output"', 'h264_backend = "software"',
    'rate_control = "vbr"', 'audio_mode = "off"', '',
]))
pam = Path("/etc/pam.d/hypr-rdp")
pam.write_text(pam.read_text() + "# retained upgrade fixture\n")
saved = {p: fingerprint(p) for p in [pam, *(CONFIG_DIR / n for n in
         ("config.toml", "password", "cert.pem", "key.pem"))]}
user("hypr-rdp", "--check-config", "--config", CONFIG_DIR / "config.toml")
record("fresh exact nvenc1 recipe binary and private configured/TLS identity")

run("systemctl", "start", "user@1100.service")
deadline = time.monotonic() + 12
while not Path("/run/user/1100/bus").exists():
    assert time.monotonic() < deadline
    time.sleep(0.1)
assert ctl("is-enabled", UNIT, ok=False).stdout.strip() == "disabled"
hashes = {"old": old_binary}
transactions = []
for package, version in ((new, new_version), (old, "0.1.7.nvenc1-1"), (new, new_version)):
    ctl("stop", UNIT)
    assert prop("ActiveState") == "inactive"
    run("pacman", "-U", "--noconfirm", package)
    ctl("daemon-reload")
    assert run("pacman", "-Q", "hypr-rdp-nvenc").stdout.strip().endswith(version)
    assert saved == {p: fingerprint(p) for p in saved}
    assert not Path(str(pam) + ".pacnew").exists()
    for name in run("pacman", "-Qlq", "hypr-rdp-nvenc").stdout.splitlines():
        info = Path(name).lstat()
        assert info.st_uid == 0 and info.st_gid == 0 and info.st_mode & 0o6022 == 0, name
    user("hypr-rdp", "--check-config", "--config", CONFIG_DIR / "config.toml")
    current = digest(Path("/usr/bin/hypr-rdp"))
    if package == old:
        assert current == old_binary
    else:
        assert current == new_binary and current != old_binary
        if "new" in hashes:
            assert current == hashes["new"]
        hashes["new"] = current
    assert prop("ActiveState") == "inactive"
    assert ctl("is-enabled", UNIT, ok=False).stdout.strip() == "disabled"
    transactions.append({"version": version, "binary_sha256": current})
record("nvenc1 -> current -> nvenc1 -> current: distinct binaries, unchanged config/password/TLS/PAM and disabled unit")

# Do not override ExecStart or ExecCondition. Only disable restart for this
# negative CPU case, so a missing compositor cannot become a restart loop.
dropin = DESKTOP_HOME / ".config/systemd/user" / (UNIT + ".d") / "cpu-negative.conf"
user("install", "-d", "-m700", dropin.parent)
write_user(dropin, "[Service]\nRestart=no\n")
ctl("daemon-reload")
ctl("set-environment", "WAYLAND_DISPLAY=ci-wayland", "HYPRLAND_INSTANCE_SIGNATURE=ci-instance")
ctl("reset-failed")
ctl("start", UNIT, ok=False)
deadline = time.monotonic() + 12
while prop("ActiveState") not in ("failed", "inactive"):
    assert time.monotonic() < deadline
    time.sleep(0.1)
state = {key: prop(key) for key in ("ActiveState", "Result", "NRestarts", "ExecMainCode",
                                  "ExecMainStatus", "ExecStart", "ExecCondition", "Conditions")}
journal_deadline = time.monotonic() + 5
while True:
    journal = run("journalctl", "--no-pager", "_SYSTEMD_USER_UNIT=" + UNIT, "_UID=1100").stdout
    if "failed to initialize display capture" in journal and "failed to connect to Hyprland socket" in journal:
        break
    if time.monotonic() >= journal_deadline:
        raise AssertionError("expected compositor failure missing from unit journal: " + journal)
    time.sleep(0.1)
(OUTPUT / "upgrade-real-unit-journal.log").write_text(journal)
(OUTPUT / "upgrade-real-unit-state.json").write_text(json.dumps(state, indent=2) + "\n")
assert state["ActiveState"] == "failed" and state["Result"] == "exit-code", state
assert state["NRestarts"] == "0" and state["ExecMainStatus"] == "1", state
assert "/usr/bin/hypr-rdp" in state["ExecStart"] and "oracle" not in state["ExecStart"]
assert "code=exited" in state["ExecCondition"] and "status=0" in state["ExecCondition"], state
assert "failed to initialize display capture" in journal and "failed to connect to Hyprland socket" in journal, journal
assert saved == {p: fingerprint(p) for p in saved}
assert listeners_before == run("ss", "-H", "-lnt").stdout
assert not Path("/run/user/1100/hypr-rdp-nvenc").exists()
record("real packaged unit ExecCondition succeeds, actual ExecStart fails boundedly on absent Hyprland IPC; no restart/listener")

ctl("stop", UNIT, ok=False)
ctl("reset-failed")
dropin.unlink()
dropin.parent.rmdir()
ctl("unset-environment", "WAYLAND_DISPLAY", "HYPRLAND_INSTANCE_SIGNATURE")
run("pacman", "-R", "--noconfirm", "hypr-rdp-nvenc")
ctl("daemon-reload")
assert fingerprint(Path(str(pam) + ".pacsave")) == saved[pam]
assert all(fingerprint(p) == saved[p] for p in saved if p != pam)
for name in ("config.toml", "password", "cert.pem", "key.pem", ".tls.lock"):
    (CONFIG_DIR / name).unlink()
CONFIG_DIR.rmdir()
Path(str(pam) + ".pacsave").unlink()
assert run("pacman", "-Q").stdout == packages_before
assert listeners_before == run("ss", "-H", "-lnt").stdout
record("remove/pacsave and precise fixture cleanup restore package/listener baseline")

(OUTPUT / "upgrade-result.json").write_text(json.dumps({
    "status": "success", "checks": CHECKS, "transactions": transactions,
    "old_provenance": old_provenance, "new_provenance": new_provenance,
    "binary_sha256": hashes, "real_execstart": True,
    "real_execstart_outcome": "expected missing-compositor failure; not server readiness",
    "gpu": False, "desktop": False, "real_rdp_client": False,
}, indent=2) + "\n")
