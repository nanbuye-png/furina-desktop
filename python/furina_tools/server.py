"""Newline-delimited JSON-RPC server over stdio (LSP-style)."""

import json
import sys
from pathlib import Path

from . import __version__
from .git_tool import git_commit, git_diff, git_status
from .scan import scan
from .tests_report import parse_test_report
from .tools import (
    ToolError, create_file, diff_file, file_hash, read_file, run_command, search, write_file,
)


def tools_list():
    return {"tools": [
        {"name": "fs.read_file", "description": "读取 workspace 内的文本文件",
         "parameters": {"type": "object",
                        "properties": {"path": {"type": "string", "description": "相对 workspace 的文件路径"},
                                       "max_bytes": {"type": "integer", "description": "最大读取字符数"}},
                        "required": ["path"]}},
        {"name": "fs.write_file", "description": "写入/覆盖 workspace 内文件（需审批）",
         "parameters": {"type": "object",
                        "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
                        "required": ["path", "content"]}},
        {"name": "fs.create_file", "description": "创建新文件（需审批，已存在则失败）",
         "parameters": {"type": "object",
                        "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
                        "required": ["path", "content"]}},
        {"name": "fs.search", "description": "在 workspace 内搜索文本或正则",
         "parameters": {"type": "object",
                        "properties": {"pattern": {"type": "string"},
                                       "path": {"type": "string", "description": "可选子目录"},
                                       "regex": {"type": "boolean"}},
                        "required": ["pattern"]}},
        {"name": "fs.diff", "description": "预览新内容与现有文件的差异",
         "parameters": {"type": "object",
                        "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
                        "required": ["path", "content"]}},
        {"name": "term.run", "description": "在 workspace 内运行命令（只读命令自动放行，其余需审批）",
         "parameters": {"type": "object",
                        "properties": {"command": {"type": "string"},
                                       "cwd": {"type": "string", "description": "可选子目录"},
                                       "timeout_ms": {"type": "integer"}},
                        "required": ["command"]}},
        {"name": "git.status", "description": "查看 git 状态（porcelain）", "parameters": {"type": "object"}},
        {"name": "git.diff", "description": "查看工作区未暂存差异", "parameters": {"type": "object",
                        "properties": {"path": {"type": "string"}}}},
        {"name": "git.commit", "description": "暂存所有改动并提交（需审批）",
         "parameters": {"type": "object", "properties": {"message": {"type": "string"}},
                        "required": ["message"]}},
        {"name": "tests.parse", "description": "解析测试输出为结构化报告（编译/断言/超时/依赖归类）",
         "parameters": {"type": "object",
                        "properties": {"command": {"type": "string"}, "stdout": {"type": "string"},
                                       "stderr": {"type": "string"}, "exit_code": {"type": "integer"}},
                        "required": ["command", "stdout", "stderr", "exit_code"]}},
    ]}


def dispatch(ws, method, params, notify):
    allow_escape = bool(params.get("allow_escape", False))
    if method == "tools.list":
        return tools_list()
    if method == "fs.read_file":
        return read_file(ws, params, allow_escape)
    if method == "fs.write_file":
        return write_file(ws, params, allow_escape)
    if method == "fs.create_file":
        return create_file(ws, params, allow_escape)
    if method == "fs.search":
        return search(ws, params, allow_escape)
    if method == "fs.diff":
        return diff_file(ws, params, allow_escape)
    if method == "fs.hash":
        return file_hash(ws, params, allow_escape)
    if method == "term.run":
        return run_command(ws, params, notify)
    if method == "tests.parse":
        return parse_test_report(
            params.get("command", ""),
            params.get("stdout", ""),
            params.get("stderr", ""),
            params.get("exit_code", -1),
        )
    if method == "git.status":
        return git_status(ws, params)
    if method == "git.diff":
        return git_diff(ws, params)
    if method == "git.commit":
        return git_commit(ws, params)
    if method == "scan.start":
        return scan(ws)
    raise ToolError(f"unknown method: {method}", -32601)


def main():
    for stream in (sys.stdin, sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8")
        except Exception:
            pass
    workspace = None

    def write(obj):
        sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
        sys.stdout.flush()

    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        rid = msg.get("id")
        method = msg.get("method", "")
        params = dict(msg.get("params") or {})
        if rid is None:
            continue  # client->server notifications are unused in v1
        params.setdefault("request_id", rid)

        def notify(obj):
            write(obj)

        try:
            if method == "initialize":
                workspace = Path(params["workspace_root"]).resolve()
                result = {"ok": True, "version": __version__, "workspace": str(workspace)}
            elif workspace is None:
                raise ToolError("sidecar not initialized", -32009)
            else:
                result = dispatch(workspace, method, params, notify)
            write({"id": rid, "result": result})
        except ToolError as e:
            write({"id": rid, "error": {"code": e.code, "message": str(e)}})
        except Exception as e:  # noqa: BLE001
            write({"id": rid, "error": {"code": -32000, "message": f"{type(e).__name__}: {e}"}})


if __name__ == "__main__":
    main()
