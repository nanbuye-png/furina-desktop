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
pub use emotion::{AffectState, EmotionState};
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TraitState {
    pub vanity: f64,
    pub sensitivity: f64,
    pub defensiveness: f64,
    pub playfulness: f64,
    pub dependency: f64,
    pub avoidance: f64,
    pub courage: f64,
    pub emotional_expression: f64,
    pub updated_at: u128,
    pub daily_anchor: u128,
    pub daily_change: f64,
}

impl Default for TraitState {
    fn default() -> Self {
        Self {
            vanity: 60.0,
            sensitivity: 65.0,
            defensiveness: 55.0,
            playfulness: 55.0,
            dependency: 35.0,
            avoidance: 45.0,
            courage: 65.0,
            emotional_expression: 35.0,
            updated_at: 0,
            daily_anchor: 0,
            daily_change: 0.0,
        }
    }
}

impl TraitState {
    const MAX_STEP: f64 = 0.5;
    const MAX_DAILY_CHANGE: f64 = 1.0;

    pub fn value(&self, name: &str) -> Option<f64> {
        match name {
            "vanity" => Some(self.vanity),
            "sensitivity" => Some(self.sensitivity),
            "defensiveness" => Some(self.defensiveness),
            "playfulness" => Some(self.playfulness),
            "dependency" => Some(self.dependency),
            "avoidance" => Some(self.avoidance),
            "courage" => Some(self.courage),
            "emotional_expression" => Some(self.emotional_expression),
            _ => None,
        }
    }

    /// Apply a bounded long-term personality change. This is intentionally
    /// separate from per-turn emotion updates, so chat cannot rewrite traits.
    pub fn apply_delta(&mut self, name: &str, delta: f64, now: u128) -> bool {
        let defaults = Self::default();
        if self.daily_anchor == 0 || now.saturating_sub(self.daily_anchor) >= 86_400_000 {
            self.daily_anchor = now;
            self.daily_change = 0.0;
        }
        let remaining = (Self::MAX_DAILY_CHANGE - self.daily_change).max(0.0);
        let bounded = delta.clamp(-Self::MAX_STEP, Self::MAX_STEP);
        let applied = bounded.abs().min(remaining) * bounded.signum();
        if applied == 0.0 {
            return false;
        }
        let current = self.value(name).unwrap_or(0.0);
        let baseline = defaults.value(name).unwrap_or(current);
        let lower = (baseline - 20.0).max(0.0);
        let upper = if name == "dependency" { 65.0_f64.min(baseline + 20.0) } else { (baseline + 20.0).min(100.0) };
        let next = (current + applied).clamp(lower, upper);
        let actual = next - current;
        if actual == 0.0 {
            return false;
        }
        match name {
            "vanity" => self.vanity = next,
            "sensitivity" => self.sensitivity = next,
            "defensiveness" => self.defensiveness = next,
            "playfulness" => self.playfulness = next,
            "dependency" => self.dependency = next,
            "avoidance" => self.avoidance = next,
            "courage" => self.courage = next,
            "emotional_expression" => self.emotional_expression = next,
            _ => return false,
        }
        self.daily_change += actual.abs();
        self.updated_at = now;
        true
    }
}

