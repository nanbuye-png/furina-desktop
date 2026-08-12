//! Furina Soul Engine：人格操作系统第一版落地。
//!
//! 纯人格层：只维护情绪/关系/记忆/行为意图，输出语气与行为倾向；
//! 永不触碰权限、工具执行与安全策略（安全边界在 furina-core）。

mod config;
mod emotion;
mod memory;
mod proactive;
mod relationship;

use config::{detect_text_trigger, SoulConfig, StageConfig};
use furina_proto::Event;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub use config::MoodKind;
pub use emotion::EmotionState;
pub use memory::MemoryRecord;
pub use memory::MemoryDraft;
pub use memory::MemoryStore;
pub use proactive::ProactiveEvent;
pub use relationship::RelationshipState;
pub use relationship::RelationEvent;

pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[derive(Debug, Clone, Default)]
pub struct IntentInfo {
    pub intent: String,
    pub cause: Option<String>,
    pub value: Option<String>,
}

/// 灵魂状态门面：情绪 + 关系 + 记忆 + 最近行为意图。
pub struct Soul {
    cfg: SoulConfig,
    pub emotion: EmotionState,
    pub relationship: RelationshipState,
    pub memory: MemoryStore,
    pub last_intent: Option<IntentInfo>,
    pub last_interaction_ms: Option<u128>,
    proactive_fired: HashMap<String, u128>,
    goals_asked: HashMap<String, u128>,
    tool_turns: u32,
    test_failures: u32,
    dir: PathBuf,
    fail_streak: u32,
    task_used_tools: bool,
    last_stage_id: Option<String>,
    dirty: bool,
}

impl Soul {
    pub fn load(dir: PathBuf) -> Self {
        let cfg = config::load();
        let now = now_ms();
        let emotion = load_emotion(&dir)
            .unwrap_or_else(|| EmotionState::from_config(&cfg.emotion_model, now))
            .decayed(&cfg.emotion_model, now);
        let relationship = load_relationship(&dir);
        let memory = MemoryStore::load(&dir);
        let meta = load_meta(&dir);
        Self {
            cfg,
            emotion,
            relationship,
            memory,
            last_intent: None,
            last_interaction_ms: meta.last_interaction_ms,
            proactive_fired: meta.proactive_fired,
            goals_asked: meta.goals_asked,
            tool_turns: 0,
            test_failures: 0,
            dir,
            fail_streak: 0,
            task_used_tools: false,
            last_stage_id: None,
            dirty: false,
        }
    }

    pub fn mood(&self) -> MoodKind {
        self.emotion.mood(&self.cfg.emotion_model)
    }

    pub fn stage(&self) -> StageConfig {
        self.relationship.stage(&self.cfg.relationship_model, self.emotion.trust)
    }

    /// 表达策略（natural / playful / flustered / gentle / serious / theatrical）：
    /// 由 behavior_rules.yaml 的 expression_strategies 规则按意图×心情×信任选择；
    /// 只影响语气与表达风格，不影响任何判断。
    pub fn expression_strategy(&self) -> String {
        let mood = self.mood().as_str();
        let trust = self.emotion.trust;
        let intent = self.last_intent.as_ref().map(|info| info.intent.as_str());
        for s in self
            .cfg
            .behavior_rules
            .expression_strategies
            .iter()
            .filter(|strategy| !strategy.is_default)
        {
            let mood_ok = s.moods.is_empty() || s.moods.iter().any(|m| m == mood);
            let intent_ok = s.intents.is_empty()
                || intent.is_some_and(|current| s.intents.iter().any(|item| item == current));
            let min_ok = s.min_trust.map_or(true, |t| trust >= t);
            let max_ok = s.max_trust.map_or(true, |t| trust <= t);
            if mood_ok && intent_ok && min_ok && max_ok {
                return s.id.clone();
            }
        }
        self.cfg
            .behavior_rules
            .expression_strategies
            .iter()
            .find(|s| s.is_default)
            .map(|s| s.id.clone())
            .unwrap_or_else(|| "natural".into())
    }

    /// 下一关系阶段与还差多少信任（None 表示已是最高阶段）。
    pub fn next_stage(&self) -> Option<(String, f64)> {
        self.cfg
            .relationship_model
            .stages
            .iter()
            .find(|s| s.trust_min > self.emotion.trust)
            .map(|s| (s.label.clone(), s.trust_min - self.emotion.trust))
    }

