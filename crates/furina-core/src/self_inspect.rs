use crate::config::Config;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use tokio::process::Command;

const SOURCE_PREFIXES: &[&str] = &[
    "crates",
    "python",
    "tests",
    "docs",
    "desktop/src-tauri/src",
    "desktop/ui/src",
    "desktop/resources/defaults",
];
const SOURCE_FILES: &[&str] = &["Cargo.toml", "Cargo.lock", "README.md"];
const BLOCKED_COMPONENTS: &[&str] = &[
    ".git", ".furina", "persona", "target", "node_modules", "dist", "bin", "secrets.env",
];

#[derive(Debug, Clone)]
pub struct SelfInspector {
    mode: String,
    source_root: Option<PathBuf>,
    manifest_path: PathBuf,
    workspace: PathBuf,
    sidecar_version: String,
    config: Config,
    config_path: PathBuf,
    proposals_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProposedFileChange {
    pub path: String,
    #[serde(default)]
    pub expected_sha256: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SelfChangeInput {
    pub problem: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub changes: Vec<ProposedFileChange>,
    #[serde(default)]
    pub config_updates: BTreeMap<String, Value>,
    #[serde(default)]
    pub tests: Vec<String>,
    #[serde(default)]
    pub risk: String,
    #[serde(default)]
    pub rollback: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SelfChangeProposal {
    pub id: String,
    pub created_at_ms: u128,
    pub base_version: String,
    pub mode: String,
    pub problem: String,
    pub evidence: Vec<String>,
    pub changes: Vec<ProposedFileChange>,
    pub diffs: BTreeMap<String, String>,
    pub config_updates: BTreeMap<String, Value>,
    pub tests: Vec<String>,
    pub risk: String,
    pub rollback: String,
    pub applicable: bool,
    pub status: String,
}

impl SelfInspector {
    pub fn new(
        mode: String,
        source_root: Option<PathBuf>,
        manifest_path: PathBuf,
        workspace: PathBuf,
        sidecar_version: String,
        config: Config,
        config_path: PathBuf,
        proposals_dir: PathBuf,
    ) -> Self {
        Self { mode, source_root, manifest_path, workspace, sidecar_version, config, config_path, proposals_dir }
    }

    pub fn status(&self) -> Value {
        let manifest = fs::read_to_string(&self.manifest_path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .unwrap_or_else(|| json!({
                "version": env!("CARGO_PKG_VERSION"),
                "components": ["furina-core", "furina-proto", "furina-soul", "furina-desktop", "furina-sidecar"],
                "config_schema_version": 2,
            }));
        let providers = self.config.llm.providers.iter().map(|provider| json!({
            "id": provider.id,
            "label": provider.label,
            "base_url": redact_endpoint(&provider.base_url),
            "model": provider.model,
            "vision": provider.vision,
            "api_key_configured": std::env::var_os(&provider.api_key_env).is_some(),
        })).collect::<Vec<_>>();
        json!({
            "mode": self.mode,
            "version": env!("CARGO_PKG_VERSION"),
            "sidecar_version": self.sidecar_version,
            "workspace": self.workspace,
            "source_access": self.source_root.is_some(),
            "manifest": manifest,
            "capabilities": [
                "workspace_files", "terminal", "git", "web", "safe_apps",
                "self_status", "self_read_source", "self_search_source", "self_propose_change"
            ],
            "config": {
                "model": self.config.model,
                "active_provider": self.config.llm.active_provider,
                "providers": providers,
                "agent": {
                    "checkpoint_interval_steps": self.config.agent.checkpoint_interval_steps,
                    "max_repeated_tool_calls": self.config.agent.max_repeated_tool_calls,
                    "max_stalled_checkpoints": self.config.agent.max_stalled_checkpoints,
                    "repair_review_after": self.config.agent.repair_review_after,
                    "self_inspection_enabled": self.config.agent.self_inspection_enabled,
                    "experience_learning_enabled": self.config.agent.experience_learning_enabled,
                    "self_change_proposals_enabled": self.config.agent.self_change_proposals_enabled,
                },
                "voice_enabled": self.config.voice.enabled,
                "asr_enabled": self.config.asr.enabled,
                "vision_enabled": self.config.vision.enabled,
                "web_backend": self.config.web.search_backend,
            }
        })
    }

    pub fn read_source(&self, relative: &str, max_bytes: usize) -> anyhow::Result<Value> {
        let path = self.resolve_source(relative, false)?;
        if !path.is_file() { anyhow::bail!("自身源码文件不存在: {relative}"); }
        let bytes = fs::read(&path)?;
        if bytes.iter().take(4096).any(|byte| *byte == 0) { anyhow::bail!("不支持读取二进制自身文件"); }
        let mut text = String::from_utf8_lossy(&bytes).to_string();
        let limit = max_bytes.clamp(1, 120_000);
        let truncated = text.len() > limit;
        if truncated { text.truncate(limit); text.push_str("\n...[truncated]"); }
        Ok(json!({"path": relative.replace('\\', "/"), "content": text, "truncated": truncated, "sha256": sha256_bytes(&bytes)}))
    }

    pub fn search_source(&self, relative: &str, pattern: &str, regex: bool, limit: usize) -> anyhow::Result<Value> {
        if pattern.trim().is_empty() { anyhow::bail!("搜索内容不能为空"); }
        let source_root = self.source_root.as_ref().ok_or_else(|| anyhow::anyhow!("安装模式不提供自身源码读取"))?.canonicalize()?;
        let roots = if relative.trim().is_empty() {
            SOURCE_PREFIXES.iter().map(|prefix| source_root.join(prefix))
                .chain(SOURCE_FILES.iter().map(|file| source_root.join(file)))
                .filter(|path| path.exists()).collect::<Vec<_>>()
        } else {
            vec![self.resolve_source(relative, false)?]
        };
        let compiled = if regex { Some(regex::Regex::new(pattern).map_err(|error| anyhow::anyhow!(error.to_string()))?) } else { None };
        let needle = pattern.to_lowercase();
        let mut matches = Vec::new();
        for root in roots {
            let mut files = Vec::new();
            collect_files(&root, &mut files, 5_000)?;
            for file in files {
                if matches.len() >= limit.clamp(1, 200) { break; }
                let Ok(text) = fs::read_to_string(&file) else { continue };
                for (index, line) in text.lines().enumerate() {
                    let matched = compiled.as_ref().map(|regex| regex.is_match(line)).unwrap_or_else(|| line.to_lowercase().contains(&needle));
                    if matched {
                        let rel = file.strip_prefix(&source_root).unwrap_or(&file).to_string_lossy().replace('\\', "/");
                        matches.push(json!({"path": rel, "line_number": index + 1, "line": truncate(line, 500)}));
                        break;
                    }
                }
            }
            if matches.len() >= limit.clamp(1, 200) { break; }
        }
        Ok(json!({"matches": matches}))
    }

    pub fn create_proposal(&self, mut input: SelfChangeInput) -> anyhow::Result<SelfChangeProposal> {
        if input.problem.trim().is_empty() { anyhow::bail!("改进提案必须说明问题"); }
        input.evidence = input.evidence.into_iter().map(|item| sanitize(&item, 500)).collect();
        validate_config_updates(&input.config_updates)?;
        let mut diffs = BTreeMap::new();
        let applicable = self.source_root.is_some() && (!input.changes.is_empty() || !input.config_updates.is_empty());
        if applicable {
            for change in &mut input.changes {
                let path = self.resolve_source(&change.path, true)?;
                let previous = if path.exists() {
                    let bytes = fs::read(&path)?;
                    let current = sha256_bytes(&bytes);
                    if change.expected_sha256.is_empty() { change.expected_sha256 = current; }
                    String::from_utf8_lossy(&bytes).to_string()
                } else {
                    String::new()
                };
                diffs.insert(change.path.clone(), unified_diff(&change.path, &previous, &change.content));
                if change.content.len() > 1_000_000 { anyhow::bail!("单个自身变更文件超过 1MB: {}", change.path); }
            }
        }
        let id = format!("proposal_{}", now_ms());
        let proposal = SelfChangeProposal {
            id: id.clone(),
            created_at_ms: now_ms(),
            base_version: env!("CARGO_PKG_VERSION").into(),
            mode: self.mode.clone(),
            problem: sanitize(&input.problem, 2_000),
            evidence: input.evidence,
            changes: input.changes,
            diffs,
            config_updates: input.config_updates,
            tests: input.tests,
            risk: sanitize(&input.risk, 2_000),
            rollback: sanitize(&input.rollback, 2_000),
            applicable,
            status: if applicable { "pending_approval".into() } else { "exported".into() },
        };
        self.save_proposal(&proposal)?;
        Ok(proposal)
    }

    pub fn mark_proposal_status(&self, proposal: &mut SelfChangeProposal, status: &str) -> anyhow::Result<()> {
        proposal.status = status.to_string();
        self.save_proposal(proposal)
    }

    fn apply_config_updates(&self, updates: &BTreeMap<String, Value>) -> anyhow::Result<()> {
        if updates.is_empty() { return Ok(()); }
        let mut config = Config::load(&self.config_path)?;
        for (key, value) in updates {
            match key.as_str() {
                "agent.checkpoint_interval_steps" => config.agent.checkpoint_interval_steps = value.as_u64().unwrap() as u32,
                "agent.max_repeated_tool_calls" => config.agent.max_repeated_tool_calls = value.as_u64().unwrap() as u32,
                "agent.max_stalled_checkpoints" => config.agent.max_stalled_checkpoints = value.as_u64().unwrap() as u32,
                "agent.repair_review_after" => config.agent.repair_review_after = value.as_u64().unwrap() as u32,
                "agent.self_inspection_enabled" => config.agent.self_inspection_enabled = value.as_bool().unwrap(),
                "agent.experience_learning_enabled" => config.agent.experience_learning_enabled = value.as_bool().unwrap(),
                "agent.self_change_proposals_enabled" => config.agent.self_change_proposals_enabled = value.as_bool().unwrap(),
                _ => anyhow::bail!("不允许通过自身提案修改配置项: {key}"),
            }
        }
        atomic_write(&self.config_path, serde_yaml::to_string(&config)?.as_bytes())
    }

    pub async fn apply_proposal(&self, proposal: &mut SelfChangeProposal) -> anyhow::Result<String> {
        if !proposal.applicable { anyhow::bail!("安装模式只能导出自身改进提案"); }
        let mut backups = Vec::new();
        let config_backup = if proposal.config_updates.is_empty() { None } else { fs::read(&self.config_path).ok() };
        for change in &proposal.changes {
            let path = self.resolve_source(&change.path, true)?;
            let previous = fs::read(&path).ok();
            if let Some(bytes) = &previous {
                let current = sha256_bytes(bytes);
                if !change.expected_sha256.is_empty() && current != change.expected_sha256 {
                    anyhow::bail!("目标文件在提案后发生变化，拒绝应用: {}", change.path);
                }
            } else if !change.expected_sha256.is_empty() {
                anyhow::bail!("目标文件已不存在，拒绝应用: {}", change.path);
            }
            backups.push((path, previous));
        }
        for (change, (path, _)) in proposal.changes.iter().zip(backups.iter()) {
            if let Err(error) = atomic_write(path, change.content.as_bytes()) {
                rollback(&backups)?;
                if !proposal.config_updates.is_empty() {
                    restore_optional_file(&self.config_path, config_backup.as_deref())?;
                }
                proposal.status = "rolled_back".into();
                self.save_proposal(proposal)?;
                return Err(error);
            }
        }
        if let Err(error) = self.apply_config_updates(&proposal.config_updates) {
            rollback(&backups)?;
            if !proposal.config_updates.is_empty() {
                restore_optional_file(&self.config_path, config_backup.as_deref())?;
            }
            proposal.status = "rolled_back".into();
            self.save_proposal(proposal)?;
            return Err(error);
        }
        for command in &proposal.tests {
            let result = run_validation(command, self.source_root.as_ref().unwrap()).await;
            if let Err(error) = result {
                rollback(&backups)?;
                if !proposal.config_updates.is_empty() {
                    if !proposal.config_updates.is_empty() {
                restore_optional_file(&self.config_path, config_backup.as_deref())?;
            }
                }
                proposal.status = "rolled_back".into();
                self.save_proposal(proposal)?;
                anyhow::bail!("验证失败，已回滚: {error}");
            }
        }
        proposal.status = "applied".into();
        self.save_proposal(proposal)?;
        Ok(format!("已应用 {} 个文件、{} 个配置项并通过 {} 个验证命令", proposal.changes.len(), proposal.config_updates.len(), proposal.tests.len()))
    }

    pub fn proposal_approval_detail(&self, proposal: &SelfChangeProposal) -> String {
        let mut detail = format!(
            "问题：{}\n目标：{}\n配置更新：{}\n风险：{}\n回滚：{}\n验证：{}",
            proposal.problem,
            proposal.changes.iter().map(|change| change.path.as_str()).collect::<Vec<_>>().join(", "),
            proposal.config_updates.keys().cloned().collect::<Vec<_>>().join(", "),
            proposal.risk,
            proposal.rollback,
            proposal.tests.join("；"),
        );
        for change in &proposal.changes {
            detail.push_str(&format!(
                "\n\n目标：{}\n预期哈希：{}\n统一差异：\n{}",
                change.path,
                change.expected_sha256,
                truncate(proposal.diffs.get(&change.path).map(String::as_str).unwrap_or("（无差异）"), 8_000),
            ));
        }
        truncate(&detail, 16_000)
    }

    fn save_proposal(&self, proposal: &SelfChangeProposal) -> anyhow::Result<()> {
        let dir = self.proposals_dir.join(&proposal.id);
        fs::create_dir_all(&dir)?;
        atomic_write(&dir.join("proposal.json"), serde_json::to_vec_pretty(proposal)?.as_slice())
    }

    fn resolve_source(&self, relative: &str, allow_missing: bool) -> anyhow::Result<PathBuf> {
        let root = self.source_root.as_ref().ok_or_else(|| anyhow::anyhow!("安装模式不提供自身源码读取"))?.canonicalize()?;
        let relative_path = Path::new(relative);
        if relative_path.is_absolute() { anyhow::bail!("自身源码路径必须是相对路径"); }
        if relative_path.components().any(|component| matches!(component, Component::ParentDir)) { anyhow::bail!("自身源码路径禁止使用 .."); }
        let normalized = relative_path.to_string_lossy().replace('\\', "/").trim_start_matches("./").to_string();
        if normalized.is_empty() { return Ok(root); }
        let lower_parts = normalized.split('/').map(|part| part.to_lowercase()).collect::<BTreeSet<_>>();
        if BLOCKED_COMPONENTS.iter().any(|blocked| lower_parts.contains(*blocked)) { anyhow::bail!("自身源码路径位于禁止区域"); }
        let normalized_lower = normalized.to_lowercase();
        let allowed = SOURCE_FILES.iter().any(|file| normalized_lower == file.to_lowercase())
            || SOURCE_PREFIXES.iter().any(|prefix| {
                let prefix = prefix.to_lowercase();
                normalized_lower == prefix || normalized_lower.starts_with(&format!("{prefix}/"))
            });
        if !allowed { anyhow::bail!("自身源码路径不在只读白名单: {relative}"); }
        let joined = root.join(relative_path);
        let checked = if joined.exists() {
            joined.canonicalize()?
        } else if allow_missing {
            let parent = joined.parent().ok_or_else(|| anyhow::anyhow!("自身源码路径缺少父目录"))?.canonicalize()?;
            if !parent.starts_with(&root) { anyhow::bail!("自身源码路径逃逸源码根目录"); }
            joined
        } else {
            anyhow::bail!("自身源码路径不存在: {relative}");
        };
        if !checked.starts_with(&root) { anyhow::bail!("自身源码路径逃逸源码根目录"); }
        Ok(checked)
    }
}

fn unified_diff(path: &str, previous: &str, next: &str) -> String {
    if previous == next { return String::new(); }
    let old_lines = previous.lines().collect::<Vec<_>>();
    let new_lines = next.lines().collect::<Vec<_>>();
    let mut output = format!("--- a/{path}\n+++ b/{path}\n@@ -1,{} +1,{} @@\n", old_lines.len(), new_lines.len());
    for line in old_lines { output.push('-'); output.push_str(line); output.push('\n'); }
    for line in new_lines { output.push('+'); output.push_str(line); output.push('\n'); }
    output
}

fn validate_config_updates(updates: &BTreeMap<String, Value>) -> anyhow::Result<()> {
    for (key, value) in updates {
        match key.as_str() {
            "agent.checkpoint_interval_steps" => validate_u32(value, key, 1, 10_000)?,
            "agent.max_repeated_tool_calls" => validate_u32(value, key, 2, 100)?,
            "agent.max_stalled_checkpoints" => validate_u32(value, key, 1, 100)?,
            "agent.repair_review_after" => validate_u32(value, key, 1, 1_000)?,
            "agent.self_inspection_enabled" | "agent.experience_learning_enabled" | "agent.self_change_proposals_enabled" => {
                if !value.is_boolean() { anyhow::bail!("配置项 {key} 必须是布尔值"); }
            }
            _ => anyhow::bail!("不允许通过自身提案修改配置项: {key}"),
        }
    }
    Ok(())
}

fn validate_u32(value: &Value, key: &str, min: u64, max: u64) -> anyhow::Result<()> {
    let number = value.as_u64().ok_or_else(|| anyhow::anyhow!("配置项 {key} 必须是正整数"))?;
    if !(min..=max).contains(&number) { anyhow::bail!("配置项 {key} 必须位于 {min}..={max}"); }
    Ok(())
}

fn restore_optional_file(path: &Path, previous: Option<&[u8]>) -> anyhow::Result<()> {
    match previous {
        Some(bytes) => atomic_write(path, bytes),
        None if path.exists() => { fs::remove_file(path)?; Ok(()) }
        None => Ok(()),
    }
}

fn redact_endpoint(endpoint: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(endpoint) else { return "[invalid endpoint]".into() };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn collect_files(root: &Path, output: &mut Vec<PathBuf>, limit: usize) -> anyhow::Result<()> {
    if root.is_file() { output.push(root.to_path_buf()); return Ok(()); }
    for entry in fs::read_dir(root)? {
        if output.len() >= limit { break; }
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if BLOCKED_COMPONENTS.contains(&name.as_str()) || name.starts_with('.') { continue; }
        if path.is_dir() { collect_files(&path, output, limit)?; } else { output.push(path); }
    }
    Ok(())
}

fn rollback(backups: &[(PathBuf, Option<Vec<u8>>)]) -> anyhow::Result<()> {
    for (path, previous) in backups {
        match previous {
            Some(bytes) => atomic_write(path, bytes)?,
            None if path.exists() => fs::remove_file(path)?,
            None => {}
        }
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("路径缺少父目录"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.furina.tmp", path.file_name().unwrap_or_default().to_string_lossy()));
    fs::write(&temp, bytes)?;
    if path.exists() { fs::remove_file(path)?; }
    fs::rename(temp, path)?;
    Ok(())
}

async fn run_validation(command: &str, cwd: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    let mut child = { let mut command_process = Command::new("cmd"); command_process.args(["/C", command]); command_process };
    #[cfg(not(windows))]
    let mut child = { let mut command_process = Command::new("sh"); command_process.args(["-lc", command]); command_process };
    let output = tokio::time::timeout(std::time::Duration::from_secs(600), child.current_dir(cwd).output()).await
        .map_err(|_| anyhow::anyhow!("验证命令超时: {command}"))??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        anyhow::bail!("{command} 退出码 {:?}: {}{}", output.status.code(), truncate(&stdout, 2_000), truncate(&stderr, 2_000));
    }
    Ok(())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sanitize(text: &str, max: usize) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let lower = line.to_lowercase();
        if ["api_key", "apikey", "secret", "password", "token="].iter().any(|needle| lower.contains(needle)) {
            lines.push("[REDACTED]".to_string());
        } else {
            lines.push(line.to_string());
        }
    }
    truncate(&lines.join("\n"), max)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max { return text.to_string(); }
    text.chars().take(max).collect::<String>() + "...[truncated]"
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_millis()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspector(root: &Path) -> SelfInspector {
        SelfInspector::new(
            "development".into(), Some(root.to_path_buf()), root.join("self-manifest.json"), root.into(),
            "test".into(), Config::default(), root.join(".furina/config.yaml"), root.join(".furina/proposals"),
        )
    }

    #[test]
    fn source_allowlist_blocks_private_and_escape_paths() {
        let root = std::env::temp_dir().join(format!("furina_self_{}", now_ms()));
        fs::create_dir_all(root.join("crates/demo/src")).unwrap();
        fs::create_dir_all(root.join("persona")).unwrap();
        fs::write(root.join("crates/demo/src/lib.rs"), "pub fn ok() {}").unwrap();
        fs::write(root.join("persona/system.md"), "secret").unwrap();
        let inspector = inspector(&root);
        assert!(inspector.read_source("crates/demo/src/lib.rs", 1000).is_ok());
        assert!(inspector.read_source("persona/system.md", 1000).is_err());
        assert!(inspector.read_source("../secret.txt", 1000).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn approved_proposal_applies_and_hash_conflicts_are_rejected() {
        let root = std::env::temp_dir().join(format!("furina_proposal_{}", now_ms()));
        fs::create_dir_all(root.join("crates/demo/src")).unwrap();
        let target = root.join("crates/demo/src/lib.rs");
        fs::write(&target, "old").unwrap();
        let inspector = inspector(&root);
        let mut proposal = inspector.create_proposal(SelfChangeInput {
            problem: "update demo".into(), evidence: vec!["test".into()],
            changes: vec![ProposedFileChange { path: "crates/demo/src/lib.rs".into(), expected_sha256: String::new(), content: "new".into() }],
            config_updates: BTreeMap::new(), tests: vec![], risk: "low".into(), rollback: "restore".into(),
        }).unwrap();
        assert!(proposal.applicable);
        assert!(proposal.diffs["crates/demo/src/lib.rs"].contains("--- a/crates/demo/src/lib.rs"));
        assert!(proposal.diffs["crates/demo/src/lib.rs"].contains("+new"));
        inspector.apply_proposal(&mut proposal).await.unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");

        let mut conflict = inspector.create_proposal(SelfChangeInput {
            problem: "conflict".into(), evidence: vec![],
            changes: vec![ProposedFileChange { path: "crates/demo/src/lib.rs".into(), expected_sha256: String::new(), content: "proposal".into() }],
            config_updates: BTreeMap::new(), tests: vec![], risk: String::new(), rollback: String::new(),
        }).unwrap();
        fs::write(&target, "external change").unwrap();
        assert!(inspector.apply_proposal(&mut conflict).await.is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "external change");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn failed_validation_rolls_back_all_touched_files() {
        let root = std::env::temp_dir().join(format!("furina_rollback_{}", now_ms()));
        fs::create_dir_all(root.join("crates/demo/src")).unwrap();
        fs::create_dir_all(root.join(".furina")).unwrap();
        fs::write(root.join(".furina/config.yaml"), "sentinel: true\n").unwrap();
        let target = root.join("crates/demo/src/lib.rs");
        fs::write(&target, "old").unwrap();
        let inspector = inspector(&root);
        let mut proposal = inspector.create_proposal(SelfChangeInput {
            problem: "bad update".into(), evidence: vec![],
            changes: vec![ProposedFileChange { path: "crates/demo/src/lib.rs".into(), expected_sha256: String::new(), content: "new".into() }],
            config_updates: BTreeMap::new(), tests: vec!["exit 1".into()], risk: "test".into(), rollback: "automatic".into(),
        }).unwrap();
        assert!(inspector.apply_proposal(&mut proposal).await.is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
        assert_eq!(fs::read_to_string(root.join(".furina/config.yaml")).unwrap(), "sentinel: true\n");
        assert_eq!(proposal.status, "rolled_back");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn typed_agent_config_updates_apply_without_raw_private_file_access() {
        let root = std::env::temp_dir().join(format!("furina_config_proposal_{}", now_ms()));
        fs::create_dir_all(root.join("crates/demo/src")).unwrap();
        fs::create_dir_all(root.join(".furina")).unwrap();
        fs::write(root.join(".furina/config.yaml"), serde_yaml::to_string(&Config::default()).unwrap()).unwrap();
        let inspector = inspector(&root);
        let mut updates = BTreeMap::new();
        updates.insert("agent.checkpoint_interval_steps".into(), json!(48));
        let mut proposal = inspector.create_proposal(SelfChangeInput {
            problem: "adjust checkpoint".into(), evidence: vec![], changes: vec![],
            config_updates: updates, tests: vec![], risk: "low".into(), rollback: "restore config".into(),
        }).unwrap();
        assert!(proposal.applicable);
        inspector.apply_proposal(&mut proposal).await.unwrap();
        let loaded = Config::load(&root.join(".furina/config.yaml")).unwrap();
        assert_eq!(loaded.agent.checkpoint_interval_steps, 48);

        let mut invalid = BTreeMap::new();
        invalid.insert("llm.api_key".into(), json!("forbidden"));
        assert!(inspector.create_proposal(SelfChangeInput {
            problem: "bad config".into(), evidence: vec![], changes: vec![], config_updates: invalid,
            tests: vec![], risk: String::new(), rollback: String::new(),
        }).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installed_mode_exports_but_never_applies_proposals() {
        let root = std::env::temp_dir().join(format!("furina_installed_proposal_{}", now_ms()));
        let inspector = SelfInspector::new(
            "installed".into(), None, PathBuf::new(), PathBuf::from("workspace"), "test".into(),
            Config::default(), root.join("config.yaml"), root.clone(),
        );
        let proposal = inspector.create_proposal(SelfChangeInput {
            problem: "installed issue".into(), evidence: vec![],
            changes: vec![ProposedFileChange { path: "crates/demo/src/lib.rs".into(), expected_sha256: String::new(), content: "new".into() }],
            config_updates: BTreeMap::new(), tests: vec![], risk: String::new(), rollback: String::new(),
        }).unwrap();
        assert!(!proposal.applicable);
        assert_eq!(proposal.status, "exported");
        assert!(root.join(&proposal.id).join("proposal.json").is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn status_never_contains_environment_secret_value() {
        let mut config = Config::default();
        config.llm.providers[0].api_key_env = "FURINA_SELF_TEST_SECRET".into();
        config.llm.providers[0].base_url = "https://user:password@example.com/v1?token=do-not-leak".into();
        unsafe { std::env::set_var("FURINA_SELF_TEST_SECRET", "do-not-leak"); }
        let inspector = SelfInspector::new("installed".into(), None, PathBuf::new(), PathBuf::from("workspace"), "test".into(), config, PathBuf::from("config.yaml"), PathBuf::from("proposals"));
        let status = inspector.status().to_string();
        assert!(!status.contains("do-not-leak"));
        assert!(!status.contains("password"));
        assert!(!status.contains("user@"));
        unsafe { std::env::remove_var("FURINA_SELF_TEST_SECRET"); }
    }
}
