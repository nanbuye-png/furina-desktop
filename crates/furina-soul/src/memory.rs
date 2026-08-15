//! 三类长期记忆（episodic / semantic / emotional）与重要性评分、检索。

use crate::config::{MemoryScoringConfig, MoodKind};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub timestamp: u128,
    pub importance_score: u8,
    pub valence: i8,
    pub tags: Vec<String>,
    #[serde(default)]
    pub emotion: Option<EmotionalMemoryContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EmotionalMemoryContext {
    pub mood: String,
    pub intensity: f64,
    pub cause: String,
    pub trigger_id: Option<String>,
    pub mood_before: String,
    pub mood_after: String,
    pub intensity_before: f64,
    pub intensity_after: f64,
    pub trend: String,
    pub unresolved: bool,
    pub deltas: Vec<(String, f64)>,
}

impl Default for EmotionalMemoryContext {
    fn default() -> Self {
        Self {
            mood: "calm".into(),
            intensity: 0.0,
            cause: String::new(),
            trigger_id: None,
            mood_before: "calm".into(),
            mood_after: "calm".into(),
            intensity_before: 0.0,
            intensity_after: 0.0,
            trend: "stable".into(),
            unresolved: false,
            deltas: Vec::new(),
        }
    }
}

/// 记忆候选（通常来自 LLM 抽取或规则事件），加入时做去重合并。
#[derive(Debug, Clone)]
pub struct MemoryDraft {
    pub kind: String,
    pub content: String,
    pub importance: u8,
    pub valence: i8,
    pub tags: Vec<String>,
}
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    pub records: Vec<MemoryRecord>,
    next_id: u64,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, kind: &str, content: impl Into<String>, importance: u8, valence: i8, tags: Vec<String>) {
        self.next_id += 1;
        self.records.push(MemoryRecord {
            id: format!("mem_{:04}", self.next_id),
            kind: kind.into(),
            content: content.into(),
            timestamp: crate::now_ms(),
            importance_score: importance.min(100),
            valence,
            tags,
            emotion: None,
        });
        if self.records.len() > 500 {
            self.prune();
        }
    }

    /// 规则触发的记忆：同类型同内容只保留一条（刷新时间戳与重要度），避免情绪记录堆叠。
    pub fn add_unique(&mut self, kind: &str, content: impl Into<String>, importance: u8, valence: i8, tags: Vec<String>) {
        let content = content.into();
        if let Some(existing) = self
            .records
            .iter_mut()
            .find(|r| r.kind == kind && r.content == content)
        {
            existing.importance_score = existing.importance_score.max(importance);
            existing.timestamp = crate::now_ms();
            if valence != 0 && existing.valence == 0 {
                existing.valence = valence;
            }
            for tag in &tags {
                if !existing.tags.contains(tag) {
                    existing.tags.push(tag.clone());
                }
            }
            return;
        }
        self.add(kind, content, importance, valence, tags);
    }

    pub fn add_emotional(
        &mut self,
        content: impl Into<String>,
        importance: u8,
        valence: i8,
        tags: Vec<String>,
        emotion: EmotionalMemoryContext,
    ) {
        self.add("emotional", content, importance, valence, tags);
        if let Some(record) = self.records.last_mut() {
            record.emotion = Some(emotion);
        }
    }

    pub fn add_emotional_unique(
        &mut self,
        content: impl Into<String>,
        importance: u8,
        valence: i8,
        tags: Vec<String>,
        emotion: EmotionalMemoryContext,
    ) {
        let content = content.into();
        if let Some(existing) = self
            .records
            .iter_mut()
            .find(|r| r.kind == "emotional" && r.content == content)
        {
            existing.importance_score = existing.importance_score.max(importance);
            existing.timestamp = crate::now_ms();
            if valence != 0 && existing.valence == 0 {
                existing.valence = valence;
            }
            for tag in &tags {
                if !existing.tags.contains(tag) {
                    existing.tags.push(tag.clone());
                }
            }
            existing.emotion = Some(emotion);
            return;
        }
        self.add_emotional(content, importance, valence, tags, emotion);
    }
    /// 去重合并地添加一批候选记忆：同类型且内容高度重叠时合并（取更高 importance）。
    pub fn add_drafts(&mut self, drafts: Vec<MemoryDraft>) {
        for d in drafts {
            let content = d.content.trim().to_string();
            if content.is_empty() {
                continue;
            }
            let fact_slot = d
                .tags
                .iter()
                .find(|tag| d.kind == "semantic" && tag.as_str() == "user_name")
                .cloned();
            if let Some(fact_slot) = fact_slot {
                if let Some(index) = self.records.iter().position(|record| {
                    record.kind == d.kind && (record.tags.iter().any(|tag| tag == &fact_slot) || (fact_slot == "user_name" && record.content.starts_with("用户姓名：")))
                }) {
                    let record_id = self.records[index].id.clone();
                    let existing = &mut self.records[index];
                    existing.content = content;
                    existing.importance_score = existing.importance_score.max(d.importance);
                    existing.timestamp = crate::now_ms();
                    for tag in &d.tags {
                        if !existing.tags.contains(tag) {
                            existing.tags.push(tag.clone());
                        }
                    }
                    self.records.retain(|record| {
                        record.id == record_id
                            || !(record.kind == d.kind
                                && (record.tags.iter().any(|tag| tag == &fact_slot) || (fact_slot == "user_name" && record.content.starts_with("用户姓名："))))
                    });
                    continue;
                }
            }
            let dup = self
                .records
                .iter_mut()
                .find(|r| r.kind == d.kind && (overlap(&r.content, &content) || similar(&r.content, &content)));
            match dup {
                Some(existing) => {
                    if d.importance > existing.importance_score {
                        existing.importance_score = d.importance;
                    }
                    if d.valence != 0 && existing.valence == 0 {
                        existing.valence = d.valence;
                    }
                    for tag in &d.tags {
                        if !existing.tags.contains(tag) {
                            existing.tags.push(tag.clone());
                        }
                    }
                    // 保留更完整的内容，避免同类记忆越并越碎。
                    if content.chars().count() > existing.content.chars().count() {
                        existing.content = content;
                    }
                }
                None => self.add(&d.kind, content, d.importance, d.valence, d.tags),
            }
        }
    }

    /// 按 id 删除单条记忆；返回是否删除成功。
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.records.len();
        self.records.retain(|r| r.id != id);
        self.records.len() != before
    }

    pub fn count(&self) -> usize {
        self.records.len()
    }

    pub fn clear(&mut self) {
        self.records.clear();
    }

    /// 按 重要性 × 新鲜度衰减 × 情绪匹配 检索 top-k。
    pub fn top_k(&self, now: u128, mood: MoodKind, cfg: &MemoryScoringConfig, k: usize) -> Vec<&MemoryRecord> {
        self.top_k_filtered(now, mood, cfg, k, None)
    }

    pub fn top_kind_k(
        &self,
        now: u128,
        mood: MoodKind,
        cfg: &MemoryScoringConfig,
        kind: &str,
        k: usize,
    ) -> Vec<&MemoryRecord> {
        self.top_k_filtered(now, mood, cfg, k, Some(kind))
    }

    fn top_k_filtered(
        &self,
        now: u128,
        mood: MoodKind,
        cfg: &MemoryScoringConfig,
        k: usize,
        kind: Option<&str>,
    ) -> Vec<&MemoryRecord> {
        let half_life = cfg.recency_half_life_hours.max(1.0);
        let mut scored: Vec<(&MemoryRecord, f64)> = self
            .records
            .iter()
            .filter(|record| kind.map(|expected| record.kind == expected).unwrap_or(true))
            .map(|r| {
                let elapsed_h = now.saturating_sub(r.timestamp) as f64 / 3_600_000.0;
                let recency = 0.5_f64.powf(elapsed_h / half_life);
                let match_boost = if r.valence != 0 {
                    if (mood.positive() && r.valence > 0) || (mood.negative() && r.valence < 0) {
                        1.2
                    } else {
                        1.0
                    }
                } else {
                    1.0
                };
                let fact_boost = if kind == Some("semantic") {
                    if r.tags.iter().any(|tag| tag == "user_name") {
                        1.5
                    } else if r.tags.iter().any(|tag| tag == "identity") {
                        1.35
                    } else if r.tags.iter().any(|tag| tag == "preference" || tag == "goal") {
                        1.2
                    } else {
                        1.0
                    }
                } else {
                    1.0
                };
                (r, r.importance_score as f64 * recency * match_boost * fact_boost)
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.into_iter().take(k).map(|(r, _)| r).collect()
    }

    pub fn top_emotional_k(
        &self,
        now: u128,
        mood: MoodKind,
        trigger_id: Option<&str>,
        cfg: &MemoryScoringConfig,
        k: usize,
    ) -> Vec<&MemoryRecord> {
        let half_life = cfg.recency_half_life_hours.max(1.0);
        let mut scored: Vec<(&MemoryRecord, f64)> = self
            .records
            .iter()
            .filter(|record| record.kind == "emotional")
            .map(|record| {
                let elapsed_h = now.saturating_sub(record.timestamp) as f64 / 3_600_000.0;
                let recency = 0.5_f64.powf(elapsed_h / half_life);
                let valence_boost = if record.valence != 0
                    && ((mood.positive() && record.valence > 0) || (mood.negative() && record.valence < 0))
                {
                    1.2
                } else {
                    1.0
                };
                let trigger_boost = trigger_id
                    .filter(|id| record.tags.iter().any(|tag| tag == *id))
                    .map(|_| 1.35)
                    .unwrap_or(1.0);
                (record, record.importance_score as f64 * recency * valence_boost * trigger_boost)
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.into_iter().take(k).map(|(record, _)| record).collect()
    }
    /// 按重要性评分公式计算（design: Σ weight×factor×100）。
    pub fn compute_importance(cfg: &MemoryScoringConfig, emphasis: f64, emotion: f64, relation: f64, future: f64) -> u8 {
        let w = |k: &str, fallback: f64| cfg.factors.get(k).copied().unwrap_or(fallback);
        let score = (w("user_emphasis", 0.25) * emphasis
            + w("emotional_strength", 0.25) * emotion
            + w("relationship_impact", 0.20) * relation
            + w("future_usefulness", 0.30) * future)
            * 100.0;
        (score.clamp(0.0, 100.0)) as u8
    }

    fn prune(&mut self) {
        self.records.sort_by(|a, b| b.importance_score.cmp(&a.importance_score));
        self.records.truncate(500);
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;
        for kind in ["episodic", "semantic", "emotional"] {
            let path = dir.join(format!("{kind}.jsonl"));
            let mut out = String::new();
            for r in self.records.iter().filter(|r| r.kind == kind) {
                out.push_str(&serde_json::to_string(r)?);
                out.push('\n');
            }
            std::fs::write(&path, out)?;
        }
        Ok(())
    }

    pub fn load(dir: &Path) -> Self {
        let mut store = Self::new();
        for kind in ["episodic", "semantic", "emotional"] {
            let path = dir.join(format!("{kind}.jsonl"));
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            for line in text.lines() {
                if let Ok(r) = serde_json::from_str::<MemoryRecord>(line) {
                    if r.id.starts_with("mem_") {
                        if let Ok(n) = r.id.trim_start_matches("mem_").parse::<u64>() {
                            store.next_id = store.next_id.max(n);
                        }
                    }
                    store.records.push(r);
                }
            }
        }
        store
    }
}

/// 归一化后的包含/相等判断，用于记忆去重。
fn overlap(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let na = norm(a);
    let nb = norm(b);
    na.len() >= 4 && (na.contains(&nb) || nb.contains(&na) || na == nb)
}

/// 中文按字符二元组计算 Jaccard 相似度；措辞不同但语义重复时用于合并。
fn similar(a: &str, b: &str) -> bool {
    let bigrams = |s: &str| -> Vec<String> {
        let chars: Vec<char> = s
            .chars()
            .filter(|c| {
                !c.is_whitespace() && !matches!(c, '。' | '，' | ',' | '．' | '！' | '？' | '“' | '”' | '‘' | '’' | '「' | '」')
            })
            .collect();
        chars.windows(2).map(|w| w.iter().collect()).collect()
    };
    let ga = bigrams(a);
    let gb = bigrams(b);
    if ga.is_empty() || gb.is_empty() {
        return false;
    }
    let (small, large) = if ga.len() <= gb.len() { (&ga, &gb) } else { (&gb, &ga) };
    let inter = small.iter().filter(|g| large.contains(g)).count();
    (inter as f64) / (large.len() as f64) >= 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load;

    #[test]
    fn scoring_weights() {
        let cfg = load();
        let s = MemoryStore::compute_importance(&cfg.memory_scoring, 1.0, 0.2, 0.1, 0.5);
        assert!(s > 40 && s <= 100);
    }

    #[test]
    fn user_name_slot_updates_and_semantic_retrieval_prioritizes_identity() {
        let mut store = MemoryStore::new();
        store.add_drafts(vec![MemoryDraft {
            kind: "semantic".into(),
            content: "用户姓名：甲".into(),
            importance: 95,
            valence: 0,
            tags: vec!["user_stated".into(), "identity".into(), "user_name".into()],
        }]);
        store.add_drafts(vec![MemoryDraft {
            kind: "semantic".into(),
            content: "用户姓名：念逐野".into(),
            importance: 95,
            valence: 0,
            tags: vec!["user_stated".into(), "identity".into(), "user_name".into()],
        }]);
        let names: Vec<_> = store
            .records
            .iter()
            .filter(|record| record.tags.iter().any(|tag| tag == "user_name"))
            .collect();
        assert_eq!(names.len(), 1);
        assert_eq!(names[0].content, "用户姓名：念逐野");
        let cfg = load();
        let top = store.top_kind_k(crate::now_ms(), MoodKind::Calm, &cfg.memory_scoring, "semantic", 1);
        assert_eq!(top[0].content, "用户姓名：念逐野");
    }

    #[test]
    fn top_k_prefers_important_and_recent() {
        let cfg = load();
        let mut m = MemoryStore::new();
        m.next_id = 100;
        m.add("semantic", "用户喜欢咖啡", 30, 0, vec![]);
        m.add("episodic", "第一次帮助用户完成大型项目", 95, 1, vec![]);
        let top = m.top_k(crate::now_ms(), MoodKind::Calm, &cfg.memory_scoring, 5);
        assert_eq!(top.len(), 2);
        assert!(top[0].content.contains("大型项目"));
    }

    #[test]
    fn drafts_deduplicate_and_merge() {
        let mut m = MemoryStore::new();
        m.add_drafts(vec![
            MemoryDraft { kind: "semantic".into(), content: "用户喜欢咖啡".into(), importance: 30, valence: 0, tags: vec![] },
            MemoryDraft { kind: "semantic".into(), content: "用户喜欢咖啡，每天一杯".into(), importance: 70, valence: 0, tags: vec![] },
            MemoryDraft { kind: "episodic".into(), content: "共同完成大型项目".into(), importance: 90, valence: 1, tags: vec![] },
        ]);
        assert_eq!(m.records.len(), 2);
        let sem = m.records.iter().find(|r| r.kind == "semantic").unwrap();
        assert_eq!(sem.importance_score, 70);
    }

    #[test]
    fn drafts_merge_when_similar_wording() {
        let mut m = MemoryStore::new();
        m.add_drafts(vec![
            MemoryDraft {
                kind: "semantic".into(),
                content: "用户明确要求Furina记住你是我的宝贝，Furina接受了这个称呼，限定只有用户一人可以这样称呼".into(),
                importance: 80,
                valence: 1,
                tags: vec!["关系".into(), "称呼".into()],
            },
            MemoryDraft {
                kind: "semantic".into(),
                content: "用户喜欢用宝贝称呼Furina，且Furina接受了这个专属称呼，只有用户一人可以这样叫".into(),
                importance: 95,
                valence: 1,
                tags: vec!["关系".into()],
            },
        ]);
        assert_eq!(m.records.len(), 1, "措辞不同但语义重复的记忆应合并");
        let sem = &m.records[0];
        assert_eq!(sem.importance_score, 95, "合并应保留更高重要度");
        assert!(sem.tags.contains(&"称呼".to_string()), "合并应合并标签");
    }

    #[test]
    fn add_unique_dedupes_same_content() {
        let mut m = MemoryStore::new();
        m.add_unique("emotional", "用户夸奖了你", 60, 1, vec!["praise".into()]);
        m.add_unique("emotional", "用户夸奖了你", 60, 1, vec!["praise".into()]);
        assert_eq!(m.count(), 1, "相同内容只保留一条");
        m.add_unique("emotional", "用户夸奖了你", 70, 1, vec!["praise".into()]);
        assert_eq!(m.records[0].importance_score, 70);
    }

    #[test]
    fn remove_by_id_and_count() {
        let mut m = MemoryStore::new();
        m.add("semantic", "a", 50, 0, vec![]);
        m.add("semantic", "b", 50, 0, vec![]);
        let id = m.records[0].id.clone();
        assert!(m.remove(&id));
        assert_eq!(m.count(), 1);
        assert!(!m.remove(&id), "重复删除应返回 false");
    }

    #[test]
    fn clear_empties_store() {
        let mut m = MemoryStore::new();
        m.add("semantic", "x", 50, 0, vec![]);
        m.clear();
        assert!(m.records.is_empty());
    }

    #[test]
    fn persistence_round_trip() {
        let dir = std::env::temp_dir().join(format!("furina_soul_mem_{}", std::process::id()));
        let mut m = MemoryStore::new();
        m.add("semantic", "测试记忆", 50, 0, vec!["test".into()]);
        m.save(&dir).unwrap();
        let loaded = MemoryStore::load(&dir);
        assert_eq!(loaded.records.len(), 1);
        assert_eq!(loaded.records[0].content, "测试记忆");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn legacy_record_without_emotion_context_is_compatible() {
        let json = r#"{"id":"mem_0001","kind":"emotional","content":"旧记忆","timestamp":1,"importance_score":60,"valence":-1,"tags":["legacy"]}"#;
        let record: MemoryRecord = serde_json::from_str(json).unwrap();
        assert!(record.emotion.is_none());
    }

    #[test]
    fn emotional_retrieval_boosts_matching_trigger() {
        let cfg = load();
        let context = EmotionalMemoryContext {
            cause: "用户认真道歉".into(),
            trigger_id: Some("sincere_apology".into()),
            ..Default::default()
        };
        let mut store = MemoryStore::new();
        store.add_emotional("道歉经历", 60, 1, vec!["sincere_apology".into()], context);
        let top = store.top_emotional_k(
            crate::now_ms(),
            MoodKind::Happy,
            Some("sincere_apology"),
            &cfg.memory_scoring,
            1,
        );
        assert_eq!(top[0].content, "道歉经历");
        assert!(top[0].emotion.is_some());
    }
}