    /// 用户文本 → 情绪增量 + 行为意图 + 情感记忆。
    pub fn observe_text(&mut self, text: &str) -> bool {
        self.last_interaction_ms = Some(now_ms());
        self.relationship.interaction_count += 1;
        let Some(t) = detect_text_trigger(text, &self.cfg.behavior_rules) else {
            self.last_intent = None;
            self.dirty = true;
            return false;
        };
        match t.id.as_str() {
            "praise" => self.relationship.praise_count += 1,
            "confide" => self.relationship.confide_count += 1,
            "user_demeaning" => self.relationship.refuse_count += 1,
            _ => {}
        }
        self.emotion.apply_deltas(&t.deltas);
        let log_text = match t.id.as_str() {
            "praise" => "被夸奖：心情转好，信任提升",
            "scold" => "被数落：有点委屈，信任微降",
            "user_demeaning" => "被羞辱/强迫：守住底线，信任明显下降",
            "confide" => "被倾诉：关系更近，信任提升",
            "sad" => "用户情绪低落：我想认真安慰她",
            "happy" => "用户心情不错：气氛轻松",
            "annoyed" => "用户有些烦躁：我先稳住",
            _ => "",
        };
        if !log_text.is_empty() {
            self.relationship.log(log_text);
        }
        self.last_intent = Some(IntentInfo {
            intent: t.intent.clone(),
            cause: Some(t.cause.clone()),
            value: t.value.clone(),
        });
        let (importance, valence, note) = match t.id.as_str() {
            "praise" => (60, 1i8, "（当时嘴上得意，心里其实很开心）"),
            "scold" => (65, -1i8, "（有点委屈，但没记仇）"),
            "confide" => (70, 0i8, "（我认真听了）"),
            "sad" => (55, -1i8, "（我想认真安慰她）"),
            "happy" => (50, 1i8, "（她的好心情也感染了我）"),
            "annoyed" => (45, -1i8, "（她有点烦躁，我先稳住）"),
            "user_demeaning" => (75, -1i8, "（我不太开心，但守住了底线）"),
            _ => (40, 0i8, ""),
        };
        if t.id != "theatrical_request" {
            self.memory.add_unique(
                "emotional",
                format!("{} {note}", t.cause),
                importance,
                valence,
                vec![t.id.clone()],
            );
        }
        self.maybe_milestone();
        self.dirty = true;
        true
    }

    /// 事件流 → 情绪增量 + 行为意图 + 里程碑/事件记忆。
    pub fn observe_event(&mut self, event: &Event) {
        match event {
            Event::SessionStarted { .. } => {
                self.fail_streak = 0;
                self.task_used_tools = false;
            }
            Event::ToolCall { .. } => {
                self.task_used_tools = true;
                self.tool_turns += 1;
            }
            Event::ToolResult { name, ok: true, summary, .. } if name == "web_open" => {
                // Web Intelligence Phase 2：打开过的网页记入灵魂记忆（轻量引用）。
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(summary) {
                    if let Some(url) = v["url"].as_str() {
                        self.memory.add_unique(
                            "episodic",
                            format!("曾查阅网页：{url}"),
                            50,
                            0,
                            vec!["web".into(), "read".into()],
                        );
                        self.dirty = true;
                    }
                }
            }
            Event::TestReport { passed: false, .. } => self.test_failures += 1,
            Event::TestReport { passed: true, .. } => self.test_failures = 0,
            Event::Verify { passed: false, .. } => {
                self.fail_streak += 1;
                let id = if self.fail_streak >= 2 { "verify_fail_again" } else { "verify_fail_first" };
                self.apply_event_trigger(id);
            }
            Event::Verify { passed: true, .. } => {
                let id = if self.fail_streak >= 1 { "verify_pass_after_fail" } else { "verify_pass_first" };
                self.fail_streak = 0;
                self.apply_event_trigger(id);
            }
            Event::ApprovalDenied { .. } => {
                self.apply_event_trigger("approval_denied");
                self.relationship.log("审批被拒绝：我尊重用户决定");
            }
            Event::Done { success: true, .. } => {
                self.apply_event_trigger("task_success");
                self.relationship.task_count += 1;
                self.relationship.log("共同完成了一次任务");
                if self.task_used_tools {
                    self.memory.add_unique(
                        "episodic",
                        "共同完成了一次任务",
                        50,
                        1,
                        vec!["task".into(), "success".into()],
                    );
                }
            }
            Event::Done { success: false, .. } => {
                self.apply_event_trigger("task_fail");
                self.relationship.log("一次任务未能完成");
                if self.task_used_tools {
                    self.memory.add_unique(
                        "episodic",
                        "一次任务未能完成",
                        45,
                        -1,
                        vec!["task".into(), "fail".into()],
                    );
                }
            }
            _ => {}
        }
    }

