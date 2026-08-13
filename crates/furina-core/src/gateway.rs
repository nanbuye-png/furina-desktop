//! Deterministic permission gateway: auto-allow allowlist, danger patterns,
//! workspace-escape detection. Safety decisions never go through the LLM.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub enum ActionKind {
    WriteFile { path: String },
    RunCommand { command: String },
    GitCommit { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    RequireApproval,
    Block(String),
}

pub struct PermissionGateway {
    auto_allow: Vec<String>,
    danger_patterns: Vec<String>,
}

impl PermissionGateway {
    pub fn new(auto_allow: Vec<String>, danger_patterns: Vec<String>) -> Self {
        Self { auto_allow, danger_patterns }
    }

    pub fn check(&self, action: &ActionKind) -> Verdict {
        match action {
            ActionKind::WriteFile { .. } | ActionKind::GitCommit { .. } => Verdict::RequireApproval,
            ActionKind::RunCommand { command } => {
                let norm = normalize_command(command);
                if is_permanently_blocked_command(&norm) {
                    return Verdict::Block("命令被安全策略永久拦截：删除、批量删除或破坏性 Git 操作不可执行".into());
                }
                for d in &self.danger_patterns {
                    if norm.contains(&normalize_command(d)) {
                        return Verdict::Block(format!("命令被安全策略拦截（危险模式：{d}）"));
                    }
                }
                for a in &self.auto_allow {
                    if norm.starts_with(&normalize_command(a)) {
                        return Verdict::Allow;
                    }
                }
                Verdict::RequireApproval
            }
        }
    }

    pub fn is_test_command(&self, command: &str) -> bool {
        let norm = normalize_command(command);
        const TESTS: &[&str] = &[
            "pytest",
            "python -m pytest",
            "python -m unittest",
            "unittest",
            "cargo test",
            "npm test",
            "mvn test",
            "gradle test",
            "go test",
        ];
        TESTS.iter().any(|t| norm.starts_with(t))
    }
}

fn is_permanently_blocked_command(norm: &str) -> bool {
    let tokens: Vec<&str> = norm.split_whitespace().collect();
    let first = tokens.first().copied().unwrap_or_default();
    if matches!(first, "del" | "erase" | "rd" | "rmdir" | "rm" | "remove-item") {
        return true;
    }
    if first == "git" && tokens.iter().any(|token| *token == "clean") {
        return true;
    }
    if first == "git" && tokens.iter().any(|token| *token == "reset") {
        return tokens.iter().any(|token| *token == "--hard");
    }
    let recursive = tokens.iter().any(|token| matches!(*token, "-r" | "-rf" | "-fr" | "--recursive" | "-recurse"));
    let wildcard = norm.contains('*') || norm.contains('?');
    recursive && wildcard
}

pub fn normalize_command(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Lexically normalize a path (handles `.`/`..` without touching the disk).
pub fn lexically_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(windows)]
fn path_under(ws: &Path, p: &Path) -> bool {
    let w = ws.to_string_lossy().to_lowercase();
    let x = p.to_string_lossy().to_lowercase();
    x.starts_with(&w)
}

#[cfg(not(windows))]
fn path_under(ws: &Path, p: &Path) -> bool {
    p.starts_with(ws)
}

/// 路径是否位于 root 之下（Windows 大小写不敏感）。
pub fn path_is_under(root: &Path, p: &Path) -> bool {
    path_under(root, p)
}

/// 解析后的路径是否落在 Soul 私有/运行时配置目录内。
/// `private` 为绝对路径列表（如 `<root>/persona`、`<root>/.furina`）；
/// `path` 可为相对 workspace 或绝对路径，`..` 会先做词法归一化。
pub fn is_private_path(private: &[PathBuf], workspace: &Path, path: &str) -> bool {
    let p = Path::new(path);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace.join(p)
    };
    let norm = lexically_normalize(&joined);
    private
        .iter()
        .any(|priv_dir| path_is_under(priv_dir, &norm))
}

