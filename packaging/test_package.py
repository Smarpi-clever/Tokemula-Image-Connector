"""Packager checks use test signing material and ordinary payload files only."""
import base64
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).parent))
import package


class PackageTests(unittest.TestCase):
    def test_release_name_and_ordinary_archive_payload(self):
        self.assertEqual(package.archive_name("0.1.0-d2a.1", "aarch64-apple-darwin"),
                         "tokemula-image-connector-0.1.0-d2a.1-macos-arm64.tar.gz")
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            stage = root / "stage"
            stage.mkdir()
            (stage / "tokemula-image").write_bytes(b"synthetic executable")
            (stage / "LICENSE").write_text("synthetic notice\n")
            first, second = root / "first.tar.gz", root / "second.tar.gz"
            package.write_archive(stage, first)
            package.write_archive(stage, second)
            self.assertEqual(first.read_bytes(), second.read_bytes())
            with tarfile.open(first, "r:gz") as archive:
                entries = archive.getmembers()
                self.assertEqual([e.name for e in entries], ["LICENSE", "tokemula-image"])
                self.assertEqual(entries[1].mode, 0o755)
                self.assertTrue(all(e.isfile() for e in entries))
            with self.assertRaises(ValueError):
                package.write_archive(stage, first)
            (stage / "link").symlink_to("LICENSE")
            with self.assertRaises(ValueError):
                package.write_archive(stage, root / "unsafe.tar.gz")

    def test_source_snapshot_excludes_tool_outputs_and_dotenv(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "src").mkdir()
            (root / "src/main.rs").write_text("fn main() {}")
            (root / ".env").write_text("SYNTHETIC_SECRET=not-release-input")
            (root / "target").mkdir()
            (root / "target/test-output").write_text("rebuildable")
            snapshot = package.source_files(root)
            self.assertEqual(list(snapshot), [Path("src/main.rs")])

    def test_release_inputs_include_runtime_assets_and_package_not_general_docs(self):
        files = {Path(name): b"sample" for name in ["connector/src/distribution.rs", "skills/tokemula-image/SKILL.md",
                "packaging/QUICKSTART.md", "packaging/package.py", "README.md", "docs/MANAGEMENT.md"]}
        selected = package.release_inputs(files)
        self.assertEqual(len(selected), 4)
        self.assertNotIn(Path("docs/MANAGEMENT.md"), selected)

    def test_ed25519_exact_bytes_signature_and_no_private_payload(self):
        openssl = package.openssl_path()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            key = root / "test-key.pem"
            package.generate_test_key(key, openssl)
            public = package.public_key(key, openssl)
            self.assertEqual(len(base64.b64decode(public)), 32)
            source = root / "release.json"
            source.write_bytes(package.json_bytes({"version": "0.1.0-d2a.1"}))
            signature = root / "signature"
            signature.write_bytes(base64.b64decode(package.sign(source.read_bytes(), key, openssl)))
            pem = root / "test-public.pem"
            pem.write_bytes(package.run([openssl, "pkey", "-in", str(key), "-pubout"]))
            package.run([openssl, "pkeyutl", "-verify", "-rawin", "-pubin", "-inkey", str(pem),
                         "-sigfile", str(signature), "-in", str(source)])
            self.assertEqual(key.stat().st_mode & 0o777, 0o600)
            with self.assertRaises(ValueError):
                package.generate_test_key(key, openssl)


if __name__ == "__main__":
    unittest.main()