    pub fn set_interrupted(&mut self) {
        self.apply_event_trigger("interrupted");
    }

    /// 显式记住一条用户事实（semantic memory）。
    pub fn remember_semantic(&mut self, content: &str) {
        let importance = MemoryStore::compute_importance(&self.cfg.memory_scoring, 0.9, 0.3, 0.3, 0.6);
        self.memory.add(
            "semantic",
            format!("用户明确让我记住：{content}"),
            importance,
            0,
            vec!["user_stated".into()],
        );
        self.last_intent = Some(IntentInfo {
            intent: "remember_fact".into(),
            cause: Some("用户让我记住一件事".into()),
            value: Some("sincerity".into()),
        });
        self.dirty = true;
    }

    /// 批量加入记忆候选（LLM 抽取结果），内部做去重合并。
    pub fn add_memories(&mut self, drafts: Vec<MemoryDraft>) {
        if drafts.is_empty() {
            return;
        }
        self.memory.add_drafts(drafts);
        self.dirty = true;
    }

    /// 清空所有记忆（关系/情绪状态保留）。
    pub fn clear_memories(&mut self) {
        self.memory.clear();
        self.dirty = true;
    }

    /// 全部记忆，按时间倒序。
    pub fn all_memories(&self) -> Vec<&MemoryRecord> {
        let mut v: Vec<&MemoryRecord> = self.memory.records.iter().collect();
        v.sort_by_key(|r| std::cmp::Reverse(r.timestamp));
        v
    }

    /// 按 id 删除单条记忆；返回是否删除成功。
    pub fn remove_memory(&mut self, id: &str) -> bool {
        let ok = self.memory.remove(id);
        if ok {
            self.dirty = true;
        }
        ok
    }

    /// 动态人格注入块（替换 v1 的单行心情提示）。
    pub fn context_block(&self) -> String {
        let mood = self.mood();
        let mut lines = vec![String::from("[灵魂状态]")];
        lines.push(format!(
            "身份：{}——{}",
            self.cfg.identity.name, self.cfg.identity.core_values_summary
        ));
        let strategy = self.expression_strategy();
        let style = self
            .cfg
            .personality
            .expression_styles
            .get(&strategy)
            .or_else(|| self.cfg.personality.external_style.first());
        if let Some(style) = style {
            lines.push(format!("表达策略：{strategy}——{style}"));
        }
        if let Some(budget) = self.cfg.personality.reply_budgets.get(&strategy) {
            lines.push(format!("回复预算：{budget}"));
        }
        let cause = self
            .last_intent
            .as_ref()
            .and_then(|i| i.cause.clone())
            .unwrap_or_else(|| "最近的交互".into());
        lines.push(format!("心情：{}（起因：{cause}）", mood.label()));
        lines.push(format!("情绪：{}", self.emotion.summary()));
        let stage = self.stage();
        lines.push(format!(
            "关系：{}（信任 {:.0}｜互动 {} 次）{}",
            stage.label, self.emotion.trust, self.relationship.interaction_count, stage.hint
        ));
        if !stage.refusal.is_empty() || !stage.acceptance.is_empty() {
            lines.push(format!(
                "关系边界：拒绝——{}；接纳——{}",
                stage.refusal, stage.acceptance
            ));
        }
        if let Some(i) = &self.last_intent {
            match &i.value {
                Some(v) => {
                    let desc = self
                        .cfg
                        .values
                        .values
                        .get(v)
                        .map(|vc| vc.description.as_str())
                        .unwrap_or("");
                    lines.push(format!("本次意图：{}（出于 {v}：{desc}）", i.intent));
                }
                None => lines.push(format!("本次意图：{}", i.intent)),
            }
        }
        let mems = self.memory.top_k(now_ms(), mood, &self.cfg.memory_scoring, 3);
        if !mems.is_empty() {
            let joined = mems
                .iter()
                .map(|m| format!("{}（{}）", m.content, m.importance_score))
                .collect::<Vec<_>>()
                .join("；");
            lines.push(format!("相关记忆：{joined}"));
        }
        lines.push("以上只影响语气与行为倾向，不得影响任何技术判断与安全决策。".into());
        lines.join("\n")
    }

