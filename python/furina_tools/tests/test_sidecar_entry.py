import json
import os
import subprocess
import sys
import tempfile
import unittest


class SidecarEntryTests(unittest.TestCase):
    def test_module_entry_supports_json_rpc(self):
        env = os.environ.copy()
        python_root = os.path.dirname(os.path.dirname(os.path.dirname(__file__)))
        env["PYTHONPATH"] = python_root + os.pathsep + env.get("PYTHONPATH", "")
        process = subprocess.Popen(
            [sys.executable, "-m", "furina_tools.sidecar_entry"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            env=env,
        )
        try:
            with tempfile.TemporaryDirectory() as workspace:
                process.stdin.write(json.dumps({"id": 1, "method": "initialize", "params": {"workspace_root": workspace}}) + "\n")
                process.stdin.flush()
                response = json.loads(process.stdout.readline())
                self.assertEqual(response["result"]["version"], "0.1.3")
        finally:
            process.terminate()
            process.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()
