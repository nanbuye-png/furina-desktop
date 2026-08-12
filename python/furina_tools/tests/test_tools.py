import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO_ROOT / "python"))

from furina_tools.tools import (  # noqa: E402
    ToolError, _relative_to_workspace, create_file, diff_file, file_hash, read_file,
    resolve_path, run_command, search, write_file,
)


class FsToolTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.ws = Path(self._tmp.name)
        (self.ws / "hello.txt").write_text("hello world\nline two\n", encoding="utf-8")

    def tearDown(self):
        self._tmp.cleanup()

    def test_read_write_roundtrip(self):
        write_file(self.ws, {"path": "sub/new.txt", "content": "abc"})
        out = read_file(self.ws, {"path": "sub/new.txt"})
        self.assertEqual(out["content"], "abc")
        self.assertFalse(out["truncated"])

    def test_read_outside_workspace_requires_escape_flag(self):
        outside = self.ws.parent / "outside.txt"
        outside.write_text("secret", encoding="utf-8")
        with self.assertRaises(ToolError):
            read_file(self.ws, {"path": str(outside)})
        out = read_file(self.ws, {"path": str(outside)}, allow_escape=True)
        self.assertEqual(out["content"], "secret")

    def test_file_hash_stable_and_changes(self):
        p = self.ws / "hash.txt"
        p.write_text("hello", encoding="utf-8")
        h1 = file_hash(self.ws, {"path": "hash.txt"})
        h2 = file_hash(self.ws, {"path": "hash.txt"})
        self.assertEqual(h1["hash"], h2["hash"])
        self.assertEqual(len(h1["hash"]), 64)
        p.write_text("hello!", encoding="utf-8")
        h3 = file_hash(self.ws, {"path": "hash.txt"})
        self.assertNotEqual(h1["hash"], h3["hash"])

    def test_read_missing_raises(self):
        with self.assertRaises(ToolError):
            read_file(self.ws, {"path": "nope.txt"})

    def test_read_truncation(self):
        write_file(self.ws, {"path": "big.txt", "content": "x" * 1000})
        out = read_file(self.ws, {"path": "big.txt", "max_bytes": 100})
        self.assertTrue(out["truncated"])
        self.assertLessEqual(len(out["content"]), 200)

    def test_search_excludes_soul_private_dirs(self):
        # persona/ 与 .furina/ 属于 Soul 私有目录，搜索遍历必须跳过
        (self.ws / "persona").mkdir()
        (self.ws / "persona" / "furina.yaml").write_text("本神准了", encoding="utf-8")
        (self.ws / ".furina").mkdir()
        (self.ws / ".furina" / "secrets.env").write_text("KEY=xxx", encoding="utf-8")
        out = search(self.ws, {"path": "", "pattern": "本神"})
        self.assertEqual(out["matches"], [])
        out2 = search(self.ws, {"path": "", "pattern": "KEY"})
        self.assertEqual(out2["matches"], [])

    def test_create_file_existing_fails(self):
        with self.assertRaises(ToolError):
            create_file(self.ws, {"path": "hello.txt", "content": "x"})

    def test_search_finds_substring(self):
        matches = search(self.ws, {"pattern": "hello"})["matches"]
        self.assertEqual(len(matches), 1)
        self.assertEqual(matches[0]["path"], "hello.txt")
        self.assertEqual(matches[0]["line_number"], 1)

    def test_search_regex(self):
        matches = search(self.ws, {"pattern": r"line\s+two", "regex": True})["matches"]
        self.assertEqual(len(matches), 1)
        self.assertEqual(matches[0]["line_number"], 2)

    def test_relative_path_falls_back_to_samefile_parent_match(self):
        file_path = self.ws / "hello.txt"
        with mock.patch.object(
            Path,
            "relative_to",
            side_effect=ValueError("simulated Windows 8.3 alias mismatch"),
        ):
            relative = _relative_to_workspace(file_path, self.ws)
        self.assertEqual(relative.as_posix(), "hello.txt")

    def test_diff(self):
        d = diff_file(self.ws, {"path": "hello.txt", "content": "hello world!\nline two\n"})
        self.assertIn("-hello world", d["diff"])
        self.assertIn("+hello world!", d["diff"])

    def test_diff_new_file(self):
        d = diff_file(self.ws, {"path": "fresh.txt", "content": "new\n"})
        self.assertFalse(d["exists"])
        self.assertIn("+new", d["diff"])

    def test_path_escape_rejected(self):
        outside = self.ws.parent / "outside.txt"
        outside.write_text("secret", encoding="utf-8")
        rel = os.path.relpath(outside, self.ws)
        with self.assertRaises(ToolError):
            read_file(self.ws, {"path": rel})
        with self.assertRaises(ToolError):
            resolve_path(self.ws, rel)

    def test_path_escape_allowed_when_flag(self):
        outside = self.ws.parent / "outside_write.txt"
        rel = os.path.relpath(outside, self.ws)
        write_file(self.ws, {"path": rel, "content": "x"}, allow_escape=True)
        self.assertTrue(outside.exists())
        outside.unlink()

    def test_absolute_inside_workspace_ok(self):
        out = read_file(self.ws, {"path": str(self.ws / "hello.txt")})
        self.assertIn("hello world", out["content"])


class TerminalToolTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.ws = Path(self._tmp.name)

    def tearDown(self):
        self._tmp.cleanup()

    def test_run_command(self):
        notifications = []
        result = run_command(
            self.ws, {"command": 'python -c "print(\'hello from furina\')"'},
            lambda n: notifications.append(n))
        self.assertEqual(result["exit_code"], 0)
        self.assertIn("hello from furina", result["stdout"])
        self.assertTrue(any(n["method"] == "term.output" for n in notifications))

    def test_run_command_timeout(self):
        result = run_command(
            self.ws, {"command": "python -c \"import time; time.sleep(30)\"", "timeout_ms": 1000},
            lambda n: None)
        self.assertTrue(result["timed_out"])
        self.assertNotEqual(result["exit_code"], 0)


if __name__ == "__main__":
    unittest.main()
