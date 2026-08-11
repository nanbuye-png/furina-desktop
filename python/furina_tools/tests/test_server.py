import json
import queue
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
PYTHON_DIR = REPO_ROOT / "python"


class SidecarClient:
    """Minimal test client for the JSON-RPC server over stdio."""

    def __init__(self):
        env = dict(__import__("os").environ)
        env["PYTHONPATH"] = str(PYTHON_DIR)
        self.proc = subprocess.Popen(
            [sys.executable, "-m", "furina_tools.server"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            text=True, encoding="utf-8", env=env, bufsize=1,
        )
        self.q = queue.Queue()
        self._reader = threading.Thread(target=self._read, daemon=True)
        self._reader.start()
        self.next_id = 0

    def _read(self):
        for line in self.proc.stdout:
            self.q.put(line.strip())

    def call(self, method, params=None, timeout=15):
        self.next_id += 1
        rid = self.next_id
        self.proc.stdin.write(json.dumps({"id": rid, "method": method, "params": params or {}}) + "\n")
        self.proc.stdin.flush()
        while True:
            line = self.q.get(timeout=timeout)
            msg = json.loads(line)
            if msg.get("id") == rid:
                return msg

    def close(self):
        try:
            self.proc.terminate()
        except Exception:
            pass
        self.proc.wait(timeout=5)
        for stream in (self.proc.stdin, self.proc.stdout, self.proc.stderr):
            try:
                stream.close()
            except Exception:
                pass


class ServerTest(unittest.TestCase):
    def test_roundtrip(self):
        with tempfile.TemporaryDirectory() as tmp:
            client = SidecarClient()
            try:
                init = client.call("initialize", {"workspace_root": tmp})
                self.assertTrue(init["result"]["ok"])
                tools = client.call("tools.list")
                names = [t["name"] for t in tools["result"]["tools"]]
                self.assertIn("fs.read_file", names)
                self.assertIn("term.run", names)

                w = client.call("fs.write_file", {"path": "a/b.txt", "content": "hello"})
                self.assertTrue(w["result"]["written"])
                r = client.call("fs.read_file", {"path": "a/b.txt"})
                self.assertEqual(r["result"]["content"], "hello")

                bad = client.call("fs.read_file", {"path": "../secret.txt"})
                self.assertIsNotNone(bad.get("error"))
                self.assertEqual(bad["error"]["code"], -32001)

                scan = client.call("scan.start")
                self.assertIn("project_type", scan["result"])
            finally:
                client.close()


if __name__ == "__main__":
    unittest.main()
