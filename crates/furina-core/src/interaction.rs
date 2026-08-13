use crate::config::{EmotionClassifierConfig, ProviderConfig};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;

const ALLOWED_TRIGGERS: &[&str] = &[
    "theatrical_request",
    "identity_question",
    "loneliness_question",
    "jealousy_cue",
    "praise",
    "scold",
    "confide",
    "sad",
    "happy",
    "annoyed",
    "user_demeaning",
    "sincere_apology",
    "cursory_apology",
];

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct InteractionAnalysis {
    pub interaction_type: String,
    pub intent_category: String,
    pub trigger_id: Option<String>,
    pub tease: bool,
    pub insult: bool,
    pub apology: bool,
    pub closeness: bool,
    pub denial: bool,
    pub jealousy: bool,
    pub severity: u8,
    pub confidence: f64,
}

impl InteractionAnalysis {
    pub fn accepted_trigger(&self) -> Option<&str> {
        if self.confidence < 0.6 {
            return None;
        }
        self.trigger_id
            .as_deref()
            .filter(|trigger| ALLOWED_TRIGGERS.contains(trigger))
    }

    fn valid(&self) -> bool {
        self.confidence.is_finite()
            && (0.0..=1.0).contains(&self.confidence)
            && self.severity <= 3
            && self
                .trigger_id
                .as_deref()
                .map(|trigger| ALLOWED_TRIGGERS.contains(&trigger))
                .unwrap_or(true)
    }
}

#[async_trait]
pub trait InteractionAnalyzer: Send + Sync {
    async fn analyze(
        &self,
        current_text: &str,
        recent_dialogue: &[(String, String)],
    ) -> anyhow::Result<InteractionAnalysis>;
}

pub struct HttpInteractionAnalyzer {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: usize,
}

impl HttpInteractionAnalyzer {
    pub fn new(
        provider: ProviderConfig,
        api_key: String,
        config: &EmotionClassifierConfig,
    ) -> anyhow::Result<Self> {
        let client = crate::proxy::apply_system_proxy(
            reqwest::Client::builder()
                .connect_timeout(Duration::from_millis(config.timeout_ms.clamp(500, 1500)))
                .timeout(Duration::from_millis(config.timeout_ms.clamp(500, 1500))),
        )
        .build()?;
        Ok(Self {
            client,
            base_url: provider.base_url,
            api_key,
            model: config.model.trim().to_string(),
            max_tokens: config.max_tokens.clamp(32, 512),
        })
    }

    fn messages(&self, current_text: &str, recent_dialogue: &[(String, String)]) -> serde_json::Value {
        let recent = recent_dialogue
            .iter()
            .map(|(role, content)| format!("{role}: {}", truncate(content, 600)))
            .collect::<Vec<_>>()
            .join("\n");
        serde_json::json!([
            {
                "role": "system",
                "content": "你是 Furina Desktop 的轻量交互分类器，只分析用户语气，不生成角色台词。只输出一个 JSON 对象，不要 Markdown。字段：interaction_type、intent_category、trigger_id、tease、insult、apology、closeness、denial、jealousy、severity、confidence。trigger_id 只能是 theatrical_request、identity_question、loneliness_question、jealousy_cue、praise、scold、confide、sad、happy、annoyed、user_demeaning、sincere_apology、cursory_apology 或 null。severity 为 0 到 3，confidence 为 0 到 1。区分轻松调侃与真实羞辱，区分敷衍道歉与真诚道歉。"
            },
            {
                "role": "user",
                "content": format!("最近对话（可能为空）：\n{recent}\n\n当前用户输入：\n{}", truncate(current_text, 1200))
            }
        ])
    }
}

#[async_trait]
impl InteractionAnalyzer for HttpInteractionAnalyzer {
    async fn analyze(
        &self,
        current_text: &str,
        recent_dialogue: &[(String, String)],
    ) -> anyhow::Result<InteractionAnalysis> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": self.model,
                "messages": self.messages(current_text, recent_dialogue),
                "max_tokens": self.max_tokens,
                "temperature": 0.0,
                "stream": false,
                "response_format": { "type": "json_object" },
            }))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("情绪分类器 HTTP {status}");
        }
        let envelope: serde_json::Value = serde_json::from_str(&body)?;
        let content = envelope["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("情绪分类器响应缺少 content"))?;
        parse_analysis(content).ok_or_else(|| anyhow::anyhow!("情绪分类器返回了无效 JSON"))
    }
}

pub async fn analyze_with_fallback(
    analyzer: &dyn InteractionAnalyzer,
    current_text: &str,
    recent_dialogue: &[(String, String)],
    timeout_ms: u64,
) -> Option<InteractionAnalysis> {
    let timeout = Duration::from_millis(timeout_ms.clamp(500, 1500));
    match tokio::time::timeout(timeout, analyzer.analyze(current_text, recent_dialogue)).await {
        Ok(Ok(analysis)) if analysis.valid() && analysis.confidence >= 0.6 => Some(analysis),
        _ => None,
    }
}

fn parse_analysis(text: &str) -> Option<InteractionAnalysis> {
    let trimmed = text.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .strip_suffix("```")
        .unwrap_or(trimmed)
        .trim();
    let analysis: InteractionAnalysis = serde_json::from_str(json).ok()?;
    analysis.valid().then_some(analysis)
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut out = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SlowAnalyzer;

    #[async_trait]
    impl InteractionAnalyzer for SlowAnalyzer {
        async fn analyze(
            &self,
            _current_text: &str,
            _recent_dialogue: &[(String, String)],
        ) -> anyhow::Result<InteractionAnalysis> {
            tokio::time::sleep(Duration::from_millis(650)).await;
            Ok(InteractionAnalysis { confidence: 0.9, trigger_id: Some("scold".into()), ..Default::default() })
        }
    }

    #[test]
    fn parses_strict_json_and_code_fence() {
        let analysis = parse_analysis(r#"```json
{"interaction_type":"tease","intent_category":"social","trigger_id":"scold","tease":true,"severity":1,"confidence":0.91}
```"#).unwrap();
        assert_eq!(analysis.accepted_trigger(), Some("scold"));
        assert!(analysis.tease);
    }

    #[test]
    fn rejects_unknown_trigger_and_invalid_confidence() {
        assert!(parse_analysis(r#"{"trigger_id":"delete_files","confidence":0.9}"#).is_none());
        assert!(parse_analysis(r#"{"trigger_id":"scold","confidence":2.0}"#).is_none());
    }

    #[tokio::test]
    async fn timeout_returns_rule_fallback_signal() {
        let result = analyze_with_fallback(&SlowAnalyzer, "你是笨蛋", &[], 500).await;
        assert!(result.is_none());
    }
}