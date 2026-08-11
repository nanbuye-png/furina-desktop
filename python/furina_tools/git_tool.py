"""Git tools: status, diff, commit."""

import subprocess

from .tools import ToolError


def _git(ws, *args, timeout=30):
    result = subprocess.run(
        ["git", *args], cwd=str(ws), capture_output=True,
        text=True, encoding="utf-8", errors="replace", timeout=timeout)
    if result.returncode != 0:
        raise ToolError(f"git {args[0]} failed: {(result.stderr or result.stdout).strip()}", -32005)
    return result.stdout


def git_status(ws, _params):
    return {"status": _git(ws, "status", "--porcelain", "-b")}


def git_diff(ws, params):
    args = ["diff"]
    if params.get("path"):
        args.append(params["path"])
    return {"diff": _git(ws, *args)}


def git_commit(ws, params):
    _git(ws, "add", "-A")
    out = _git(ws, "commit", "-m", params["message"])
    return {"commit": out}
