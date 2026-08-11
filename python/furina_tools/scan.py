"""Project scanner: detect manifests, language, and a default test command."""

import os
from pathlib import Path

from .tools import EXCLUDED_DIRS

MANIFESTS = {
    "pyproject.toml": "python",
    "requirements.txt": "python",
    "setup.py": "python",
    "Pipfile": "python",
    "package.json": "node",
    "pom.xml": "java",
    "build.gradle": "java",
    "Cargo.toml": "rust",
    "go.mod": "go",
    "composer.json": "php",
    "Gemfile": "ruby",
}

TEST_COMMANDS = {
    "python": "python -m pytest",
    "node": "npm test",
    "rust": "cargo test",
    "java": "mvn test",
    "go": "go test ./...",
    "php": "phpunit",
    "ruby": "bundle exec rspec",
}

EXT_TO_LANG = {
    ".py": "python", ".js": "node", ".ts": "node", ".jsx": "node", ".tsx": "node",
    ".rs": "rust", ".java": "java", ".kt": "java", ".go": "go",
    ".rb": "ruby", ".php": "php", ".cs": "csharp", ".cpp": "cpp", ".c": "c",
}


def scan(ws):
    ws = Path(ws).resolve()
    manifests = []
    project_type = "unknown"
    language = "unknown"
    for name, ptype in MANIFESTS.items():
        if (ws / name).is_file():
            manifests.append(name)
            if project_type == "unknown":
                project_type = ptype
                language = ptype

    ext_counts = {}
    total_files = 0
    top_level = []
    for entry in sorted(ws.iterdir(), key=lambda e: e.name.lower()):
        if entry.name in EXCLUDED_DIRS or entry.name.startswith("."):
            continue
        if entry.is_dir():
            top_level.append({"path": entry.name, "kind": "dir", "size": None})
        else:
            try:
                size = entry.stat().st_size
            except OSError:
                size = None
            top_level.append({"path": entry.name, "kind": "file", "size": size})
            if entry.suffix:
                ext_counts[entry.suffix.lower()] = ext_counts.get(entry.suffix.lower(), 0) + 1

    for root, dirs, files in os.walk(ws):
        dirs[:] = [d for d in dirs if d not in EXCLUDED_DIRS and not d.startswith(".")]
        total_files += len(files)

    if project_type == "unknown" and ext_counts:
        best = max(ext_counts, key=ext_counts.get)
        language = EXT_TO_LANG.get(best, best.lstrip("."))
        if language in TEST_COMMANDS:
            project_type = language

    return {
        "project_type": project_type,
        "language": language,
        "test_command": TEST_COMMANDS.get(project_type, ""),
        "manifests": manifests,
        "top_level": top_level,
        "total_files": total_files,
    }
