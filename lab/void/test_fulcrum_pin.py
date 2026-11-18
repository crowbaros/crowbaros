"""The Void lab builds exactly the Fulcrum commit it is told to, and nothing from a working copy."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import compositor_guest as guest

A = "a" * 40
B = "b" * 40


def git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), "-c", "user.name=Lab", "-c", "user.email=lab@example.invalid",
                           *args], check=True, capture_output=True, text=True).stdout.strip()


class Pin(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.pin = Path(self.temp.name) / "fulcrum.json"
        self.pin.write_text(json.dumps({"repository": "https://example.invalid/fulcrum.git", "revision": A}))
        patcher = patch.object(guest, "PIN", self.pin)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.addCleanup(self.temp.cleanup)

    def test_the_pin_decides_by_default(self):
        pin = guest.fulcrum_pin({})
        self.assertEqual((pin["revision"], pin["pinned"]), (A, True))
        self.assertEqual(pin["repository"], "https://example.invalid/fulcrum.git")

    def test_an_unpinned_commit_is_named_as_one(self):
        pin = guest.fulcrum_pin({"FULCRUM_REVISION": B})
        self.assertEqual((pin["revision"], pin["pinned"], pin["pinned_revision"]), (B, False, A))

    def test_a_local_clone_is_named_by_its_full_path(self):
        pin = guest.fulcrum_pin({"FULCRUM_REPOSITORY": self.temp.name})
        self.assertEqual(pin["repository"], str(Path(self.temp.name).resolve()))

    def test_a_branch_or_short_commit_is_refused(self):
        for revision in ("main", A[:12], A.upper()):
            with self.subTest(revision=revision), self.assertRaises(SystemExit):
                guest.fulcrum_pin({"FULCRUM_REVISION": revision})

    def test_a_pin_that_is_not_a_full_commit_is_refused(self):
        self.pin.write_text(json.dumps({"repository": "x", "revision": "main"}))
        with self.assertRaises(SystemExit):
            guest.fulcrum_pin({"FULCRUM_REVISION": A})


class CommittedPin(unittest.TestCase):
    def test_the_committed_pin_names_a_full_commit(self):
        pin = json.loads(guest.PIN.read_text())
        self.assertRegex(pin["revision"], r"^[0-9a-f]{40}$")
        self.assertTrue(pin["repository"])


class Export(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name)
        self.repo, self.git_dir, self.trees = base / "fulcrum", base / "lab/fulcrum.git", base / "lab/fulcrum"
        self.repo.mkdir()
        git(self.repo, "init", "-q", "-b", "main")
        (self.repo / "Cargo.toml").write_text("committed\n")
        git(self.repo, "add", "Cargo.toml")
        git(self.repo, "commit", "-q", "-m", "first")
        self.revision = git(self.repo, "rev-parse", "HEAD")

    def export(self, revision):
        pin = {"revision": revision, "repository": str(self.repo)}
        with patch("builtins.print"):
            return guest.fulcrum_source(pin, self.git_dir, self.trees)

    def test_the_commit_is_built_and_the_working_copy_is_not(self):
        (self.repo / "Cargo.toml").write_text("changed, not committed\n")
        (self.repo / "untracked.rs").write_text("")
        tree = self.export(self.revision)
        self.assertEqual(tree, self.trees / self.revision)
        self.assertEqual((tree / "Cargo.toml").read_text(), "committed\n")
        self.assertFalse((tree / "untracked.rs").exists())
        self.assertEqual(sorted(p.name for p in self.trees.iterdir()), [self.revision])

    def test_a_commit_off_every_branch_is_fetched(self):
        (self.repo / "Cargo.toml").write_text("second\n")
        git(self.repo, "commit", "-q", "-am", "second")
        second = git(self.repo, "rev-parse", "HEAD")
        git(self.repo, "reset", "-q", "--hard", self.revision)
        self.assertEqual((self.export(second) / "Cargo.toml").read_text(), "second\n")

    def test_an_exported_tree_is_reused(self):
        tree = self.export(self.revision)
        (tree / "marker").write_text("")
        self.assertTrue((self.export(self.revision) / "marker").exists())

    def test_a_commit_the_repository_lacks_is_refused(self):
        with self.assertRaises(SystemExit):
            self.export("0" * 40)
        self.assertFalse((self.trees / ("0" * 40)).exists())


if __name__ == "__main__":
    unittest.main()