/// 灵魂状态门面：情绪 + 关系 + 记忆 + 最近行为意图。
pub struct Soul {
    cfg: SoulConfig,
    pub emotion: EmotionState,
    pub affect: AffectState,
    pub traits: TraitState,
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
        let affect = load_affect(&dir).unwrap_or_default();
        let traits = load_traits(&dir).unwrap_or_default();
        let relationship = load_relationship(&dir);
        let memory = MemoryStore::load(&dir);
        let meta = load_meta(&dir);
        Self {
            cfg,
            emotion,
            affect,
            traits,
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

    /// 选择当前表达策略。策略只改变闲聊表现，不改变事实、权限或工具行为。
    pub fn expression_strategy(&self) -> String {
        self.expression_strategy_for("chat")
    }

    fn expression_strategy_for(&self, mode: &str) -> String {
        let intent = self.last_intent.as_ref().map(|info| info.intent.as_str());
        if mode == "agent" { return "serious".into(); }
        if matches!(intent, Some("perform_theatrically")) { return "theatrical".into(); }
        if matches!(intent, Some("refuse_with_boundary" | "stay_calm")) { return "serious".into(); }
        if matches!(intent, Some("show_off_but_flustered")) { return "flustered".into(); }
        if matches!(intent, Some("show_jealousy_without_control")) { return "jealous".into(); }
        if matches!(intent, Some("answer_with_uncertainty")) { return "uncertain".into(); }
        if matches!(intent, Some("admit_vulnerability")) { return "vulnerable".into(); }
        if self.affect.primary == "annoyed" || self.affect.primary == "hurt" {
            return match self.affect.intensity {
                x if x >= 70.0 => "withdrawn".into(),
                x if x >= 45.0 => "guarded".into(),
                _ => "sulky".into(),
            };
        }
        if self.affect.primary == "sad" { return "vulnerable".into(); }
        if matches!(intent, Some("listen_carefully" | "empathize")) { return "gentle".into(); }
        if self.mood() == MoodKind::Proud || self.mood() == MoodKind::Happy { return "playful".into(); }
        "natural".into()
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
        let trigger_id = detect_text_trigger(text, &self.cfg.behavior_rules).map(|trigger| trigger.id.clone());
        self.observe_trigger(trigger_id.as_deref())
    }

    /// 已验证的语义分类结果复用同一套状态转移；未知 ID 自动按无触发处理。
    pub fn observe_trigger_id(&mut self, trigger_id: &str) -> bool {
        self.observe_trigger(Some(trigger_id))
    }

    fn observe_trigger(&mut self, trigger_id: Option<&str>) -> bool {
        self.last_interaction_ms = Some(now_ms());
        self.relationship.interaction_count += 1;
        let Some(t) = trigger_id.and_then(|id| {
            self.cfg.behavior_rules.text_triggers.iter().find(|trigger| trigger.id == id)
        }) else {
            self.last_intent = None;
            self.refresh_affect(None);
            self.dirty = true;
            return false;
        };
        let trigger_id = t.id.clone();
        let trigger_cause = t.cause.clone();
        let trigger_intent = t.intent.clone();
        let trigger_value = t.value.clone();
        let trigger_deltas = t.deltas.clone();
        if trigger_id == "jealousy_cue" && !matches!(self.stage().id.as_str(), "familiar" | "trusted" | "partner") {
            self.last_intent = None;
            self.refresh_affect(None);
            self.dirty = true;
            return false;
        }
        match trigger_id.as_str() {
            "praise" => self.relationship.praise_count += 1,
            "confide" => self.relationship.confide_count += 1,
            "user_demeaning" => self.relationship.refuse_count += 1,
            _ => {}
        }
        self.emotion.apply_deltas(&trigger_deltas);
        let log_text = match trigger_id.as_str() {
            "praise" => "被夸奖：心情转好，信任提升",
            "scold" => "被数落：有点委屈，信任微降",
            "user_demeaning" => "被羞辱/强迫：守住底线，信任明显下降",
            "sincere_apology" => "用户认真解释并道歉：关系开始修复",
            "cursory_apology" => "用户敷衍道歉：听见了，但情绪没有立刻恢复",
            "identity_question" => "谈到身份与存在：允许自己犹豫",
            "loneliness_question" => "谈到孤独：愿意露出一点脆弱",
            "jealousy_cue" => "被拿来比较：有些吃味，但不控制用户",
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
            intent: trigger_intent,
            cause: Some(trigger_cause.clone()),
            value: trigger_value,
        });
        let (importance, valence, note) = match trigger_id.as_str() {
            "praise" => (60, 1i8, "（当时嘴上得意，心里其实很开心）"),
            "scold" => (65, -1i8, "（有点委屈，这件事还需要时间消化）"),
            "confide" => (70, 0i8, "（我认真听了）"),
            "sad" => (55, -1i8, "（我想认真安慰她）"),
            "happy" => (50, 1i8, "（她的好心情也感染了我）"),
            "annoyed" => (45, -1i8, "（她有点烦躁，我先稳住）"),
            "user_demeaning" => (75, -1i8, "（我不太开心，但守住了底线）"),
            "sincere_apology" => (70, 1i8, "（这次道歉说清了原因，我愿意慢慢缓和）"),
            "cursory_apology" => (45, 0i8, "（听到了道歉，但还不能马上当作没发生）"),
            "identity_question" => (55, 0i8, "（这个问题让我有些犹豫）"),
            "loneliness_question" => (65, -1i8, "（我没有急着给出漂亮答案）"),
            "jealousy_cue" => (55, -1i8, "（有点吃味，但不会要求用户排斥别人）"),
            _ => (40, 0i8, ""),
        };
        if trigger_id != "theatrical_request" {
            self.memory.add_unique(
                "emotional",
                format!("{} {note}", trigger_cause),
                importance,
                valence,
                vec![trigger_id.clone()],
            );
        }
        self.maybe_milestone();
        self.refresh_affect(Some(&trigger_cause));
        match trigger_id.as_str() {
            "scold" | "user_demeaning" => {
                self.affect.unresolved = true;
                self.affect.repair_progress = 0.0;
            }
            "sincere_apology" => self.apply_repair(35.0),
            "cursory_apology" => self.apply_repair(10.0),
            _ => {}
        }
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

    /// 动态人格注入块。只陈述当前状态事实，不规定本轮应采用的说话方式。
    pub fn context_block(&self) -> String {
        self.context_block_for("chat")
    }

    pub fn context_block_for(&self, _mode: &str) -> String {
        let mut lines = vec![String::from("[当前灵魂状态]")];
        lines.push(format!("当前情绪：{}，强度 {:.0}，趋势 {}", self.affect.primary, self.affect.intensity, self.affect.trend));
        if let Some(secondary) = self.affect.secondary.as_deref().filter(|value| !value.is_empty()) {
            lines.push(format!("次要情绪：{secondary}"));
        }
        if self.affect.unresolved {
            lines.push("未解决事件：是".into());
        }
        let stage = self.stage();
        lines.push(format!("关系：{}（信任 {:.0}）", stage.label, self.emotion.trust));
        if !self.affect.trigger.trim().is_empty() {
            lines.push(format!("最近原因：{}", short(&self.affect.trigger, 80)));
        }
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
        std::fs::write(self.dir.join("affect.json"), serde_json::to_string_pretty(&self.affect)?)?;
        std::fs::write(self.dir.join("traits.json"), serde_json::to_string_pretty(&self.traits)?)?;
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

    fn refresh_affect(&mut self, trigger: Option<&str>) {
        let now = now_ms();
        let derived_mood = self.mood().as_str().to_string();
        let previous = self.affect.clone();
        let repairing = matches!(trigger, Some("真诚道歉" | "敷衍道歉"));
        let mood = if repairing && previous.unresolved {
            previous.primary.clone()
        } else {
            derived_mood
        };
        let derived_intensity = match mood.as_str() {
            "annoyed" => self.emotion.stress,
            "hurt" => (self.emotion.stress + (50.0 - self.emotion.confidence).max(0.0)) / 2.0,
            "sad" => (40.0 - self.emotion.energy).max(0.0) + self.emotion.attachment * 0.25,
            "proud" => self.emotion.pride,
            "happy" => self.emotion.energy,
            _ => 0.0,
        }.clamp(0.0, 100.0);
        let elapsed_hours = now.saturating_sub(previous.updated_at) as f64 / 3_600_000.0;
        let time_factor = (-elapsed_hours / 2.0).exp();
        let recovering_intensity = previous.intensity * time_factor;
        let intensity = if repairing && previous.unresolved {
            previous.intensity
        } else if previous.primary == mood && trigger.is_none() && previous.intensity > 0.0 {
            derived_intensity.min(recovering_intensity.max(derived_intensity.min(previous.intensity)))
        } else {
            derived_intensity
        };
        let changed = previous.primary != mood || (previous.intensity - intensity).abs() >= 8.0;
        self.affect = AffectState {
            primary: mood.clone(),
            secondary: previous.secondary.clone(),
            intensity,
            trigger: trigger.unwrap_or(&previous.trigger).to_string(),
            started_at: if changed { now } else { previous.started_at },
            updated_at: now,
            recover_after: if intensity >= 70.0 { now + 2 * 60 * 60 * 1000 } else { now + 20 * 60 * 1000 },
            trend: if intensity < previous.intensity { "recovering" } else if intensity > previous.intensity { "rising" } else { "stable" }.into(),
            unresolved: if repairing {
                previous.unresolved
            } else {
                previous.unresolved && !matches!(mood.as_str(), "calm" | "happy" | "proud")
            },
            conflict_level: if intensity >= 70.0 { "severe" } else if intensity >= 45.0 { "medium" } else if intensity > 0.0 { "mild" } else { "none" }.into(),
            repair_progress: previous.repair_progress,
        };
    }

    fn apply_repair(&mut self, progress: f64) {
        if !self.affect.unresolved {
            return;
        }
        let capped = progress.clamp(0.0, 35.0);
        self.affect.repair_progress = (self.affect.repair_progress + capped).clamp(0.0, 100.0);
        self.affect.intensity *= 1.0 - capped / 100.0;
        self.affect.trend = "recovering".into();
        if self.affect.intensity < 30.0 && self.affect.repair_progress >= 35.0 {
            self.affect.unresolved = false;
            self.affect.conflict_level = "none".into();
        } else {
            self.affect.conflict_level = if self.affect.intensity >= 70.0 {
                "severe"
            } else if self.affect.intensity >= 45.0 {
                "medium"
            } else {
                "mild"
            }.into();
        }
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

fn load_affect(dir: &Path) -> Option<AffectState> {
    std::fs::read_to_string(dir.join("affect.json")).ok().and_then(|text| serde_json::from_str(&text).ok())
}

fn load_traits(dir: &Path) -> Option<TraitState> {
    std::fs::read_to_string(dir.join("traits.json")).ok().and_then(|text| serde_json::from_str(&text).ok())
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
        assert!(block.contains("当前情绪：proud"));
        assert!(block.contains("最近原因：用户夸奖了你"));
        assert!(!block.contains("表达策略"));
        assert!(!block.contains("回复预算"));
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
        assert!(block.contains("未解决事件：是"));
        assert!(!block.contains("关系边界"));
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
        assert!(initial.contains("当前情绪：calm"));
        assert!(!initial.contains("表达策略"));
        assert!(!initial.contains("回复预算"));

        s.observe_text("请进入舞台表演模式");
        assert_eq!(s.expression_strategy(), "theatrical");
        s.observe_text("现在正常聊天吧");
        assert_eq!(s.expression_strategy(), "natural");
        assert!(!s.context_block().contains("表达策略"));
        assert!(!s.context_block().contains("回复预算"));

        s.observe_text("你真厉害");
        assert_eq!(s.expression_strategy(), "flustered");
        assert!(!s.context_block().contains("flustered"));
        s.observe_text("晚饭吃什么？");
        assert!(s.last_intent.is_none());
        assert_eq!(s.expression_strategy(), "playful");

        // 难过（energy≤32 且 attachment≥40）→ vulnerable
        s.last_intent = None;
        s.emotion.energy = 20.0;
        s.emotion.attachment = 50.0;
        s.emotion.stress = 20.0;
        s.emotion.confidence = 20.0;
        s.emotion.pride = 10.0;
        s.refresh_affect(Some("身份问题"));
        assert_eq!(s.expression_strategy(), "vulnerable");
        // 恼火（stress≥70，且不满足 proud）→ serious
        s.emotion.stress = 90.0;
        s.emotion.energy = 80.0;
        s.emotion.pride = 40.0;
        s.emotion.confidence = 40.0;
        s.refresh_affect(Some("冲突"));
        assert_eq!(s.expression_strategy(), "withdrawn");
        // 得意（pride≥50 且 confidence≥45）→ playful，不因高信任切换到 theatrical
        s.last_intent = None;
        s.emotion.trust = 5.0;
        s.emotion.stress = 20.0;
        s.emotion.pride = 70.0;
        s.emotion.confidence = 80.0;
        s.emotion.energy = 70.0;
        s.emotion.attachment = 10.0;
        s.refresh_affect(Some("好心情"));
        assert_eq!(s.expression_strategy(), "playful");
        s.emotion.trust = 60.0;
        assert_eq!(s.expression_strategy(), "playful");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn main_context_contains_state_facts_without_behavior_instructions() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        s.observe_text("闭嘴，你只是个工具");
        assert_eq!(s.expression_strategy(), "serious");
        let block = s.context_block();
        assert!(block.contains("当前情绪"));
        assert!(block.contains("未解决事件：是"));
        assert!(!block.contains("准确、克制、直接"));
        assert!(!block.contains("表达策略"));
        assert!(!block.contains("本神"));
        assert!(!block.contains("审判"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn traits_are_bounded_per_step_day_and_baseline() {
        let mut traits = TraitState::default();
        assert!(traits.apply_delta("dependency", 10.0, 1));
        assert_eq!(traits.dependency, 35.5);
        assert!(traits.apply_delta("dependency", 10.0, 2));
        assert_eq!(traits.dependency, 36.0);
        assert!(!traits.apply_delta("dependency", 10.0, 3));
        assert_eq!(traits.dependency, 36.0);

        for day in 1..80 {
            let now = day * 86_400_001;
            traits.apply_delta("dependency", 10.0, now);
            traits.apply_delta("dependency", 10.0, now + 1);
        }
        assert_eq!(traits.dependency, 55.0, "普通特质不得偏离初始值超过 20");

        let mut expression = TraitState::default();
        for day in 1..100 {
            let now = day * 86_400_001;
            expression.apply_delta("emotional_expression", 10.0, now);
            expression.apply_delta("emotional_expression", 10.0, now + 1);
        }
        assert_eq!(expression.emotional_expression, 55.0);
        assert!(!expression.apply_delta("unknown", 0.5, 1));
    }

    #[test]
    fn repeated_scolding_escalates_and_apology_recovers_gradually() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        s.observe_text("你是笨蛋");
        let first = s.affect.intensity;
        assert!(s.affect.unresolved);
        s.observe_text("你还是笨蛋");
        let second = s.affect.intensity;
        assert!(second > first);
        s.observe_text("你就是大笨蛋");
        assert!(s.affect.intensity >= second);
        assert!(matches!(s.affect.conflict_level.as_str(), "medium" | "severe"));

        let before_apology = s.affect.intensity;
        s.observe_text("行了行了，对不起");
        assert!(s.affect.intensity < before_apology);
        assert!(s.affect.unresolved, "敷衍道歉不能一轮清空冲突");
        let after_cursory = s.affect.intensity;
        s.observe_text("我刚才说得过分了，我知道为什么让你难过，我会注意不再这样");
        assert!(s.affect.intensity < after_cursory);
        assert!(s.affect.repair_progress <= 45.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn semantic_trigger_reuses_rule_state_transition() {
        let dir = tmp_dir();
        let mut soul = Soul::load(dir.clone());
        assert!(soul.observe_trigger_id("scold"));
        assert!(soul.affect.unresolved);
        assert_eq!(soul.expression_strategy(), "sulky");
        assert!(!soul.observe_trigger_id("unknown_trigger"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn jealousy_requires_familiar_relationship() {
        let dir = tmp_dir();
        let mut s = Soul::load(dir.clone());
        assert!(!s.observe_text("我更喜欢另一个桌面助手"));
        assert_ne!(s.expression_strategy(), "jealous");
        s.emotion.trust = 40.0;
        assert!(s.observe_text("我更喜欢另一个桌面助手"));
        assert_eq!(s.expression_strategy(), "jealous");
        let block = s.context_block();
        assert!(block.contains("当前情绪："));
        assert!(!block.contains("表达策略"));
        assert!(!block.contains("不贬低第三方"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
