"""prepare-gpui.py: GPUI's source is made again only when what it is made from,
or the source itself, has changed. Each test prepares a small crate of its own."""
import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("prepare_gpui", Path(__file__).with_name("prepare-gpui.py"))
prepare_gpui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare_gpui)

PATCH = """--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1 @@
-pub fn frames() -> u32 { 60 }
+pub fn frames() -> u32 { 0 }
"""


class Prepare(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        crate = io.BytesIO()
        with tarfile.open(fileobj=crate, mode="w:gz") as tar:
            for name, text in {"gpui-0.2.2/src/lib.rs": "pub fn frames() -> u32 { 60 }\n",
                               "gpui-0.2.2/Cargo.toml": "[package]\nname = \"gpui\"\n"}.items():
                info = tarfile.TarInfo(name)
                info.size = len(text)
                tar.addfile(info, io.BytesIO(text.encode()))
        (self.root / ".upstream-cache").mkdir()
        (self.root / ".upstream-cache/gpui-0.2.2.crate").write_bytes(crate.getvalue())
        self.sha = hashlib.sha256(crate.getvalue()).hexdigest()
        (self.root / "patches").mkdir()
        (self.root / "patches/still.patch").write_text(PATCH)
        self.source = self.root / "gpui-source/src/lib.rs"

    def prepare(self):
        return prepare_gpui.prepare(self.root, url="http://example.org/unused", sha=self.sha,
                                    patches=["still.patch"])

    def test_makes_the_patched_source(self):
        self.assertTrue(self.prepare())
        self.assertEqual(self.source.read_text(), "pub fn frames() -> u32 { 0 }\n")

    def test_leaves_a_source_made_from_the_same_patches(self):
        self.prepare()
        made = self.source.stat().st_mtime_ns
        self.assertFalse(self.prepare())
        self.assertEqual(self.source.stat().st_mtime_ns, made)

    def test_makes_it_again_when_a_patch_changes(self):
        self.prepare()
        (self.root / "patches/still.patch").write_text(PATCH.replace("{ 0 }", "{ 1 }"))
        self.assertTrue(self.prepare())
        self.assertEqual(self.source.read_text(), "pub fn frames() -> u32 { 1 }\n")

    def test_makes_it_again_when_the_source_was_edited(self):
        self.prepare()
        self.source.write_text("pub fn frames() -> u32 { 7 }\n")
        self.assertTrue(self.prepare())
        self.assertEqual(self.source.read_text(), "pub fn frames() -> u32 { 0 }\n")

    def test_makes_it_again_when_a_file_was_added(self):
        self.prepare()
        (self.root / "gpui-source/src/extra.rs").write_text("\n")
        self.assertTrue(self.prepare())
        self.assertFalse((self.root / "gpui-source/src/extra.rs").exists())

    def test_makes_it_again_over_a_source_from_before_the_recipe(self):
        self.prepare()
        (self.root / "gpui-source" / prepare_gpui.MARKER).write_text(self.sha + "\n")
        self.assertTrue(self.prepare())

    def test_refuses_a_directory_it_did_not_make(self):
        (self.root / "gpui-source").mkdir()
        with self.assertRaises(SystemExit):
            self.prepare()


if __name__ == "__main__":
    unittest.main()
