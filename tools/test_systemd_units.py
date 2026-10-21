"""Check systemd argument handling without installing units or starting daemons."""
import argparse
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "mmx_systemd", Path(__file__).resolve().parents[1] / "scripts/systemd/install.py"
)
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class UnitTests(unittest.TestCase):
    def arguments(self, directory, command):
        return argparse.Namespace(
            name="mmx-rust-harvester", working_directory=directory, command=command,
            description="Test MMX", after=[], wants=[], nice=10, stop_timeout=180,
            environment=[],
            require_mount=[],
            require_free=[],
        )

    def test_arguments_remain_literal_in_systemd(self):
        with tempfile.TemporaryDirectory(prefix="mmx $ %-units-") as directory:
            args = self.arguments(directory, ["--", "/usr/bin/printf", "%s", 'a b"c\\d', "$HOME", "${SECRET}", "--node"])
            args.environment = ["MMX_HOME=a b$HOME%h"]
            content = installer.render(args)
            self.assertIn('"%%s" "a b\\"c\\\\d" "$$HOME" "$${SECRET}"', content)
            self.assertIn('Environment="MMX_HOME=a b$HOME%%h"', content)
            self.assertIn("Restart=always", content)
            unit = Path(directory) / "mmx-rust-test.service"
            unit.write_text(content)
            env = dict(os.environ, XDG_RUNTIME_DIR=directory)
            result = subprocess.run(["systemd-analyze", "--user", "verify", str(unit)], capture_output=True, text=True, env=env)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_storage_checks_need_no_ledger_reads(self):
        with tempfile.TemporaryDirectory() as directory:
            args = self.arguments(directory, ["/usr/bin/true"])
            args.require_mount = [directory]
            args.require_free = [(directory, "0")]
            content = installer.render(args)
            self.assertLess(content.index('"/usr/bin/mountpoint"'), content.index("check_storage.py"))
            helper = Path(installer.__file__).with_name("check_storage.py")
            self.assertEqual(subprocess.run(["python3", str(helper), directory, "0"], capture_output=True).returncode, 0)
            self.assertNotEqual(subprocess.run(["python3", str(helper), directory, str(1 << 100)], capture_output=True).returncode, 0)

    def test_rejects_unit_and_line_injection(self):
        for name in ["../mmx-rust-test", "mmx-rust-test\nExecStart=x", "another-service"]:
            with self.assertRaises(ValueError):
                installer.unit_name(name)
        with tempfile.TemporaryDirectory() as directory:
            args = self.arguments(directory, ["/usr/bin/true", "x\nExecStart=/bin/false"])
            with self.assertRaises(ValueError):
                installer.render(args)
            args.command = ["relative-program"]
            with self.assertRaises(ValueError):
                installer.render(args)
            args.command = ["/usr/bin/true"]
            args.environment = ["NOT-A-NAME=value"]
            with self.assertRaises(ValueError):
                installer.render(args)


if __name__ == "__main__":
    unittest.main()
