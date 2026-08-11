import sys
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO_ROOT / "python"))

from furina_tools.scan import scan  # noqa: E402


class ScanTest(unittest.TestCase):
    def test_python_project(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp)
            (p / "pyproject.toml").write_text("[project]\nname='x'\n", encoding="utf-8")
            (p / "app.py").write_text("print(1)\n", encoding="utf-8")
            (p / "tests").mkdir()
            result = scan(p)
            self.assertEqual(result["project_type"], "python")
            self.assertEqual(result["test_command"], "python -m pytest")
            self.assertIn("pyproject.toml", result["manifests"])
            self.assertEqual(result["total_files"], 2)

    def test_node_project(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp)
            (p / "package.json").write_text("{\"name\": \"x\"}\n", encoding="utf-8")
            result = scan(p)
            self.assertEqual(result["project_type"], "node")
            self.assertEqual(result["test_command"], "npm test")

    def test_unknown_empty_dir(self):
        with tempfile.TemporaryDirectory() as tmp:
            result = scan(Path(tmp))
            self.assertEqual(result["project_type"], "unknown")
            self.assertEqual(result["test_command"], "")

    def test_skips_junk_dirs(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp)
            (p / "src.py").write_text("x", encoding="utf-8")
            (p / "__pycache__").mkdir()
            (p / "__pycache__" / "a.pyc").write_bytes(b"\x00")
            result = scan(p)
            self.assertEqual(result["total_files"], 1)


if __name__ == "__main__":
    unittest.main()
