//! 关系引擎：里程碑 + 由信任值推导关系阶段。

use crate::config::{RelationshipModelConfig, StageConfig};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RelationshipState {
    pub milestones: Vec<String>,
    #[serde(default)]
    pub milestone_times: Vec<u128>,
    #[serde(default)]
    pub interaction_count: u64,
    #[serde(default)]
    pub task_count: u64,
    #[serde(default)]
    pub confide_count: u64,
    #[serde(default)]
    pub praise_count: u64,
    #[serde(default)]
    pub refuse_count: u64,
    #[serde(default)]
    pub relation_log: Vec<RelationEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationEvent {
    pub ts: u128,
    pub text: String,
}

impl RelationshipState {
    pub fn stage(&self, cfg: &RelationshipModelConfig, trust: f64) -> StageConfig {
        cfg.stages
            .iter()
            .rev()
            .find(|s| trust >= s.trust_min)
            .cloned()
            .unwrap_or_else(StageConfig::default)
    }

    pub fn add_milestone(&mut self, milestone: impl Into<String>) {
        let m = milestone.into();
        if !self.milestones.contains(&m) {
            self.milestones.push(m);
            self.milestone_times.push(crate::now_ms());
        }
    }

    /// 记录一条关系事件（保留最近 40 条，供关系面板展示）。
    pub fn log(&mut self, text: impl Into<String>) {
        self.relation_log.push(RelationEvent {
            ts: crate::now_ms(),
            text: text.into(),
        });
        if self.relation_log.len() > 40 {
            self.relation_log.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load;

    #[test]
    fn stage_progresses_with_trust() {
        let cfg = load();
        let r = RelationshipState::default();
        assert_eq!(r.stage(&cfg.relationship_model, 10.0).id, "stranger");
        assert_eq!(r.stage(&cfg.relationship_model, 40.0).id, "familiar");
        assert_eq!(r.stage(&cfg.relationship_model, 90.0).id, "partner");
        assert!(!r.stage(&cfg.relationship_model, 10.0).refusal.is_empty());
    }

    #[test]
    fn milestone_deduplicates() {
        let mut r = RelationshipState::default();
        r.add_milestone("第一次共同完成项目");
        r.add_milestone("第一次共同完成项目");
        assert_eq!(r.milestones.len(), 1);
        assert_eq!(r.milestone_times.len(), 1, "里程碑时间应与里程碑一一对应");
    }

    #[test]
    fn relation_log_trims_and_orders() {
        let mut r = RelationshipState::default();
        for i in 0..45 {
            r.log(format!("事件 {i}"));
        }
        assert_eq!(r.relation_log.len(), 40);
        assert!(!r.relation_log.iter().any(|e| e.text == "事件 0"), "最早的应被裁剪");
        assert!(r.relation_log.iter().any(|e| e.text == "事件 44"));
    }
}
