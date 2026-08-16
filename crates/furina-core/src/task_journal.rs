use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const TASK_JOURNAL_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CompletedAction {
    pub fingerprint: String,
    pub tool: String,
    pub result_fingerprint: String,
    pub summary: String,
    pub completed_at_ms: u128,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskCheckpointRecord {
    pub schema_version: u32,
    pub task_id: String,
    pub original_goal: String,
    pub status: String,
    pub checkpoint_count: u32,
    pub steps: u32,
    pub total_tokens: u64,
    pub repair_rounds: u32,
    pub token_budget_limit: u64,
    pub summary: String,
    pub blocking: String,
    pub wrote_files: bool,
    pub verified: bool,
    pub test_command: String,
    pub scanned: bool,
    pub known_hashes: BTreeMap<String, String>,
    pub completed_actions: Vec<CompletedAction>,
    pub failure_evidence: Vec<String>,
    pub tool_patterns: Vec<String>,
    pub app_version: String,
    pub updated_at_ms: u128,
}

impl TaskCheckpointRecord {
    pub fn is_recoverable(&self) -> bool {
        matches!(self.status.as_str(), "active" | "checkpoint" | "awaiting_approval" | "paused" | "interrupted")
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TaskRecoverySummary {
    pub task_id: String,
    pub goal: String,
    pub status: String,
    pub checkpoint_count: u32,
    pub steps: u32,
    pub updated_at_ms: u128,
}

impl From<&TaskCheckpointRecord> for TaskRecoverySummary {
    fn from(record: &TaskCheckpointRecord) -> Self {
        Self {
            task_id: record.task_id.clone(),
            goal: sanitize_text(&record.original_goal, 240),
            status: record.status.clone(),
            checkpoint_count: record.checkpoint_count,
            steps: record.steps,
            updated_at_ms: record.updated_at_ms,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskJournalStore {
    path: PathBuf,
}

impl TaskJournalStore {
    pub fn open(agent_dir: &Path) -> Self {
        Self { path: agent_dir.join("active-task.json") }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Option<TaskCheckpointRecord> {
        let record = fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str::<TaskCheckpointRecord>(&text).ok())?;
        (record.schema_version == TASK_JOURNAL_SCHEMA_VERSION && record.is_recoverable()).then_some(record)
    }

    pub fn summary(&self) -> Option<TaskRecoverySummary> {
        self.load().as_ref().map(TaskRecoverySummary::from)
    }

    pub fn save(&self, record: &TaskCheckpointRecord) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(record)?;
        atomic_write(&self.path, &bytes)
    }

    pub fn clear(&self) -> anyhow::Result<()> {
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}

pub fn sanitize_text(text: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for line in text.lines() {
        let lower = line.to_lowercase();
        let sensitive = [
            "api_key", "apikey", "secret", "password", "passwd", "authorization", "bearer ", "token=", "private_key",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
        if sensitive {
            output.push_str("[REDACTED]\n");
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    let mut output = output.trim().chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        output.push_str("...[truncated]");
    }
    output
}

fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("任务日志路径缺少父目录"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", path.file_name().unwrap_or_default().to_string_lossy()));
    fs::write(&temp, bytes)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("furina-task-journal-{}-{stamp}-{name}", std::process::id()))
    }

    fn record() -> TaskCheckpointRecord {
        TaskCheckpointRecord {
            schema_version: TASK_JOURNAL_SCHEMA_VERSION,
            task_id: "task_1".into(),
            original_goal: "完成长任务".into(),
            status: "checkpoint".into(),
            checkpoint_count: 2,
            steps: 64,
            total_tokens: 5000,
            repair_rounds: 1,
            token_budget_limit: 10_000,
            summary: "已完成扫描".into(),
            blocking: "无".into(),
            wrote_files: true,
            verified: false,
            test_command: "cargo test".into(),
            scanned: true,
            known_hashes: BTreeMap::new(),
            completed_actions: Vec::new(),
            failure_evidence: Vec::new(),
            tool_patterns: vec!["fs_read_file".into()],
            app_version: env!("CARGO_PKG_VERSION").into(),
            updated_at_ms: 1,
        }
    }

    #[test]
    fn journal_round_trip_and_clear() {
        let dir = temp_dir("roundtrip");
        let store = TaskJournalStore::open(&dir);
        store.save(&record()).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.task_id, "task_1");
        assert_eq!(store.summary().unwrap().steps, 64);
        store.clear().unwrap();
        assert!(store.load().is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn journal_redacts_secret_lines() {
        let sanitized = sanitize_text("正常内容\nAPI_KEY=should-not-leak\n继续", 200);
        assert!(sanitized.contains("正常内容"));
        assert!(sanitized.contains("[REDACTED]"));
        assert!(!sanitized.contains("should-not-leak"));
    }
}
