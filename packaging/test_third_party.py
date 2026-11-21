import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("third_party", Path(__file__).with_name("third_party.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class LicenseTests(unittest.TestCase):
    def test_alternatives_and_mandatory_notices(self):
        self.assertEqual(module.selected_license("Apache-2.0 OR MIT"), "MIT")
        self.assertEqual(module.selected_license("MIT/Apache-2.0"), "MIT")
        self.assertEqual(module.selected_license("(MIT OR Apache-2.0) AND Unicode-3.0"), "MIT AND Unicode-3.0")
        self.assertEqual(module.selected_license("ISC AND (Apache-2.0 OR ISC) AND MIT AND BSD-3-Clause"), "ISC AND MIT AND BSD-3-Clause")
        self.assertEqual(module.selected_license("Apache-2.0 WITH LLVM-exception OR MIT"), "MIT")
        for invalid in ["Unknown-1.0", "MIT AND", "MIT OR (Apache-2.0", "MIT/Proprietary", "MIT AND OR Apache-2.0"]:
            with self.assertRaises(ValueError):
                module.selected_license(invalid)

    def test_nested_notices_and_missing_text_fail(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            with self.assertRaises(ValueError):
                module._legal_files(root, None)
            (root / "LICENSE").write_text("Synthetic license text")
            (root / "vendor").mkdir()
            (root / "vendor/NOTICE").write_text("Synthetic bundled copyright")
            self.assertEqual(len(module._legal_files(root, None)), 2)
            with self.assertRaises(ValueError):
                module._legal_files(root, "../external")
            (root / "LICENSE-link").symlink_to(root / "LICENSE")
            with self.assertRaises(ValueError):
                module._legal_files(root, None)


if __name__ == "__main__":
    unittest.main()
