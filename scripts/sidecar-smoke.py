"""Smoke-test a Furina sidecar executable or the Python module entry point."""

import json
import subprocess
import sys
import tempfile
from pathlib import Path


def exchange(process, payload):
    process.stdin.write(json.dumps(payload, ensure_ascii=False) + "\n")
    process.stdin.flush()
    line = process.stdout.readline()
    if not line:
        stderr = process.stderr.read()
        raise RuntimeError(f"sidecar exited without a response: {stderr}")
    return json.loads(line)


def main():
    command = [sys.argv[1]] if len(sys.argv) > 1 else [sys.executable, "-m", "furina_tools.sidecar_entry"]
    with tempfile.TemporaryDirectory(prefix="furina-sidecar-smoke-") as workspace:
        process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )
        try:
            initialized = exchange(process, {"id": 1, "method": "initialize", "params": {"workspace_root": str(Path(workspace))}})
            if initialized.get("result", {}).get("version") != "0.1.2":
                raise RuntimeError(f"unexpected initialize response: {initialized}")
            tools = exchange(process, {"id": 2, "method": "tools.list", "params": {}})
            names = {item.get("name") for item in tools.get("result", {}).get("tools", [])}
            if "fs.read_file" not in names or "term.run" not in names:
                raise RuntimeError(f"tools.list missing expected tools: {tools}")
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
    print("sidecar smoke test passed")


if __name__ == "__main__":
    main()
