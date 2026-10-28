"""Exercise publication with fake installers and a local GitHub CLI stand-in."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


WALLET = Path(__file__).resolve().parents[1]
VERSION = json.loads((WALLET / "package.json").read_text())["version"]
SHA = "a" * 40
TAG = f"wallet-v{VERSION}-build.42"
FAKE_GH = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
path = pathlib.Path(os.environ["FAKE_GH_STATE"])
state = json.loads(path.read_text())
args = sys.argv[1:]
state["calls"].append(args)
def finish(code=0):
    path.write_text(json.dumps(state))
    sys.exit(code)
def option(name):
    return args[args.index(name) + 1]
assert args[0] == "release", args
command = args[1]
if command == "view":
    if args[2] == "--repo":
        if not state.get("latest"):
            finish(1)
        print(state["latest"])
    else:
        release = state.get("release")
        if not release:
            finish(1)
        print(json.dumps(release))
elif command == "create":
    assert "--draft" in args and "--latest=false" in args
    assert "Windows 10/11 x64" in pathlib.Path(option("--notes-file")).read_text()
    state["release"] = {"isDraft": True, "targetCommitish": option("--target")}
elif command == "upload":
    assert state["release"]["isDraft"], "must never overwrite a published release"
    files = args[3:args.index("--repo")]
    assert len(files) == 7 and all(pathlib.Path(f).is_file() for f in files)
    if state.pop("fail_upload", False):
        finish(1)
elif command == "edit":
    assert "--draft=false" in args
    state["release"]["isDraft"] = False
    state["made_latest"] = "--latest=true" in args
else:
    raise AssertionError(args)
finish()
'''


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.packages = self.root / "packages"
        self.packages.mkdir()
        self.state = self.root / "gh-state.json"
        self.state.write_text(json.dumps({"calls": []}))
        cli = self.root / "gh"
        cli.write_text(FAKE_GH)
        cli.chmod(0o755)
        self.env = {
            **os.environ,
            "PATH": f"{self.root}:{os.environ['PATH']}",
            "FAKE_GH_STATE": str(self.state),
            "GITHUB_REF": "refs/heads/main",
            "GITHUB_EVENT_NAME": "push",
            "GITHUB_REPOSITORY": "mmx-network/mmx-rust-node",
            "GITHUB_RUN_NUMBER": "42",
            "GITHUB_RUN_ID": "1234",
            "GITHUB_SHA": SHA,
            "GITHUB_STEP_SUMMARY": str(self.root / "summary"),
        }
        self.files = [
            f"mmx-wallet_{VERSION}_amd64.deb",
            f"mmx-wallet_{VERSION}_amd64.AppImage",
            f"mmx-wallet_{VERSION}_macos_universal.dmg",
            f"mmx-wallet_{VERSION}_x64-setup.exe",
        ]
        for name in self.files:
            (self.packages / name).write_text(f"fake installer {name}\n")
        for suffix, names in [
            ("linux-amd64", self.files[:2]),
            ("macos-universal", self.files[2:3]),
            ("windows-x64", self.files[3:]),
        ]:
            checksums = "".join(
                f"{hashlib.sha256((self.packages / name).read_bytes()).hexdigest()}  {name}\n"
                for name in names
            )
            (self.packages / f"sha256sums-{suffix}").write_text(checksums)

    def run_release(self, success=True):
        result = subprocess.run(
            ["bash", str(WALLET / "scripts/publish-release.sh"), str(self.packages)],
            env=self.env, text=True, capture_output=True,
        )
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return json.loads(self.state.read_text())

    def seed_state(self, **values):
        self.state.write_text(json.dumps({"calls": [], **values}))

    def test_publishes_complete_set_and_rerun_leaves_it_unchanged(self):
        state = self.run_release()
        self.assertFalse(state["release"]["isDraft"])
        self.assertTrue(state["made_latest"])
        self.assertIn(TAG, (self.root / "summary").read_text())
        state = self.run_release()
        self.assertEqual([call[1] for call in state["calls"]].count("upload"), 1)
        self.assertEqual([call[1] for call in state["calls"]].count("edit"), 1)

    def test_failed_upload_stays_draft_and_retry_completes_it(self):
        self.seed_state(fail_upload=True)
        state = self.run_release(success=False)
        self.assertTrue(state["release"]["isDraft"])
        state = self.run_release()
        self.assertFalse(state["release"]["isDraft"])
        self.assertEqual([call[1] for call in state["calls"]].count("create"), 1)

    def test_older_build_does_not_replace_latest(self):
        self.seed_state(latest=f"wallet-v{VERSION}-build.43")
        self.assertFalse(self.run_release()["made_latest"])

    def test_refuses_release_for_another_commit(self):
        self.seed_state(release={"isDraft": True, "targetCommitish": "b" * 40})
        state = self.run_release(success=False)
        self.assertEqual([call[1] for call in state["calls"]], ["view"])

    def test_missing_installer_prevents_any_github_calls(self):
        (self.packages / self.files[2]).unlink()
        self.assertEqual(self.run_release(success=False)["calls"], [])

    def test_corrupt_installer_prevents_any_github_calls(self):
        (self.packages / self.files[1]).write_text("corrupted")
        self.assertEqual(self.run_release(success=False)["calls"], [])

    def test_incomplete_checksum_manifest_prevents_any_github_calls(self):
        manifest = self.packages / "sha256sums-linux-amd64"
        manifest.write_text(manifest.read_text().splitlines()[0] + "\n")
        self.assertEqual(self.run_release(success=False)["calls"], [])

    def test_non_main_and_pull_requests_cannot_publish(self):
        for ref, event in [("refs/heads/feature", "push"), ("refs/heads/main", "pull_request")]:
            with self.subTest(ref=ref, event=event):
                self.env.update(GITHUB_REF=ref, GITHUB_EVENT_NAME=event)
                self.assertEqual(self.run_release(success=False)["calls"], [])


if __name__ == "__main__":
    unittest.main()
