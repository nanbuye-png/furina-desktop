use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExperienceRecord {
    pub id: String,
    pub key: String,
    pub task_category: String,
    pub outcome: String,
    pub tool_patterns: Vec<String>,
    pub failure_evidence: Vec<String>,
    pub lesson: String,
    pub applicability: String,
    pub confidence: f64,
    pub occurrences: u32,
    pub version: String,
    pub updated_at_ms: u128,
}

#[derive(Debug, Clone)]
pub struct TaskTrace {
    pub task: String,
    pub success: bool,
    pub summary: String,
    pub tool_patterns: Vec<String>,
    pub failure_evidence: Vec<String>,
    pub repair_rounds: u32,
    pub checkpoint_count: u32,
    pub lesson_override: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AgentExperienceStore {
    path: PathBuf,
    records: Vec<ExperienceRecord>,
}

impl AgentExperienceStore {
    pub fn open(dir: &Path) -> Self {
        let path = dir.join("experience.jsonl");
        let records = fs::read_to_string(&path).ok().map(|text| {
            text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
        }).unwrap_or_default();
        Self { path, records }
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn context_for(&self, task: &str, limit: usize, max_chars: usize) -> String {
        let terms = terms(task);
        let mut scored = self.records.iter().map(|record| {
            let haystack = format!("{} {} {} {}", record.task_category, record.lesson, record.applicability, record.tool_patterns.join(" ")).to_lowercase();
            let overlap = terms.iter().filter(|term| haystack.contains(term.as_str())).count() as f64;
            (record, overlap + record.confidence + (record.occurrences.min(5) as f64 * 0.1))
        }).filter(|(_, score)| *score > 0.5).collect::<Vec<_>>();
        scored.sort_by(|left, right| right.1.partial_cmp(&left.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut output = String::new();
        for (record, _) in scored.into_iter().take(limit) {
            let line = format!("- [{}] {}（适用：{}，置信度 {:.2}）
", record.task_category, record.lesson, record.applicability, record.confidence);
            if output.chars().count() + line.chars().count() > max_chars { break; }
            output.push_str(&line);
        }
        if output.is_empty() { String::new() } else { format!("[Agent经验]
{output}") }
    }

    pub fn record(&mut self, trace: TaskTrace) -> anyhow::Result<ExperienceRecord> {
        let category = classify_task(&trace.task);
        let tools = trace.tool_patterns.into_iter().collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
        let failures = trace.failure_evidence.into_iter().map(|item| sanitize(&item, 500)).collect::<Vec<_>>();
        let evidence_key = failures.first().cloned().unwrap_or_else(|| tools.join(","));
        let key = format!("{}:{}", category, normalize_key(&evidence_key));
        let lesson = if let Some(lesson) = trace.lesson_override.filter(|lesson| !lesson.trim().is_empty()) {
            sanitize(&lesson, 800)
        } else if trace.success {
            if trace.repair_rounds > 0 { format!("经过 {} 轮修复后验证成功；后续先复用已验证路径。", trace.repair_rounds) }
            else { format!("使用 {} 完成任务并验证成功。", if tools.is_empty() { "无工具".into() } else { tools.join("、") }) }
        } else if let Some(failure) = failures.first() {
            format!("任务未完成，优先避免重复失败：{}", sanitize(failure, 240))
        } else {
            format!("任务未完成；重新确认目标、权限和验证条件后再继续。")
        };
        let now = now_ms();
        if let Some(existing) = self.records.iter_mut().find(|record| record.key == key) {
            existing.occurrences += 1;
            existing.confidence = if trace.success { (existing.confidence + 0.08).min(0.95) } else { (existing.confidence - 0.04).max(0.2) };
            existing.outcome = if trace.success { "success".into() } else { "failure".into() };
            existing.tool_patterns = tools;
            existing.failure_evidence = failures;
            existing.lesson = lesson;
            existing.updated_at_ms = now;
            let record = existing.clone();
            self.save()?;
            return Ok(record);
        }
        let record = ExperienceRecord {
            id: format!("exp_{}_{}", now, self.records.len() + 1),
            key,
            task_category: category,
            outcome: if trace.success { "success".into() } else { "failure".into() },
            tool_patterns: tools,
            failure_evidence: failures,
            lesson,
            applicability: sanitize(&trace.task, 180),
            confidence: if trace.success { 0.65 } else { 0.5 },
            occurrences: 1,
            version: env!("CARGO_PKG_VERSION").into(),
            updated_at_ms: now,
        };
        self.records.push(record.clone());
        if self.records.len() > 500 { self.records.sort_by_key(|record| std::cmp::Reverse(record.updated_at_ms)); self.records.truncate(500); }
        self.save()?;
        Ok(record)
    }

    pub fn proposal_candidate(&self, record: &ExperienceRecord) -> bool {
        record.outcome == "failure" && record.occurrences >= 2 && !record.failure_evidence.is_empty()
    }

    fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() { fs::create_dir_all(parent)?; }
        let mut output = String::new();
        for record in &self.records { output.push_str(&serde_json::to_string(record)?); output.push('\n'); }
        let temp = self.path.with_extension("jsonl.tmp");
        fs::write(&temp, output)?;
        if self.path.exists() { fs::remove_file(&self.path)?; }
        fs::rename(temp, &self.path)?;
        Ok(())
    }
}

fn classify_task(task: &str) -> String {
    let lower = task.to_lowercase();
    if ["test", "测试", "修复", "bug", "失败"].iter().any(|needle| lower.contains(needle)) { "repair".into() }
    else if ["配置", "config", "设置"].iter().any(|needle| lower.contains(needle)) { "configuration".into() }
    else if ["搜索", "查询", "read", "检查", "inspect"].iter().any(|needle| lower.contains(needle)) { "inspection".into() }
    else { "general".into() }
}

fn terms(text: &str) -> BTreeSet<String> {
    text.split(|character: char| !character.is_alphanumeric() && character != '_')
        .map(|part| part.trim().to_lowercase()).filter(|part| part.chars().count() >= 2).collect()
}

fn normalize_key(text: &str) -> String {
    text.to_lowercase().chars().filter(|character| character.is_alphanumeric() || *character == '_' || character.is_whitespace()).take(160).collect()
}

fn sanitize(text: &str, max: usize) -> String {
    let mut output = String::new();
    for line in text.lines() {
        let lower = line.to_lowercase();
        if ["api_key", "apikey", "secret", "password", "token="].iter().any(|needle| lower.contains(needle)) { output.push_str("[REDACTED]
"); }
        else { output.push_str(line); output.push('\n'); }
    }
    output.chars().take(max).collect::<String>().trim().to_string()
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_millis()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_deduplicate_and_trigger_candidate_after_two_failures() {
        let dir = std::env::temp_dir().join(format!("furina_exp_{}", now_ms()));
        let mut store = AgentExperienceStore::open(&dir);
        let trace = TaskTrace { task: "修复测试".into(), success: false, summary: "failed".into(), tool_patterns: vec!["term_run".into()], failure_evidence: vec!["same failure".into()], repair_rounds: 1, checkpoint_count: 0, lesson_override: None };
        let first = store.record(trace.clone()).unwrap();
        assert!(!store.proposal_candidate(&first));
        let second = store.record(trace).unwrap();
        assert!(store.proposal_candidate(&second));
        assert_eq!(second.occurrences, 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn sanitizes_secret_like_evidence() {
        let dir = std::env::temp_dir().join(format!("furina_exp_secret_{}", now_ms()));
        let mut store = AgentExperienceStore::open(&dir);
        let record = store.record(TaskTrace { task: "配置".into(), success: false, summary: String::new(), tool_patterns: vec![], failure_evidence: vec!["api_key=do-not-store".into()], repair_rounds: 0, checkpoint_count: 0, lesson_override: None }).unwrap();
        assert!(!serde_json::to_string(&record).unwrap().contains("do-not-store"));
        let _ = fs::remove_dir_all(dir);
    }
}
