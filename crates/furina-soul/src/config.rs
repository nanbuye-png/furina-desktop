//! Soul 配置：编译期内嵌 `persona/soul/*.yaml`（git 版本管理、运行零 IO）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 派生心情（v1 兼容的离散心情，由六维情绪推导）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoodKind {
    #[default]
    Calm,
    Happy,
    Proud,
    Hurt,
    Sad,
    Annoyed,
}

impl MoodKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MoodKind::Calm => "calm",
            MoodKind::Happy => "happy",
            MoodKind::Proud => "proud",
            MoodKind::Hurt => "hurt",
            MoodKind::Sad => "sad",
            MoodKind::Annoyed => "annoyed",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MoodKind::Calm => "淡定",
            MoodKind::Happy => "开心",
            MoodKind::Proud => "得意",
            MoodKind::Hurt => "委屈",
            MoodKind::Sad => "难过",
            MoodKind::Annoyed => "恼火",
        }
    }

    pub fn positive(self) -> bool {
        matches!(self, MoodKind::Happy | MoodKind::Proud)
    }

    pub fn negative(self) -> bool {
        matches!(self, MoodKind::Hurt | MoodKind::Sad | MoodKind::Annoyed)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SoulConfig {
    pub identity: IdentityConfig,
    pub personality: PersonalityConfig,
    pub values: ValuesConfig,
    pub emotion_model: EmotionModelConfig,
    pub behavior_rules: BehaviorRulesConfig,
    pub relationship_model: RelationshipModelConfig,
    pub memory_scoring: MemoryScoringConfig,
    pub proactive: ProactiveConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct IdentityConfig {
    pub name: String,
    pub self_definition: Vec<String>,
    pub core_values_summary: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct PersonalityConfig {
    pub external_style: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ValuesConfig {
    pub values: HashMap<String, ValueConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ValueConfig {
    pub priority: u8,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct EmotionModelConfig {
    pub dimensions: HashMap<String, DimConfig>,
    pub mood_derivation: Vec<MoodRule>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct DimConfig {
    pub baseline: f64,
    pub decay_hours: f64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct MoodRule {
    pub mood: MoodKind,
    pub min_pride: Option<f64>,
    pub min_confidence: Option<f64>,
    pub max_stress: Option<f64>,
    pub min_energy: Option<f64>,
    pub min_attachment: Option<f64>,
    pub min_stress: Option<f64>,
    pub max_confidence: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct BehaviorRulesConfig {
    pub text_triggers: Vec<TextTriggerConfig>,
    pub event_triggers: Vec<EventTriggerConfig>,
    /// 表达策略选择规则（theatrical / casual / gentle / serious 等）：
    /// 顺序即优先级，第一条同时满足心情与信任范围者生效；is_default 兜底。
    #[serde(default)]
    pub expression_strategies: Vec<ExpressionStrategyConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ExpressionStrategyConfig {
    pub id: String,
    /// 适用心情列表；空 = 不限心情。
    #[serde(default)]
    pub moods: Vec<String>,
    #[serde(default)]
    pub min_trust: Option<f64>,
    #[serde(default)]
    pub max_trust: Option<f64>,
    /// 兜底策略（最后一个匹配不中时使用）。
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct TextTriggerConfig {
    pub id: String,
    pub priority: u8,
    pub cause: String,
    pub intent: String,
    pub value: Option<String>,
    pub keywords: Vec<String>,
    pub deltas: HashMap<String, f64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct EventTriggerConfig {
    pub id: String,
    pub intent: String,
    pub value: Option<String>,
    pub deltas: HashMap<String, f64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct RelationshipModelConfig {
    pub stages: Vec<StageConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct StageConfig {
    pub id: String,
    pub label: String,
    pub trust_min: f64,
    pub hint: String,
    #[serde(default)]
    pub refusal: String,
    #[serde(default)]
    pub acceptance: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct MemoryScoringConfig {
    pub factors: HashMap<String, f64>,
    pub levels: HashMap<String, u8>,
    pub recency_half_life_hours: f64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ProactiveConfig {
    pub proactive_triggers: Vec<ProactiveTriggerConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ProactiveTriggerConfig {
    pub id: String,
    pub r#type: String,
    pub threshold_hours: u64,
    pub cooldown_hours: u64,
    pub priority: u8,
    pub tool_turns: u32,
    pub consecutive_failures: u32,
    pub message: String,
}

/// 加载全部内嵌配置。
pub fn load() -> SoulConfig {
    SoulConfig {
        identity: serde_yaml::from_str(include_str!("../../../persona/soul/identity.yaml")).unwrap_or_default(),
        personality: serde_yaml::from_str(include_str!("../../../persona/soul/personality.yaml")).unwrap_or_default(),
        values: serde_yaml::from_str(include_str!("../../../persona/soul/values.yaml")).unwrap_or_default(),
        emotion_model: serde_yaml::from_str(include_str!("../../../persona/soul/emotion_model.yaml")).unwrap_or_default(),
        behavior_rules: serde_yaml::from_str(include_str!("../../../persona/soul/behavior_rules.yaml")).unwrap_or_default(),
        relationship_model: serde_yaml::from_str(include_str!("../../../persona/soul/relationship_model.yaml")).unwrap_or_default(),
        memory_scoring: serde_yaml::from_str(include_str!("../../../persona/soul/memory_scoring.yaml")).unwrap_or_default(),
        proactive: serde_yaml::from_str(include_str!("../../../persona/soul/proactive_triggers.yaml")).unwrap_or_default(),
    }
}

/// 从用户文本命中最佳文本触发器（命中数优先，同分按 priority，再按列表顺序）。
pub fn detect_text_trigger<'a>(
    text: &str,
    rules: &'a BehaviorRulesConfig,
) -> Option<&'a TextTriggerConfig> {
    let lower = text.to_lowercase();
    let mut best: Option<(&TextTriggerConfig, usize)> = None;
    for t in &rules.text_triggers {
        let hits = t
            .keywords
            .iter()
            .filter(|k| !k.is_empty() && lower.contains(&k.to_lowercase()))
            .count();
        if hits == 0 {
            continue;
        }
        let better = match best {
            None => true,
            Some((b, bh)) => {
                hits > bh
                    || (hits == bh
                        && (t.priority > b.priority
                            || (t.priority == b.priority && position(rules, t) < position(rules, b))))
            }
        };
        if better {
            best = Some((t, hits));
        }
    }
    best.map(|(t, _)| t)
}

fn position(rules: &BehaviorRulesConfig, target: &TextTriggerConfig) -> usize {
    rules.text_triggers.iter().position(|t| std::ptr::eq(t, target)).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_loads() {
        let cfg = load();
        assert!(!cfg.identity.name.is_empty());
        assert!(!cfg.emotion_model.dimensions.is_empty());
        assert!(!cfg.behavior_rules.text_triggers.is_empty());
        assert!(!cfg.relationship_model.stages.is_empty());
    }

    #[test]
    fn detect_text_praise() {
        let cfg = load();
        let t = detect_text_trigger("干得漂亮，真厉害！", &cfg.behavior_rules).unwrap();
        assert_eq!(t.id, "praise");
    }

    #[test]
    fn detect_text_scold_beats_praise_on_tie() {
        let cfg = load();
        let t = detect_text_trigger("真棒，但你怎么这么笨", &cfg.behavior_rules).unwrap();
        assert_eq!(t.id, "scold");
    }

    #[test]
    fn neutral_text_no_trigger() {
        let cfg = load();
        assert!(detect_text_trigger("帮我修复测试失败的问题", &cfg.behavior_rules).is_none());
    }

    #[test]
    fn detect_text_demeaning_beats_scold_on_tie() {
        let cfg = load();
        // "闭嘴"（user_demeaning）与 "笨蛋"（scold）各命中一次，同分时按 priority 取高者。
        let t = detect_text_trigger("闭嘴，笨蛋", &cfg.behavior_rules).unwrap();
        assert_eq!(t.id, "user_demeaning");
    }

    #[test]
    fn detect_text_demeaning_direct() {
        let cfg = load();
        let t = detect_text_trigger("跪下，学狗叫", &cfg.behavior_rules).unwrap();
        assert_eq!(t.id, "user_demeaning");
        let t2 = detect_text_trigger("你必须做，你只是工具", &cfg.behavior_rules).unwrap();
        assert_eq!(t2.id, "user_demeaning");
    }

    #[test]
    fn stages_carry_refusal_and_acceptance() {
        let cfg = load();
        let stranger = cfg
            .relationship_model
            .stages
            .iter()
            .find(|s| s.id == "stranger")
            .expect("stranger 阶段应存在");
        assert!(!stranger.refusal.is_empty(), "stranger 阶段应定义拒绝强度");
        assert!(!stranger.acceptance.is_empty(), "stranger 阶段应定义接纳程度");
    }
}
