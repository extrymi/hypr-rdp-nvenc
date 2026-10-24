#!/usr/bin/env python3
"""Actual pacman/systemd/PAM lifecycle in a disposable booted Arch container."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

UNIT = "hypr-rdp-nvenc.service"
HOME = Path("/home/tester")
CONFIG = HOME / ".config/hypr-rdp"
OUT = Path("/out")
V1 = OUT / "hypr-rdp-nvenc-0.1.7.nvenc1-1-x86_64.pkg.tar.zst"
V2 = OUT / "hypr-rdp-nvenc-0.1.7.nvenc1-2-x86_64.pkg.tar.zst"
CHECKS = []


def run(*args, ok=True, data=None, timeout=30):
    result = subprocess.run(list(map(str, args)), input=data, text=True,
                            capture_output=True, timeout=timeout)
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


def wait_for(predicate, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.1)
    raise AssertionError("condition did not become true before deadline")


def record(name):
    CHECKS.append(name)
    print(f"PASS: {name}", flush=True)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_user(path, text, mode=0o600):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(mode)
    run("chown", "-R", "tester:tester", HOME / ".config")


assert Path("/.dockerenv").exists()
assert Path("/proc/1/comm").read_text().strip() == "systemd"
assert os.geteuid() == 0 and not Path("/dev/nvidia0").exists()
sentinels = {
    Path("/etc/ssh/sshd_config"): "# existing SSH fixture\nPort 22\n",
    Path("/etc/nftables.conf"): "# existing firewall fixture\n",
    HOME / ".config/hypr/hyprland.conf": "# existing desktop fixture\n",
    HOME / ".config/omarchy/test-sentinel": "existing Omarchy fixture\n",
    Path("/etc/hypr-rdp-ci-vpn-sentinel"): "existing VPN fixture\n",
}
for path, text in sentinels.items():
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
snapshot = {str(path): digest(path) for path in sentinels}
packages_before = run("pacman", "-Q").stdout
listeners_before = run("ss", "-H", "-lnt").stdout
run("pacman", "-U", "--noconfirm", V1)
run("pacman", "-Qkk", "hypr-rdp-nvenc")
assert not CONFIG.exists()
assert listeners_before == run("ss", "-H", "-lnt").stdout
run("systemctl", "start", "user@1100.service")
wait_for(lambda: Path("/run/user/1100/bus").exists())
assert ctl("is-enabled", UNIT, ok=False).stdout.strip() == "disabled"
assert prop("ActiveState") == "inactive"
user("systemd-analyze", "--user", "verify", "/usr/lib/systemd/user/" + UNIT)
record("install: managed files, no config/TLS/activation/listener side effects")

ctl("unset-environment", "WAYLAND_DISPLAY", "HYPRLAND_INSTANCE_SIGNATURE")
ctl("start", UNIT)
assert prop("ActiveState") == "inactive" and prop("ConditionResult") == "no"
record("unit refuses a missing graphical-session environment")

# Retain the actual packaged preflight; only ExecStart is a CPU fixture.
dropin = HOME / ".config/systemd/user" / (UNIT + ".d") / "ci.conf"
write_user(dropin, "[Service]\nExecStart=\nExecStart=/usr/local/libexec/hypr-rdp-ci/service-oracle\nRestartSec=100ms\nTimeoutStopSec=2\n")
ctl("daemon-reload")
ctl("set-environment", "WAYLAND_DISPLAY=ci-wayland", "HYPRLAND_INSTANCE_SIGNATURE=ci-instance",
    "SSLKEYLOGFILE=/home/tester/should-not-exist.keys")
ctl("start", UNIT)
time.sleep(0.5)
assert prop("ActiveState") == "inactive" and prop("NRestarts") == "0"
assert not (CONFIG / "oracle.json").exists()
record("missing configuration: production ExecCondition skips without retries")

user("install", "-d", "-m700", CONFIG)
user("hypr-rdp-tls", "--dns", "rdp-ci.invalid", "--ip", "127.0.0.1")
assert (CONFIG / "key.pem").stat().st_mode & 0o777 == 0o600
run("openssl", "x509", "-in", CONFIG / "cert.pem", "-noout", "-checkhost", "rdp-ci.invalid")
run("openssl", "x509", "-in", CONFIG / "cert.pem", "-noout", "-checkip", "127.0.0.1")
identity = {name: digest(CONFIG / name) for name in ("cert.pem", "key.pem")}
assert user("hypr-rdp-tls", "--dns", "different.invalid", ok=False).returncode
assert identity == {name: digest(CONFIG / name) for name in identity}
record("explicit TLS provisioning: SANs, key permissions, saved-identity refusal")

fixtures = HOME / ".config/hypr-rdp-tls-fixtures"
user("install", "-d", "-m700", fixtures)
for name in ("partial", "symlink", "fifo", "busy", "invalid"):
    user("install", "-d", "-m700", fixtures / name)
write_user(fixtures / "partial/key.pem", "partial fixture\n")
assert user("hypr-rdp-tls", "--directory", fixtures / "partial", "--dns", "ci.invalid", ok=False).returncode
(fixtures / "symlink/cert.pem").symlink_to(fixtures / "nonexistent")
assert user("hypr-rdp-tls", "--directory", fixtures / "symlink", "--dns", "ci.invalid", ok=False).returncode
os.mkfifo(fixtures / "fifo/.tls.lock", 0o600)
assert user("hypr-rdp-tls", "--directory", fixtures / "fifo", "--dns", "ci.invalid", ok=False, timeout=2).returncode
write_user(fixtures / "busy/.tls.lock", "")
with (fixtures / "busy/.tls.lock").open("r+") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    assert user("hypr-rdp-tls", "--directory", fixtures / "busy", "--dns", "ci.invalid", ok=False, timeout=2).returncode
assert user("hypr-rdp-tls", "--directory", fixtures / "invalid", "--dns", "invalid\nname", ok=False).returncode
assert user("hypr-rdp-tls", "--directory", fixtures / "invalid", "--ip", "999.999.1.1", ok=False).returncode
assert not (fixtures / "invalid/key.pem").exists()
record("TLS helper: partial/dangling/FIFO/busy/injected/invalid SAN inputs fail boundedly")

template = Path("/usr/share/doc/hypr-rdp-nvenc/config.example.toml").read_text()
template = template.replace("REPLACE_WITH_RDP_USERNAME", "ci-rdp-user")
template = template.replace("/home/REPLACE_WITH_DESKTOP_USER", str(HOME))
template = template.replace("REPLACE_WITH_EXISTING_OUTPUT", "ci-existing-output")
write_user(CONFIG / "password", "ci-rdp-password\n")
write_user(CONFIG / "config.toml", template)
user("hypr-rdp", "--check-config", "--config", CONFIG / "config.toml")
assert not Path("/usr/lib/libnvidia-encode.so.1").exists()
record("packaged native build validates config offline without NVIDIA or compositor")

(CONFIG / "key.pem").chmod(0o644)
ctl("start", UNIT)
assert prop("ActiveState") == "inactive" and prop("NRestarts") == "0"
(CONFIG / "key.pem").chmod(0o600)
record("bad TLS permissions: production preflight skips unit without retries")

ctl("start", "graphical-session.target")
ctl("start", UNIT)
wait_for(lambda: (CONFIG / "oracle.json").exists())
oracle = json.loads((CONFIG / "oracle.json").read_text())
assert oracle["umask"] == 0o077 and not oracle["keylog_present"]
assert oracle["wayland"] == "ci-wayland" and oracle["instance"] == "ci-instance"
assert prop("NoNewPrivileges") == "no"
ctl("stop", "graphical-session.target")
wait_for(lambda: not Path(f'/proc/{oracle["pid"]}').exists() and
         not Path(f'/proc/{oracle["child"]}').exists())
assert prop("ActiveState") == "inactive" and not (HOME / "should-not-exist.keys").exists()
record("real user manager: environment, private umask, no keylog, target stop/reap")

(CONFIG / "oracle.json").unlink()
write_user(CONFIG / "fail-oracle", "fail\n")
ctl("start", UNIT, ok=False)
wait_for(lambda: prop("ActiveState") == "failed")
assert 1 <= int(prop("NRestarts")) <= 3 and prop("Result") == "start-limit-hit"
(CONFIG / "fail-oracle").unlink()
ctl("reset-failed", UNIT)
record("runtime failures hit the bounded systemd start limit")

probe = "/usr/local/libexec/hypr-rdp-ci/pam-probe"
user(probe, "tester", data="disposable-ci-password\n")
assert user(probe, "tester", data="incorrect-ci-password\n", ok=False).returncode
record("packaged PAM system-auth: desktop UID admits correct and refuses wrong password")

config_hashes = {name: digest(CONFIG / name) for name in ("config.toml", "password", "cert.pem", "key.pem")}
pam_path = Path("/etc/pam.d/hypr-rdp")
pam_path.write_text(pam_path.read_text() + "# administrator fixture retained by pacman\n")
pam_hash = digest(pam_path)
for package, version in ((V2, "0.1.7.nvenc1-2"), (V1, "0.1.7.nvenc1-1")):
    run("pacman", "-U", "--noconfirm", package)
    assert run("pacman", "-Q", "hypr-rdp-nvenc").stdout.strip().endswith(version)
    assert digest(pam_path) == pam_hash
    assert config_hashes == {name: digest(CONFIG / name) for name in config_hashes}
    user("hypr-rdp", "--check-config", "--config", CONFIG / "config.toml")
    assert prop("ActiveState") == "inactive"
record("upgrade/downgrade preserve edited PAM, config/password/TLS and inactive unit")

ctl("disable", "--now", UNIT)
run("pacman", "-R", "--noconfirm", "hypr-rdp-nvenc")
ctl("daemon-reload")
for path in ("/usr/bin/hypr-rdp", "/usr/bin/hypr-rdp-tls", "/usr/lib/systemd/user/" + UNIT,
             "/usr/share/doc/hypr-rdp-nvenc", "/usr/share/licenses/hypr-rdp-nvenc"):
    assert not Path(path).exists(), path
assert not pam_path.exists() and digest(Path(str(pam_path) + ".pacsave")) == pam_hash
assert config_hashes == {name: digest(CONFIG / name) for name in config_hashes}
assert run("pacman", "-Q").stdout == packages_before
record("remove: payload gone, edited PAM saved, user identity and shared dependencies retained")

# Only known disposable fixture files; no recursive production config purge.
for path in CONFIG.iterdir():
    assert path.name in {"config.toml", "password", "cert.pem", "key.pem", ".tls.lock"}
    path.unlink()
CONFIG.rmdir()
dropin.unlink()
dropin.parent.rmdir()
Path(str(pam_path) + ".pacsave").unlink()
shutil.rmtree(fixtures)  # Entire tree belongs to this test.
assert not Path("/run/user/1100/hypr-rdp-nvenc").exists()
assert snapshot == {str(path): digest(path) for path in sentinels}
assert listeners_before == run("ss", "-H", "-lnt").stdout
record("full fixture cleanup preserves desktop/SSH/VPN/firewall files and listeners")

(OUT / "arch-lifecycle-result.json").write_text(json.dumps({
    "status": "success", "checks": CHECKS,
    "real_pacman": True, "real_systemd_user_manager": True, "real_pam_unix": True,
    "execstart": "CPU oracle; actual packaged binary ExecCondition retained",
    "gpu": False, "desktop": False, "real_rdp_client": False,
}, indent=2) + "\n")
