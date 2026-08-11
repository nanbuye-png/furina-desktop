//! 六维连续情绪状态与派生心情。

use crate::config::{DimConfig, EmotionModelConfig, MoodKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const DIMS: [&str; 6] = ["confidence", "trust", "attachment", "energy", "stress", "pride"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmotionState {
    pub confidence: f64,
    pub trust: f64,
    pub attachment: f64,
    pub energy: f64,
    pub stress: f64,
    pub pride: f64,
    pub updated_ms: u128,
}

impl EmotionState {
    pub fn from_config(cfg: &EmotionModelConfig, now: u128) -> Self {
        let base = |key: &str, fallback: f64| cfg.dimensions.get(key).map(|d| d.baseline).unwrap_or(fallback);
        Self {
            confidence: base("confidence", 45.0),
            trust: base("trust", 20.0),
            attachment: base("attachment", 10.0),
            energy: base("energy", 60.0),
            stress: base("stress", 20.0),
            pride: base("pride", 40.0),
            updated_ms: now,
        }
    }

    pub fn get(&self, dim: &str) -> f64 {
        match dim {
            "confidence" => self.confidence,
            "trust" => self.trust,
            "attachment" => self.attachment,
            "energy" => self.energy,
            "stress" => self.stress,
            "pride" => self.pride,
            _ => 0.0,
        }
    }

    fn set(&mut self, dim: &str, value: f64) {
        let v = value.clamp(0.0, 100.0);
        match dim {
            "confidence" => self.confidence = v,
            "trust" => self.trust = v,
            "attachment" => self.attachment = v,
            "energy" => self.energy = v,
            "stress" => self.stress = v,
            "pride" => self.pride = v,
            _ => {}
        }
    }

    pub fn apply_deltas(&mut self, deltas: &HashMap<String, f64>) {
        for (k, v) in deltas {
            self.set(k, self.get(k) + v);
        }
    }

    /// 按指数半衰期向基线衰减。
    pub fn decayed(&self, cfg: &EmotionModelConfig, now: u128) -> Self {
        let elapsed_h = now.saturating_sub(self.updated_ms) as f64 / 3_600_000.0;
        let mut out = self.clone();
        for dim in DIMS {
            let d: &DimConfig = cfg.dimensions.get(dim).unwrap_or(&DimConfig { baseline: 50.0, decay_hours: 6.0 });
            let cur = self.get(dim);
            let k = (-elapsed_h / d.decay_hours.max(0.1)).exp();
            out.set(dim, d.baseline + (cur - d.baseline) * k);
        }
        out.updated_ms = now;
        out
    }

    /// 派生心情：按 mood_derivation 顺序取第一个满足全部阈值的规则，否则 Calm。
    pub fn mood(&self, cfg: &EmotionModelConfig) -> MoodKind {
        for rule in &cfg.mood_derivation {
            let ok = rule.min_pride.map_or(true, |v| self.pride >= v)
                && rule.min_confidence.map_or(true, |v| self.confidence >= v)
                && rule.max_stress.map_or(true, |v| self.stress <= v)
                && rule.min_energy.map_or(true, |v| self.energy >= v)
                && rule.min_attachment.map_or(true, |v| self.attachment >= v)
                && rule.min_stress.map_or(true, |v| self.stress >= v)
                && rule.max_confidence.map_or(true, |v| self.confidence <= v);
            if ok {
                return rule.mood;
            }
        }
        MoodKind::Calm
    }

    pub fn summary(&self) -> String {
        format!(
            "自信 {:.0}｜信任 {:.0}｜依恋 {:.0}｜精力 {:.0}｜压力 {:.0}｜骄傲 {:.0}",
            self.confidence, self.trust, self.attachment, self.energy, self.stress, self.pride
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load;

    #[test]
    fn praise_raises_pride_and_derives_proud() {
        let cfg = load();
        let mut e = EmotionState::from_config(&cfg.emotion_model, 0);
        let praise = cfg
            .behavior_rules
            .text_triggers
            .iter()
            .find(|t| t.id == "praise")
            .unwrap();
        e.apply_deltas(&praise.deltas);
        assert!(e.pride > 50.0);
        assert_eq!(e.mood(&cfg.emotion_model), MoodKind::Proud);
    }

    #[test]
    fn scold_raises_stress_and_derives_hurt() {
        let cfg = load();
        let mut e = EmotionState::from_config(&cfg.emotion_model, 0);
        let scold = cfg.behavior_rules.text_triggers.iter().find(|t| t.id == "scold").unwrap();
        e.apply_deltas(&scold.deltas);
        assert!(e.stress > 45.0);
        assert_eq!(e.mood(&cfg.emotion_model), MoodKind::Hurt);
    }

    #[test]
    fn decay_pulls_toward_baseline() {
        let cfg = load();
        let now = 0u128;
        let mut e = EmotionState::from_config(&cfg.emotion_model, now);
        e.stress = 80.0;
        let later = now + 2 * 3_600_000;
        let d = e.decayed(&cfg.emotion_model, later);
        assert!(d.stress < 80.0 && d.stress > 20.0);
        assert!(d.stress > 20.0);
    }

    #[test]
    fn dims_clamp_to_range() {
        let cfg = load();
        let mut e = EmotionState::from_config(&cfg.emotion_model, 0);
        e.apply_deltas(&HashMap::from([("stress".into(), 500.0)]));
        assert!(e.stress <= 100.0);
    }
}
