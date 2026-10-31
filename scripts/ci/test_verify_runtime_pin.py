#!/usr/bin/env python3
"""Negative/positive cases for verify-runtime-pin.py on throwaway git repositories."""
import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("pin", Path(__file__).with_name("verify-runtime-pin.py"))
pin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pin)


class RuntimePin(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.git("init", "-q")
        self.write("src/main.rs", "fn main() {}\n")
        self.write("README.md", "runtime readme\n")
        self.write("pkg/pam/hypr-rdp.arch", "auth include system-auth\n")
        self.write("protocols/keyboard.xml", "<protocol/>\n")
        self.runtime = self.commit("runtime")
        self.pin(self.runtime)

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.root), "-c", "user.name=t", "-c", "user.email=t@t", *args],
                              check=True, capture_output=True, text=True).stdout.strip()

    def write(self, path, text):
        (self.root / path).parent.mkdir(parents=True, exist_ok=True)
        (self.root / path).write_text(text)

    def commit(self, message):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)
        return self.git("rev-parse", "HEAD")

    def pin(self, commit, doc=None):
        self.write("pkg/arch/PKGBUILD", f"_commit={commit}\n")
        self.write("pkg/arch/arch-package.md", f"Runtime pin `{doc or commit}`.\n")
        return self.commit("package")

    def rejects(self):
        with self.assertRaises(SystemExit):
            pin.check(self.root)

    def test_packaging_recipe_workflow_and_ci_changes_are_accepted(self):
        for path in ("pkg/arch/hypr-rdp-nvenc.service", ".github/workflows/arch-package.yml",
                     "scripts/ci/arch/lifecycle.py", "ci/arch.json"):
            self.write(path, "changed\n")
        self.commit("packaging only")
        self.assertEqual(pin.check(self.root), self.runtime)

    def test_runtime_source_change_without_pin_bump_is_rejected(self):
        self.write("src/main.rs", "fn main() { fix(); }\n")
        self.commit("runtime fix on the packaging branch")
        self.rejects()

    def test_installed_runtime_files_outside_src_are_runtime_inputs(self):
        for path in ("README.md", "pkg/pam/hypr-rdp.arch", "protocols/keyboard.xml", "Cargo.lock"):
            with self.subTest(path=path):
                self.write(path, "drift\n")
                self.commit(f"drift {path}")
                self.rejects()
                self.git("reset", "-q", "--hard", "HEAD~1")

    def test_bumping_the_pin_to_a_commit_with_that_tree_is_accepted(self):
        self.write("src/main.rs", "fn main() { fix(); }\n")
        fixed = self.commit("runtime fix")
        self.pin(fixed)
        self.assertEqual(pin.check(self.root), fixed)

    def test_unknown_unrelated_or_stale_documented_pins_are_rejected(self):
        self.pin("0" * 40)
        self.rejects()
        self.pin(self.runtime, doc="1" * 40)
        self.rejects()


if __name__ == "__main__":
    unittest.main()