/// Returns true if the given path (relative to the workspace or absolute)
/// escapes the workspace root.
pub fn escapes_workspace(workspace: &Path, path: &str) -> bool {
    let p = Path::new(path);
    let joined = if p.is_absolute() { p.to_path_buf() } else { workspace.join(p) };
    let norm = lexically_normalize(&joined);
    !path_under(workspace, &norm)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gw() -> PermissionGateway {
        PermissionGateway::new(
            vec![
                "pytest".into(),
                "python -m pytest".into(),
                "git status".into(),
                "git diff".into(),
            ],
            vec!["rm -rf".into(), "drop database".into(), "git push --force".into()],
        )
    }

    #[test]
    fn allow_readonly_commands() {
        assert_eq!(gw().check(&ActionKind::RunCommand { command: "pytest".into() }), Verdict::Allow);
        assert_eq!(gw().check(&ActionKind::RunCommand { command: "PYTHON -m pytest tests/".into() }), Verdict::Allow);
        assert_eq!(gw().check(&ActionKind::RunCommand { command: "git status --porcelain".into() }), Verdict::Allow);
        assert_eq!(gw().check(&ActionKind::RunCommand { command: "git diff HEAD".into() }), Verdict::Allow);
    }

    #[test]
    fn require_approval_for_writes_and_commits() {
        assert_eq!(gw().check(&ActionKind::WriteFile { path: "a.py".into() }), Verdict::RequireApproval);
        assert_eq!(gw().check(&ActionKind::GitCommit { message: "x".into() }), Verdict::RequireApproval);
        assert_eq!(gw().check(&ActionKind::RunCommand { command: "npm install".into() }), Verdict::RequireApproval);
    }

    #[test]
    fn block_dangerous_commands() {
        assert!(matches!(gw().check(&ActionKind::RunCommand { command: "rm -rf /".into() }), Verdict::Block(_)));
        assert!(matches!(gw().check(&ActionKind::RunCommand { command: "mysql -e 'DROP DATABASE x'".into() }), Verdict::Block(_)));
        assert!(matches!(gw().check(&ActionKind::RunCommand { command: "git push --force origin main".into() }), Verdict::Block(_)));
    }

    #[test]
    fn permanently_block_deletion_and_destructive_git_commands() {
        for command in [
            "rm file.txt",
            "rm -rf build",
            "del file.txt",
            "erase file.txt",
            "rd /s /q build",
            "rmdir /s /q build",
            "Remove-Item file.txt",
            "Remove-Item -Recurse build",
            "git clean -fd",
            "git reset --hard HEAD",
            "rm -r *.tmp",
        ] {
            assert!(matches!(gw().check(&ActionKind::RunCommand { command: command.into() }), Verdict::Block(_)), "{command}");
        }
    }

    #[test]
    fn test_command_detection() {
        assert!(gw().is_test_command("python -m pytest tests"));
        assert!(gw().is_test_command("cargo test"));
        assert!(gw().is_test_command("npm test -- --watch"));
        assert!(!gw().is_test_command("npm install"));
    }

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(normalize_command("  Git   DIFF   --cached "), "git diff --cached");
    }

    #[test]
    fn path_escape_detection() {
        let ws = Path::new(r"C:\work\proj");
        assert!(!escapes_workspace(ws, "src/main.rs"));
        assert!(!escapes_workspace(ws, "src/../README.md"));
        assert!(escapes_workspace(ws, "../outside.txt"));
        assert!(escapes_workspace(ws, r"C:\work\other\file.txt"));
    }

    #[test]
    fn private_path_detection() {
        let root = Path::new(r"C:\work\Furina_Agent");
        let ws = root.to_path_buf();
        let private = vec![root.join("persona"), root.join(".furina")];
        // 相对路径（LLM 常用）
        assert!(is_private_path(&private, &ws, "persona/furina.yaml"));
        assert!(is_private_path(&private, &ws, ".furina/secrets.env"));
        assert!(is_private_path(&private, &ws, ".furina/memory/emotion.json"));
        // 绝对路径
        assert!(is_private_path(&private, &ws, r"C:\work\Furina_Agent\persona\soul\values.yaml"));
        // .. 归一化后落入私有目录
        assert!(is_private_path(&private, &ws, "tests/../persona/system_prompt.md"));
        // 普通工作区文件不误伤
        assert!(!is_private_path(&private, &ws, "src/main.rs"));
        assert!(!is_private_path(&private, &ws, "README.md"));
        // workspace 非仓库根时，用户项目自己的 persona/ 不应被误伤
        let user_ws = Path::new(r"C:\work\user_proj");
        assert!(!is_private_path(&private, user_ws, "persona/cast.yaml"));
    }
}
