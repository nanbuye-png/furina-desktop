"""Structured test report parsing (stdlib only).

Heuristic categorization of test output into:
compile / assertion / timeout / dependency / other.
"""

import re


def detect_framework(command):
    c = command.lower()
    if "pytest" in c or "unittest" in c:
        return "pytest"
    if "cargo test" in c:
        return "cargo"
    if "npm test" in c or "npx" in c or "yarn test" in c:
        return "npm"
    if "mvn test" in c or "gradle test" in c:
        return "maven"
    if "go test" in c:
        return "go"
    return "unknown"


def _counts(text):
    """Extract passed/failed/total counts from common formats."""
    passed = failed = total = 0
    for m in re.finditer(r"(\d+)\s+passed", text):
        passed = int(m.group(1))
    for m in re.finditer(r"(\d+)\s+failed", text):
        failed = int(m.group(1))
    for m in re.finditer(r"(\d+)\s+error", text):
        failed = max(failed, int(m.group(1)))
    for m in re.finditer(r"Ran\s+(\d+)\s+tests?", text):
        total = int(m.group(1))
    if total == 0 and (passed or failed):
        total = passed + failed
    return passed, failed, total


def _category(line):
    if re.search(r"ModuleNotFoundError|No module named|ImportError|could not find|not found in this scope|is not recognized|command not found", line):
        return "dependency"
    if re.search(r"error\[|error:|SyntaxError|IndentationError|cannot find|expected .* found|not found in this scope|E0425|E0308|E0599|E0277|Compiling.*error", line):
        return "compile"
    if re.search(r"timed out|timeout|Timeout|duration exceeded|killed after", line):
        return "timeout"
    if re.search(r"AssertionError|FAILED|FAIL:|assert |expected|Actual| != |got ", line):
        return "assertion"
    return "other"


def parse_test_report(command, stdout, stderr, exit_code):
    text = (stdout or "") + "\n" + (stderr or "")
    passed_count, failed_count, total = _counts(text)
    passed = bool(exit_code == 0)
    errors = []
    seen = set()
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        cat = _category(stripped)
        if cat == "other":
            continue
        test = ""
        m = re.search(r"FAILED\s+([^\s]+)", stripped)
        if m:
            test = m.group(1)
        else:
            m = re.search(r"FAIL:\s+([^\s(]+)", stripped)
            if m:
                test = m.group(1)
        key = (cat, test, stripped[:120])
        if key in seen:
            continue
        seen.add(key)
        errors.append({
            "category": cat,
            "test": test,
            "message": stripped[:300],
        })
        if len(errors) >= 8:
            break

    summary = _summary(text, passed_count, failed_count, total)
    return {
        "framework": detect_framework(command),
        "passed": passed,
        "total": total,
        "passed_count": passed_count,
        "failed_count": failed_count,
        "errors": errors,
        "summary": summary,
    }


def _summary(text, passed, failed, total):
    for line in text.splitlines():
        s = line.strip()
        if re.search(r"^\d+ (passed|failed|error)", s) or s.startswith("Ran ") or "test result" in s:
            return s[:200]
    return ""