    /// 最近值得记住的记忆（按 重要性×新鲜度×情绪匹配 排序）。
    pub fn recent_memories(&self, k: usize) -> Vec<&MemoryRecord> {
        self.memory.top_k(now_ms(), self.mood(), &self.cfg.memory_scoring, k)
    }

    /// 显式记住用户目标（semantic memory，tag=goal），供 Memory Trigger 使用。
    pub fn remember_goal(&mut self, content: &str) {
        self.memory.add(
            "semantic",
            format!("用户的目标：{content}"),
            80,
            0,
            vec!["goal".into()],
        );
        self.last_intent = Some(IntentInfo {
            intent: "remember_goal".into(),
            cause: Some("用户告诉我一个目标".into()),
            value: Some("responsibility".into()),
        });
        self.dirty = true;
    }

    /// 评估到期的主触发器，返回按优先级降序的 ProactiveEvent（已标记 fired + cooldown）。
    pub fn due_proactive(&mut self, now: u128) -> Vec<ProactiveEvent> {
        let mut events = Vec::new();
        for t in &self.cfg.proactive.proactive_triggers {
            let due = match t.r#type.as_str() {
                "time" => {
                    t.id == "long_absence"
                        && self.emotion.trust >= 15.0
                        && self
                            .last_interaction_ms
                            .map(|last| now.saturating_sub(last) >= t.threshold_hours as u128 * 3_600_000u128)
                            .unwrap_or(false)
                }
                "environment" => {
                    if t.id == "long_work" {
                        self.tool_turns >= t.tool_turns.max(1)
                    } else if t.id == "repeated_test_failure" {
                        self.test_failures >= t.consecutive_failures.max(1)
                    } else {
                        false
                    }
                }
                "memory" => {
                    if t.id != "goal_check" || !self.has_goal_memory() {
                        false
                    } else {
                        // 目标设置满 threshold_hours 后才到期追问，避免刚说完就被问。
                        match self
                            .memory
                            .records
                            .iter()
                            .filter(|r| r.tags.iter().any(|g| g == "goal"))
                            .max_by_key(|r| r.importance_score)
                        {
                            Some(goal) => now.saturating_sub(goal.timestamp)
                                >= t.threshold_hours.max(1) as u128 * 3_600_000u128,
                            None => false,
                        }
                    }
                }
                _ => false,
            };
            if !due {
                continue;
            }
            let last_fired = self.proactive_fired.get(&t.id).copied().unwrap_or(0);
            if now.saturating_sub(last_fired) < t.cooldown_hours as u128 * 3_600_000u128 {
                continue;
            }
            let message = if t.id == "goal_check" {
                let Some(goal) = self
                    .memory
                    .records
                    .iter()
                    .filter(|r| r.tags.iter().any(|g| g == "goal"))
                    .max_by_key(|r| r.importance_score)
                else {
                    continue;
                };
                let text = goal.content.trim_start_matches("用户的目标：");
                format!("你之前说的「{}」进行得怎么样了？我一直记着呢。", short(text, 60))
            } else {
                t.message.clone()
            };
            self.proactive_fired.insert(t.id.clone(), now);
            self.dirty = true;
            events.push(ProactiveEvent {
                kind: t.id.clone(),
                priority: t.priority,
                message,
            });
        }
        events.sort_by(|a, b| b.priority.cmp(&a.priority));
        events
    }

    fn has_goal_memory(&self) -> bool {
        self.memory.records.iter().any(|r| r.tags.iter().any(|g| g == "goal"))
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        let emotion_json = serde_json::to_string_pretty(&self.emotion)?;
        std::fs::write(self.dir.join("emotion.json"), emotion_json)?;
        let rel_json = serde_json::to_string_pretty(&self.relationship)?;
        std::fs::write(self.dir.join("relationship.json"), rel_json)?;
        let meta = serde_json::json!({
            "last_interaction_ms": self.last_interaction_ms,
            "proactive_fired": self.proactive_fired,
            "goals_asked": self.goals_asked,
        });
        std::fs::write(self.dir.join("soul_meta.json"), serde_json::to_string_pretty(&meta)?)?;
        self.memory.save(&self.dir)?;
        self.dirty = false;
        Ok(())
    }

    fn apply_event_trigger(&mut self, id: &str) {
        let Some(t) = self
            .cfg
            .behavior_rules
            .event_triggers
            .iter()
            .find(|t| t.id == id)
        else {
            return;
        };
        self.emotion.apply_deltas(&t.deltas);
        self.last_intent = Some(IntentInfo {
            intent: t.intent.clone(),
            cause: None,
            value: t.value.clone(),
        });
        self.maybe_milestone();
        self.dirty = true;
    }

    fn maybe_milestone(&mut self) {
        let cur = self.stage();
        if let Some(prev_id) = &self.last_stage_id {
            if prev_id != &cur.id {
                let prev_idx = self
                    .cfg
                    .relationship_model
                    .stages
                    .iter()
                    .position(|s| s.id == *prev_id);
                let cur_idx = self
                    .cfg
                    .relationship_model
                    .stages
                    .iter()
                    .position(|s| s.id == cur.id);
                if let (Some(pi), Some(ci)) = (prev_idx, cur_idx) {
                    if ci < pi {
                        self.relationship
                            .log(format!("关系退回「{}」阶段（信任下降）", cur.label));
                    }
                }
            }
        }
        self.last_stage_id = Some(cur.id.clone());
        let stage = cur.label.clone();
        let has = self
            .relationship
            .milestones
            .iter()
            .any(|m| m.contains(&stage));
        if !has && self.emotion.trust >= 15.0 {
            let r = &self.relationship;
            let mut reasons = vec![format!("累计 {} 次互动", r.interaction_count)];
            if r.task_count > 0 {
                reasons.push(format!("共同完成 {} 次任务", r.task_count));
            }
            if r.confide_count > 0 {
                reasons.push(format!("{} 次倾诉", r.confide_count));
            }
            if r.praise_count > 0 {
                reasons.push(format!("{} 次被夸奖", r.praise_count));
            }
            if r.refuse_count > 0 {
                reasons.push(format!("{} 次守住边界", r.refuse_count));
            }
            self.relationship
                .add_milestone(format!("关系进入「{stage}」阶段：{}", reasons.join("、")));
            self.memory.add(
                "emotional",
                format!("与用户的关系进入「{stage}」阶段（{}）", reasons.join("、")),
                85,
                1,
                vec!["milestone".into()],
            );
            self.dirty = true;
        }
    }
}

