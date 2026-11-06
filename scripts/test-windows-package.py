import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location("package_windows", Path(__file__).with_name("package-windows-portable.py"))
packaging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packaging)

class PortablePackageTest(unittest.TestCase):
    def test_zip_contains_the_same_executable_and_no_profile_or_symbols(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "torto.exe"
            binary.write_bytes(b"MZ" + b"release fixture" * 10)
            archive = packaging.package(binary, root / "dist", "0.10.0")
            with zipfile.ZipFile(archive) as bundle:
                self.assertEqual(set(bundle.namelist()), {"torto.exe", "torto-portable.json", "LICENSE", "README.txt"})
                self.assertEqual(bundle.read("torto.exe"), binary.read_bytes())
                self.assertEqual(bundle.read("torto-portable.json").strip(), b'{"version":1}')
            self.assertEqual(archive.with_name(archive.name + ".sha256").read_text().split()[0], hashlib.sha256(archive.read_bytes()).hexdigest())

if __name__ == "__main__":
    unittest.main()
