use furina_proto::Event;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuditRecord {
    pub timestamp_ms: u128,
    pub task_id: Option<String>,
    pub category: String,
    pub action: String,
    pub outcome: String,
    pub detail_fingerprint: String,
    pub detail_summary: String,
    pub app_version: String,
}

#[derive(Debug, Clone)]
pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    pub fn open(agent_dir: &Path) -> Self {
        Self { path: agent_dir.join("audit.jsonl") }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn record_event(&self, task_id: Option<&str>, event: &Event) -> anyhow::Result<()> {
        let Some((category, action, outcome, detail)) = event_fields(event) else {
            return Ok(());
        };
        self.append(AuditRecord {
            timestamp_ms: now_ms(),
            task_id: task_id.map(str::to_string),
            category: category.into(),
            action,
            outcome,
            detail_fingerprint: fingerprint(&detail),
            detail_summary: safe_summary(&detail),
            app_version: env!("CARGO_PKG_VERSION").into(),
        })
    }

    pub fn append(&self, record: AuditRecord) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        serde_json::to_writer(&mut file, &record)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }

    pub fn tail(&self, limit: usize) -> Vec<AuditRecord> {
        let Ok(text) = fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        let mut records = text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect::<Vec<_>>();
        if records.len() > limit {
            records.drain(..records.len() - limit);
        }
        records
    }
}

fn event_fields(event: &Event) -> Option<(&'static str, String, String, String)> {
    match event {
        Event::ToolCall { name, summary } => Some(("tool", name.clone(), "requested".into(), format!("{} bytes", summary.len()))),
        Event::ToolResult { name, ok, summary } => Some(("tool", name.clone(), if *ok { "success".into() } else { "failure".into() }, summary.clone())),
        Event::ApprovalRequired { kind, detail } => Some(("approval", kind.clone(), "required".into(), format!("{} bytes", detail.len()))),
        Event::ApprovalGranted { kind } => Some(("approval", kind.clone(), "granted".into(), String::new())),
        Event::ApprovalDenied { kind } => Some(("approval", kind.clone(), "denied".into(), String::new())),
        Event::Checkpoint { sequence, reason, summary, .. } => Some(("task", format!("checkpoint_{sequence}"), reason.clone(), summary.clone())),
        Event::SelfChangeProposed { id, summary, targets, applicable } => Some((
            "self_change",
            id.clone(),
            if *applicable { "pending_approval".into() } else { "exported".into() },
            format!("{}; targets={}", summary, targets.join(",")),
        )),
        Event::SelfChangeApplied { id, success, summary } => Some((
            "self_change",
            id.clone(),
            if *success { "applied".into() } else { "rejected_or_rolled_back".into() },
            summary.clone(),
        )),
        Event::Done { success, summary } => Some(("task", "done".into(), if *success { "success".into() } else { "failure".into() }, summary.clone())),
        Event::TaskRecoveryAvailable { task_id, goal, .. } => Some(("recovery", task_id.clone(), "available".into(), goal.clone())),
        Event::TaskRecoveryResumed { task_id, .. } => Some(("recovery", task_id.clone(), "resumed".into(), String::new())),
        Event::TaskRecoveryDiscarded { task_id } => Some(("recovery", task_id.clone(), "discarded".into(), String::new())),
        Event::DiagnosticExported { path } => Some(("diagnostic", "export".into(), "success".into(), path.clone())),
        _ => None,
    }
}

fn safe_summary(detail: &str) -> String {
    if detail.is_empty() {
        return String::new();
    }
    let lower = detail.to_lowercase();
    if ["api_key", "apikey", "secret", "password", "authorization", "bearer ", "token="].iter().any(|needle| lower.contains(needle)) {
        return "[REDACTED]".into();
    }
    let looks_like_content = detail.contains("@@ -") || detail.lines().count() > 8 || detail.len() > 800;
    if looks_like_content {
        return format!("[CONTENT OMITTED: {} bytes]", detail.len());
    }
    detail.chars().take(300).collect()
}

fn fingerprint(detail: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(detail.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_omits_source_and_secrets() {
        let dir = std::env::temp_dir().join(format!("furina-audit-{}-{}", std::process::id(), now_ms()));
        let log = AuditLog::open(&dir);
        log.record_event(Some("task_1"), &Event::ApprovalRequired {
            kind: "写入文件".into(),
            detail: "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-secret\n+API_KEY=should-not-leak".into(),
        }).unwrap();
        let records = log.tail(10);
        assert_eq!(records.len(), 1);
        assert!(!records[0].detail_summary.contains("should-not-leak"));
        assert!(!records[0].detail_summary.contains("--- a/file"));
        assert_eq!(records[0].outcome, "required");
        let _ = fs::remove_dir_all(dir);
    }
}
