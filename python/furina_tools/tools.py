"""Tool implementations: filesystem, search, diff, terminal. Stdlib only."""

import difflib
import subprocess
import threading
from pathlib import Path

EXCLUDED_DIRS = {
    ".git", "node_modules", "target", "__pycache__", ".pytest_cache",
    "venv", ".venv", "dist", "build", ".idea", ".vscode", "cache", "logs",
    # Soul 私有目录：人格配置 / 记忆 / 密钥不得被搜索遍历
    ".furina", "persona",
}


class ToolError(Exception):
    def __init__(self, message, code=-32000):
        super().__init__(message)
        self.code = code


def resolve_path(workspace, rel, allow_escape=False):
    """Resolve a path against the workspace root; reject escapes unless allowed."""
    workspace = Path(workspace).resolve()
    p = Path(rel)
    if not p.is_absolute():
        p = workspace / p
    p = p.resolve()
    if not allow_escape:
        try:
            p.relative_to(workspace)
        except ValueError:
            raise ToolError(f"path escapes workspace: {rel}", -32001)
    return p


def read_file(ws, params, allow_escape=False):
    path = resolve_path(ws, params["path"], allow_escape)
    max_bytes = int(params.get("max_bytes", 60000))
    if not path.is_file():
        raise ToolError(f"file not found: {params['path']}", -32002)
    data = path.read_bytes()
    if b"\x00" in data[:4096]:
        raise ToolError(f"binary file not supported: {params['path']}", -32003)
    text = data.decode("utf-8", errors="replace")
    truncated = False
    if len(text) > max_bytes:
        text = text[:max_bytes] + "\n...[truncated]"
        truncated = True
    return {"content": text, "truncated": truncated}


def write_file(ws, params, allow_escape=False):
    path = resolve_path(ws, params["path"], allow_escape)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(params["content"], encoding="utf-8")
    return {"written": True, "path": str(path)}


def create_file(ws, params, allow_escape=False):
    path = resolve_path(ws, params["path"], allow_escape)
    if path.exists():
        raise ToolError(f"file already exists: {params['path']}", -32004)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(params["content"], encoding="utf-8")
    return {"created": True, "path": str(path)}


def search(ws, params, allow_escape=False):
    root = resolve_path(ws, params.get("path", ""), allow_escape)
    pattern = params["pattern"]
    use_regex = bool(params.get("regex", False))
    limit = int(params.get("limit", 200))
    if use_regex:
        import re
        matcher = re.compile(pattern)
    else:
        matcher = pattern.lower()
    matches = []
    for dirpath, dirnames, filenames in os_walk(root):
        dirnames[:] = [d for d in dirnames if d not in EXCLUDED_DIRS and not d.startswith(".")]
        for name in filenames:
            if len(matches) >= limit:
                break
            fpath = Path(dirpath) / name
            try:
                with open(fpath, "r", encoding="utf-8", errors="replace") as f:
                    for lineno, line in enumerate(f, 1):
                        hit = matcher.search(line) if use_regex else (pattern.lower() in line.lower())
                        if hit:
                            rel = str(fpath.relative_to(ws)).replace("\\", "/")
                            matches.append({"path": rel, "line_number": lineno, "line": line.rstrip()[:500]})
                            break
            except (OSError, UnicodeError):
                continue
        if len(matches) >= limit:
            break
    return {"matches": matches}


def os_walk(root):
    import os
    return os.walk(str(root))


def diff_file(ws, params, allow_escape=False):
    path = resolve_path(ws, params["path"], allow_escape)
    new_lines = params["content"].splitlines(keepends=True)
    if path.is_file():
        old_lines = path.read_text(encoding="utf-8", errors="replace").splitlines(keepends=True)
        diff = "".join(difflib.unified_diff(
            old_lines, new_lines,
            fromfile=f"a/{params['path']}", tofile=f"b/{params['path']}", lineterm="\n"))
        exists = True
    else:
        diff = "".join(difflib.unified_diff(
            [], new_lines,
            fromfile="/dev/null", tofile=f"b/{params['path']}", lineterm="\n"))
        exists = False
    return {"diff": diff, "exists": exists}


def file_hash(ws, params, allow_escape=False):
    path = resolve_path(ws, params["path"], allow_escape)
    if not path.is_file():
        raise ToolError(f"file not found: {params['path']}", -32002)
    import hashlib

    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return {"path": str(path), "hash": h.hexdigest()}


def run_command(ws, params, notify):
    command = params["command"]
    cwd = resolve_path(ws, params.get("cwd", "")) if params.get("cwd") else resolve_path(ws, ".")
    timeout_ms = params.get("timeout_ms")
    timeout = timeout_ms / 1000.0 if timeout_ms else None
    request_id = params.get("request_id", 0)
    proc = subprocess.Popen(
        command, shell=True, cwd=str(cwd),
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, encoding="utf-8", errors="replace", bufsize=1,
    )
    out_chunks, err_chunks = [], []

    def pump(stream, name, sink):
        for line in iter(stream.readline, ""):
            sink.append(line)
            notify({"method": "term.output",
                    "params": {"requestId": request_id, "stream": name, "data": line}})

    t1 = threading.Thread(target=pump, args=(proc.stdout, "stdout", out_chunks))
    t1.daemon = True
    t2 = threading.Thread(target=pump, args=(proc.stderr, "stderr", err_chunks))
    t2.daemon = True
    t1.start()
    t2.start()
    timed_out = False
    try:
        code = proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        try:
            subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                           capture_output=True, timeout=15)
        except Exception:
            pass
        try:
            proc.kill()
        except Exception:
            pass
        code = 124
    t1.join(timeout=2)
    t2.join(timeout=2)
    for stream in (proc.stdout, proc.stderr):
        try:
            stream.close()
        except Exception:
            pass
    return {
        "exit_code": code,
        "timed_out": timed_out,
        "stdout": "".join(out_chunks)[-60000:],
        "stderr": "".join(err_chunks)[-60000:],
    }
