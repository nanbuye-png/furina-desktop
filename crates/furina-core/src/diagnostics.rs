use crate::app::RuntimePaths;
use crate::audit::{AuditLog, AuditRecord};
use crate::config::Config;
use crate::experience::AgentExperienceStore;
use crate::task_journal::{TaskJournalStore, TaskRecoverySummary};
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

const DIAGNOSTIC_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticSnapshot {
    pub schema_version: u32,
    pub id: String,
    pub created_at_ms: u128,
    pub app_version: String,
    pub runtime: serde_json::Value,
    pub config: serde_json::Value,
    pub task: Option<TaskRecoverySummary>,
    pub audit_tail: Vec<AuditRecord>,
    pub experience_count: usize,
    pub proposal_count: usize,
}

pub fn export(paths: &RuntimePaths, config: &Config, sidecar_version: Option<&str>) -> anyhow::Result<PathBuf> {
    let created_at_ms = now_ms();
    let id = format!("diag_{created_at_ms}_{}", std::process::id());
    let journal = TaskJournalStore::open(&paths.agent_dir());
    let audit = AuditLog::open(&paths.agent_dir());
    let experience = AgentExperienceStore::open(&paths.agent_dir());
    let snapshot = DiagnosticSnapshot {
        schema_version: DIAGNOSTIC_SCHEMA_VERSION,
        id: id.clone(),
        created_at_ms,
        app_version: env!("CARGO_PKG_VERSION").into(),
        runtime: json!({
            "mode": paths.mode,
            "workspace_configured": paths.workspace_root.is_dir(),
            "source_access": paths.source_root.is_some(),
            "sidecar": {
                "available": paths.sidecar.available(),
                "description": paths.sidecar.description(),
                "version": sidecar_version.unwrap_or("unknown"),
            },
        }),
        config: config_summary(config),
        task: journal.summary(),
        audit_tail: audit.tail(40),
        experience_count: experience.len(),
        proposal_count: count_proposals(&paths.proposals_dir()),
    };
    let directory = paths.agent_dir().join("diagnostics").join(&id);
    fs::create_dir_all(&directory)?;
    let output = directory.join("diagnostic.json");
    atomic_write(&output, &serde_json::to_vec_pretty(&snapshot)?)?;
    Ok(output)
}

fn config_summary(config: &Config) -> serde_json::Value {
    let providers = config.llm.providers.iter().map(|provider| {
        json!({
            "id": provider.id,
            "label": provider.label,
            "model": provider.model,
            "vision": provider.vision,
            "api_key_configured": std::env::var_os(&provider.api_key_env).is_some(),
        })
    }).collect::<Vec<_>>();
    json!({
        "persona": "[OMITTED]",
        "model": config.model,
        "active_provider": config.llm.active_provider,
        "providers": providers,
        "agent": {
            "checkpoint_interval_steps": config.agent.checkpoint_interval_steps,
            "max_repeated_tool_calls": config.agent.max_repeated_tool_calls,
            "max_stalled_checkpoints": config.agent.max_stalled_checkpoints,
            "repair_review_after": config.agent.repair_review_after,
            "self_inspection_enabled": config.agent.self_inspection_enabled,
            "experience_learning_enabled": config.agent.experience_learning_enabled,
            "self_change_proposals_enabled": config.agent.self_change_proposals_enabled,
        },
        "approval_mode": config.approval.mode,
        "voice_enabled": config.voice.enabled,
        "asr_enabled": config.asr.enabled,
        "vision_enabled": config.vision.enabled,
        "web_backend": config.web.search_backend,
    })
}

fn count_proposals(path: &Path) -> usize {
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter(|entry| entry.path().is_dir())
        .count()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("诊断路径缺少父目录"))?;
    let temp = parent.join(format!(".{}.tmp", path.file_name().unwrap_or_default().to_string_lossy()));
    fs::write(&temp, bytes)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temp, path)?;
    Ok(())
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
    use crate::sidecar::SidecarLaunch;

    #[test]
    fn diagnostic_summary_omits_persona_and_secret_fields() {
        let summary = config_summary(&Config::default()).to_string();
        assert!(summary.contains("[OMITTED]"));
        assert!(!summary.contains("api_key_env"));
        assert!(!summary.contains("base_url"));
    }

    #[test]
    fn export_writes_only_redacted_snapshot() {
        let root = std::env::temp_dir().join(format!("furina-diagnostics-{}", now_ms()));
        let paths = RuntimePaths {
            mode: "development".into(),
            resource_root: root.join("resources"),
            data_root: root.clone(),
            workspace_root: root.join("workspace"),
            source_root: Some(root.join("source")),
            self_manifest_path: root.join("self-manifest.json"),
            sidecar: SidecarLaunch::Disabled(root.join("missing")),
        };
        let output = export(&paths, &Config::default(), None).unwrap();
        let text = fs::read_to_string(output).unwrap();
        assert!(text.contains("diag_"));
        assert!(!text.contains("secrets.env"));
        assert!(!text.contains("Soul"));
        let _ = fs::remove_dir_all(root);
    }
}