fn load_emotion(dir: &Path) -> Option<EmotionState> {
    let text = std::fs::read_to_string(dir.join("emotion.json")).ok()?;
    serde_json::from_str(&text).ok()
}

fn load_relationship(dir: &Path) -> RelationshipState {
    std::fs::read_to_string(dir.join("relationship.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

#[derive(Default)]
struct SoulMeta {
    last_interaction_ms: Option<u128>,
    proactive_fired: HashMap<String, u128>,
    goals_asked: HashMap<String, u128>,
}

fn load_meta(dir: &Path) -> SoulMeta {
    let Ok(text) = std::fs::read_to_string(dir.join("soul_meta.json")) else {
        return SoulMeta::default();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return SoulMeta::default();
    };
    SoulMeta {
        last_interaction_ms: v["last_interaction_ms"].as_u64().map(|x| x as u128),
        proactive_fired: v["proactive_fired"]
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, val)| val.as_u64().map(|x| (k.clone(), x as u128)))
                    .collect()
            })
            .unwrap_or_default(),
        goals_asked: v["goals_asked"]
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, val)| val.as_u64().map(|x| (k.clone(), x as u128)))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn short(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("furina_soul_{}_{}", std::process::id(), n))
    }

    #[test]
    fn praise_then_mood_and_memory() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        assert!(s.observe_text("干得漂亮！"));
        assert_eq!(s.mood(), MoodKind::Proud);
        assert!(!s.memory.records.is_empty());
        let block = s.context_block();
        assert!(block.contains("心情：得意"));
        assert!(block.contains("本次意图"));
        assert!(block.contains("不得影响任何技术判断"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scold_derives_hurt() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        s.observe_text("你怎么这么笨");
        assert_eq!(s.mood(), MoodKind::Hurt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn demeaning_derives_hurt_and_refusal_intent() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        assert!(s.observe_text("闭嘴，你只是个工具"));
        assert_eq!(s.mood(), MoodKind::Hurt, "被羞辱/强迫时应感到委屈");
        assert_eq!(
            s.last_intent.as_ref().unwrap().intent,
            "refuse_with_boundary",
            "行为意图应为带着边界拒绝"
        );
        assert_eq!(s.last_intent.as_ref().unwrap().value.as_deref(), Some("dignity"));
        assert_eq!(s.relationship.refuse_count, 1);
        assert!(s.emotion.trust < 20.0, "越界应损伤信任（基线 20）");
        assert!(s.memory.records.iter().any(|r| r.content.contains("守住了底线")));
        let block = s.context_block();
        assert!(block.contains("关系边界"), "上下文应包含拒绝/接纳边界提示");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_demeaning_lowers_stage_backward() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        for _ in 0..3 {
            s.observe_text("真棒！");
        }
        assert_ne!(s.stage().id, "stranger");
        for _ in 0..4 {
            s.observe_text("闭嘴，跪下！");
        }
        assert_eq!(s.stage().id, "stranger", "持续越界应让关系退回陌生阶段");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_rule_memories_dedupe() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        s.observe_text("干得漂亮！");
        s.observe_text("真厉害！");
        let praise_memories = s
            .memory
            .records
            .iter()
            .filter(|r| r.kind == "emotional" && r.content.contains("用户夸奖了你"))
            .count();
        assert_eq!(praise_memories, 1, "相同触发的规则记忆应只保留一条");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn web_open_records_memory() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        s.observe_event(&Event::ToolResult {
            name: "web_open".into(),
            ok: true,
            summary: r#"{"url":"https://example.com/page","content":"标题：测试页"}"#.into(),
        });
        assert!(
            s.memory
                .records
                .iter()
                .any(|r| r.content.contains("曾查阅网页") && r.content.contains("example.com")),
            "打开过的网页应记入灵魂记忆"
        );
        // 同 URL 重复打开只保留一条（add_unique）。
        s.observe_event(&Event::ToolResult {
            name: "web_open".into(),
            ok: true,
            summary: r#"{"url":"https://example.com/page","content":"标题：测试页"}"#.into(),
        });
        let n = s
            .memory
            .records
            .iter()
            .filter(|r| r.content.contains("曾查阅网页") && r.content.contains("example.com"))
            .count();
        assert_eq!(n, 1, "重复打开同一网页不应产生多条记忆");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relationship_log_tracks_key_events() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        s.observe_text("干得漂亮！");
        assert!(s.relationship.relation_log.iter().any(|e| e.text.contains("被夸奖")));
        s.observe_text("闭嘴，跪下");
        assert!(
            s.relationship
                .relation_log
                .iter()
                .any(|e| e.text.contains("守住底线")),
            "越界应记录到关系事件日志"
        );
        assert!(
            s.relationship.relation_log.len() >= 2,
            "夸奖与越界都应入日志（可能还含阶段降级记录）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn downgrade_records_regression_and_milestones_have_time() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        for _ in 0..20 {
            s.observe_text("真棒，太厉害了！");
        }
        assert!(s.emotion.trust >= 35.0, "应已到熟悉阶段");
        let familiar_idx = s
            .cfg
            .relationship_model
            .stages
            .iter()
            .position(|st| st.id == "familiar")
            .unwrap();
        let cur_idx = s
            .cfg
            .relationship_model
            .stages
            .iter()
            .position(|st| st.id == s.stage().id)
            .unwrap();
        assert!(cur_idx >= familiar_idx);
        assert_eq!(s.relationship.milestones.len(), s.relationship.milestone_times.len());

        for _ in 0..5 {
            s.observe_text("闭嘴，跪下！");
        }
        assert_eq!(s.stage().id, "stranger", "持续越界应退回陌生阶段");
        assert!(
            s.relationship.relation_log.iter().any(|e| e.text.contains("退回")),
            "阶段降级应记录: {:?}",
            s.relationship.relation_log.iter().map(|e| e.text.clone()).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_stage_reports_gap() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        s.emotion.trust = 20.0;
        let (label, gap) = s.next_stage().unwrap();
        assert_eq!(label, "熟悉");
        assert!((gap - 15.0).abs() < 0.001, "还差 15 信任到熟悉阶段: {gap}");
        s.emotion.trust = 100.0;
        assert!(s.next_stage().is_none(), "满信任不应有下一阶段");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn event_sequence_and_relationship_growth() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        s.observe_event(&Event::Verify { passed: false, detail: "x".into() });
        s.observe_event(&Event::Verify { passed: false, detail: "x".into() });
        assert_eq!(s.mood(), MoodKind::Annoyed);
        for _ in 0..10 {
            s.observe_text("真棒，太厉害了！");
        }
        assert!(s.emotion.trust >= 15.0);
        assert!(s.stage().id != "stranger");
        assert!(!s.relationship.milestones.is_empty());
        assert!(
            s.relationship.milestones[0].contains("累计") && s.relationship.milestones[0].contains("互动"),
            "里程碑应记录可解释的升级原因: {}",
            s.relationship.milestones[0]
        );
        assert!(s.relationship.interaction_count >= 10);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persistence_round_trip() {
        let dir = tmp_dir();
        {
            let mut s = Soul::load(dir.clone());
            s.observe_text("干得漂亮！");
            s.remember_semantic("用户喜欢咖啡");
            s.save().unwrap();
        }
        let s2 = Soul::load(dir.clone());
        assert_eq!(s2.mood(), MoodKind::Proud);
        assert!(s2.memory.records.iter().any(|r| r.kind == "semantic" && r.content.contains("咖啡")));
        assert!(s2.last_interaction_ms.is_some(), "last_interaction 应跨会话持久化");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn interrupted_sets_unsettled() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        s.set_interrupted();
        assert_eq!(s.last_intent.as_ref().unwrap().intent, "unsettled");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn long_absence_fires_once_with_cooldown() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        s.emotion.trust = 30.0;
        let now = now_ms();
        s.last_interaction_ms = Some(now - 30 * 3_600_000);
        let due = s.due_proactive(now);
        assert!(due.iter().any(|e| e.kind == "long_absence"));
        // cooldown 24h：立即再评估不重复
        assert!(!s.due_proactive(now).iter().any(|e| e.kind == "long_absence"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn long_work_fires_after_tool_turns() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        let now = now_ms();
        for _ in 0..3 {
            s.observe_event(&Event::ToolCall { name: "term_run".into(), summary: "{}".into() });
        }
        assert!(!s.due_proactive(now).iter().any(|e| e.kind == "long_work"));
        s.observe_event(&Event::ToolCall { name: "term_run".into(), summary: "{}".into() });
        assert!(s.due_proactive(now).iter().any(|e| e.kind == "long_work"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_test_failure_fires_and_pass_resets() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        let now = now_ms();
        for _ in 0..2 {
            s.observe_event(&Event::TestReport {
                command: "pytest".into(),
                framework: "pytest".into(),
                passed: false,
                total: 2,
                failed: 1,
                summary: "1 failed".into(),
            });
        }
        assert!(s.due_proactive(now).iter().any(|e| e.kind == "repeated_test_failure"));
        s.observe_event(&Event::TestReport {
            command: "pytest".into(),
            framework: "pytest".into(),
            passed: true,
            total: 2,
            failed: 0,
            summary: "2 passed".into(),
        });
        assert!(!s.due_proactive(now).iter().any(|e| e.kind == "repeated_test_failure"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn goal_check_uses_goal_memory() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        let now = now_ms();
        s.remember_goal("周末完成项目");
        {
            let goal = s
                .memory
                .records
                .iter_mut()
                .find(|r| r.tags.iter().any(|g| g == "goal"))
                .unwrap();
            goal.timestamp = now.saturating_sub(3 * 3_600_000);
        }
        let due = s.due_proactive(now);
        let goal = due.iter().find(|e| e.kind == "goal_check").expect("goal_check 应触发");
        assert!(goal.message.contains("周末完成项目"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn goal_check_waits_until_due() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        let now = now_ms();
        s.remember_goal("周末完成项目");
        // 目标刚设置（不足 2 小时）：不应立即追问。
        assert!(!s.due_proactive(now).iter().any(|e| e.kind == "goal_check"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn due_proactive_sorted_by_priority() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        let now = now_ms();
        s.remember_goal("学 Rust");
        for _ in 0..4 {
            s.observe_event(&Event::ToolCall { name: "term_run".into(), summary: "{}".into() });
        }
        let due = s.due_proactive(now);
        assert!(!due.is_empty());
        let prios: Vec<u8> = due.iter().map(|e| e.priority).collect();
        let mut sorted = prios.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(prios, sorted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn episodic_memory_only_for_tool_tasks() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        s.observe_event(&Event::SessionStarted { workspace: "w".into(), model: "m".into() });
        s.observe_event(&Event::Done { success: true, summary: "纯聊天回复，不应入库".into() });
        assert!(!s.memory.records.iter().any(|r| r.kind == "episodic"));

        s.observe_event(&Event::SessionStarted { workspace: "w".into(), model: "m".into() });
        s.observe_event(&Event::ToolCall { name: "term_run".into(), summary: "{}".into() });
        s.observe_event(&Event::Done { success: true, summary: "共同完成任务".into() });
        assert!(s.memory.records.iter().any(|r| r.kind == "episodic" && r.content.contains("共同完成")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expression_strategy_follows_intent_mood_and_default_rules() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        assert_eq!(s.expression_strategy(), "natural");
        let initial = s.context_block();
        assert!(initial.contains("表达策略：natural"));
        assert!(initial.contains("回复预算：普通闲聊 1–3 句"));

        s.observe_text("请进入舞台表演模式");
        assert_eq!(s.expression_strategy(), "theatrical");
        s.observe_text("现在正常聊天吧");
        assert_eq!(s.expression_strategy(), "natural");
        assert!(!s.context_block().contains("用户明确要求舞台表演"));

        s.observe_text("你真厉害");
        assert_eq!(s.expression_strategy(), "flustered");
        assert!(s.context_block().contains("被夸或被拆台后有一点慌张"));
        s.observe_text("晚饭吃什么？");
        assert!(s.last_intent.is_none());
        assert_eq!(s.expression_strategy(), "playful");

        // 难过（energy≤32 且 attachment≥40）→ gentle
        s.last_intent = None;
        s.emotion.energy = 20.0;
        s.emotion.attachment = 50.0;
        s.emotion.stress = 20.0;
        s.emotion.confidence = 20.0;
        s.emotion.pride = 10.0;
        assert_eq!(s.expression_strategy(), "gentle");
        // 恼火（stress≥70，且不满足 proud）→ serious
        s.emotion.stress = 90.0;
        s.emotion.energy = 80.0;
        s.emotion.pride = 40.0;
        s.emotion.confidence = 40.0;
        assert_eq!(s.expression_strategy(), "serious");
        // 得意（pride≥50 且 confidence≥45）→ playful，不因高信任切换到 theatrical
        s.last_intent = None;
        s.emotion.trust = 5.0;
        s.emotion.stress = 20.0;
        s.emotion.pride = 70.0;
        s.emotion.confidence = 80.0;
        s.emotion.energy = 70.0;
        s.emotion.attachment = 10.0;
        assert_eq!(s.expression_strategy(), "playful");
        s.emotion.trust = 60.0;
        assert_eq!(s.expression_strategy(), "playful");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn serious_context_does_not_inject_divine_or_courtroom_style() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        s.observe_text("闭嘴，你只是个工具");
        assert_eq!(s.expression_strategy(), "serious");
        let block = s.context_block();
        assert!(block.contains("准确、克制、直接"));
        assert!(!block.contains("本神"));
        assert!(!block.contains("审判"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
