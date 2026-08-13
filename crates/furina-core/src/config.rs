//! Configuration loaded from `.furina/config.yaml` (serde defaults mirror the
//! shipped template).

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub model: String,
    pub llm: LlmConfig,
    pub agent: AgentConfig,
    pub persona: String,
    pub approval: ApprovalConfig,
    pub ui: UiConfig,
    pub web: WebConfig,
    pub web_cache: WebCacheConfig,
    pub vision: VisionConfig,
    pub voice: VoiceConfig,
    pub asr: AsrConfig,
    pub qwen: QwenConfig,
    pub interject: InterjectConfig,
    pub emotion_classifier: EmotionClassifierConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LlmConfig {
    /// 旧版单提供方字段（providers 为空时回退使用）
    pub base_url: String,
    pub api_key_env: String,
    pub temperature: f64,
    pub max_total_tokens: u64,
    pub per_request_max_tokens: usize,
    pub tool_output_truncate: usize,
    /// 网关：激活的提供方 id
    pub active_provider: Option<String>,
    /// 网关：全部模型提供方（OpenAI 兼容端点，含中转站）
    pub providers: Vec<ProviderConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub api_key_env: String,
    pub model: String,
    #[serde(default)]
    pub vision: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentConfig {
    pub max_repair_rounds: u32,
    pub max_steps_per_task: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ApprovalConfig {
    pub mode: String,
    pub auto_allow: Vec<String>,
    pub danger_patterns: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct UiConfig {
    /// Desktop UI 使用该配置段保留兼容性；当前主界面由 Tauri Desktop 启动。
    pub mode: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct WebConfig {
    /// none | tavily | bing | searxng
    pub search_backend: String,
    pub api_key_env: String,
    pub endpoint: String,
    pub max_results: usize,
    /// 主后端失败时的回退后端（如 sogou，国内直连可用）；空表示不回退。
    #[serde(default)]
    pub fallback_backend: String,
    /// 回退后端端点覆盖（默认按后端取内置端点）。
    #[serde(default)]
    pub fallback_endpoint: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct WebCacheConfig {
    /// 是否启用自动清理（启动会话时 + TUI 常驻每日）。
    pub enabled: bool,
    /// 缓存保留天数，超过即清理。
    pub retention_days: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct VisionConfig {
    /// 是否启用图片识别（视觉预处理代理）。
    pub enabled: bool,
    /// auto=按列表顺序取第一个 vision:true 且 key 已设置的提供方；或指定 provider id。
    pub preferred_provider: String,
    /// 单张图片大小上限（字节）。
    pub max_image_bytes: u64,
    /// 允许的图片扩展名。
    pub allowed_formats: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct VoiceConfig {
    /// 是否启用语音合成。
    pub enabled: bool,
    /// 用户可见的 TTS 服务名称；请求格式由 protocol 决定。
    pub provider: String,
    /// fish | qwen_omni | openai；空值按旧 provider 自动推导。
    pub protocol: String,
    /// 读取 API key 的环境变量（优先 .furina/secrets.env）。
    pub api_key_env: String,
    /// 完整 TTS 请求地址；qwen_omni 兼容旧配置时可回退 qwen.base_url。
    pub endpoint: String,
    /// TTS 模型名称。
    pub model: String,
    /// 声音模型 ID（reference_id，如鱼声平台的角色音色包）。
    pub reference_id: String,
    /// 通用音色名称或 ID（OpenAI Speech / Qwen Omni）。
    pub voice: String,
    /// 输出音频格式：mp3 / wav / flac。
    pub format: String,
    /// 单次合成文本上限（字符）。
    pub max_text_len: usize,
    /// 朗读语速（0.5–2.0，默认 1.0）；会话内可用 /语速 调整。
    pub speed: f64,
    /// 会话启动时是否自动朗读回复（可用 /语音 随时切换）。
    pub auto_play: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AsrConfig {
    /// 是否启用语音识别。
    pub enabled: bool,
    /// 用户可见的 ASR 服务名称；请求格式由 protocol 决定。
    pub provider: String,
    /// fish | qwen_omni | openai；空值按旧 provider 自动推导。
    pub protocol: String,
    /// 读取 API key 的环境变量。
    pub api_key_env: String,
    /// 识别语言（ISO 码，默认 zh）。
    pub language: String,
    /// 完整 ASR 请求地址；qwen_omni 兼容旧配置时可回退 qwen.base_url。
    pub endpoint: String,
    /// ASR 模型名称。
    pub model: String,
    /// 可选转写提示词。
    pub prompt: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct QwenConfig {
    /// 读取 API key 的环境变量（阿里百炼/DashScope）。
    pub api_key_env: String,
    /// OpenAI 兼容端点（DashScope）。
    pub base_url: String,
    /// ASR 模型（Omni 多模态，免费额度）。
    pub asr_model: String,
    /// 转写提示词（要求模型只输出转写文本）。
    pub asr_prompt: String,
    /// TTS 模型（Omni 多模态，免费额度，输出音频）。
    pub tts_model: String,
    /// TTS 固定音色名（如 Tina / Seraphina / Ethan 等）。
    pub tts_voice: String,
}

/// 执行关键节点的人格化插话（LLM 生成，展示层辅助）。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct InterjectConfig {
    /// 是否启用：审批通过/拒绝、任务完成/失败、验证失败时生成一句人格插话。
    pub enabled: bool,
    /// 每个任务最多生成几句插话（0=不限制），防止长任务噪音。
    pub max_per_task: u32,
    /// 插话生成采样温度（越高越多样）。
    pub temperature: f64,
    /// 插话输出截断上限（字符）。
    pub max_chars: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct EmotionClassifierConfig {
    pub enabled: bool,
    pub provider_id: String,
    pub model: String,
    pub timeout_ms: u64,
    pub max_tokens: usize,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.deepseek.com".into(),
            api_key_env: "FURINA_API_KEY".into(),
            temperature: 0.2,
            max_total_tokens: 1_000_000,
            per_request_max_tokens: 56_000,
            tool_output_truncate: 6_000,
            active_provider: Some("deepseek".into()),
            providers: vec![ProviderConfig {
                id: "deepseek".into(),
                label: "DeepSeek".into(),
                base_url: "https://api.deepseek.com".into(),
                api_key_env: "FURINA_API_KEY".into(),
                model: "deepseek-chat".into(),
                vision: false,
            }],
        }
    }
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            base_url: String::new(),
            api_key_env: String::new(),
            model: String::new(),
            vision: false,
        }
    }
}

impl Default for VisionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            preferred_provider: "auto".into(),
            max_image_bytes: 4 * 1024 * 1024,
            allowed_formats: vec!["png".into(), "jpg".into(), "jpeg".into(), "webp".into()],
        }
    }
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "fish".into(),
            protocol: String::new(),
            api_key_env: "FISH_AUDIO_API_KEY".into(),
            endpoint: "https://api.fish.audio/v1/tts".into(),
            model: "s2.1-pro-free".into(),
            reference_id: String::new(),
            voice: String::new(),
            format: "mp3".into(),
            max_text_len: 1000,
            speed: 1.0,
            auto_play: false,
        }
    }
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "qwen".into(),
            protocol: String::new(),
            api_key_env: "FISH_AUDIO_API_KEY".into(),
            language: "zh".into(),
            endpoint: "https://api.fish.audio/v1/asr".into(),
            model: String::new(),
            prompt: String::new(),
        }
    }
}

impl Default for QwenConfig {
    fn default() -> Self {
        Self {
            api_key_env: "QWEN_API_KEY".into(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            asr_model: "qwen3.5-omni-flash".into(),
            asr_prompt: "请把这段音频中的话转写成文字，只输出转写结果，不要任何额外说明。".into(),
            tts_model: "qwen3.5-omni-flash".into(),
            tts_voice: "Tina".into(),
        }
    }
}

impl Default for InterjectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_per_task: 3,
            temperature: 0.9,
            max_chars: 60,
        }
    }
}

impl Default for EmotionClassifierConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider_id: String::new(),
            model: String::new(),
            timeout_ms: 1500,
            max_tokens: 256,
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self { max_repair_rounds: 3, max_steps_per_task: 50 }
    }
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        Self {
            mode: "mixed".into(),
            auto_allow: vec![
                "pytest".into(),
                "python -m pytest".into(),
                "python -m unittest".into(),
                "unittest".into(),
                "cargo test".into(),
                "npm test".into(),
                "mvn test".into(),
                "go test".into(),
                "git status".into(),
                "git diff".into(),
                "git log".into(),
            ],
            danger_patterns: vec![
                "rm -rf".into(),
                "rm -fr".into(),
                "del /s".into(),
                "rd /s".into(),
                "format c:".into(),
                "drop database".into(),
                "git push --force".into(),
                "git push -f".into(),
                "mkfs".into(),
                "shutdown".into(),
                ":(){".into(),
            ],
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self { mode: "chat".into() }
    }
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            search_backend: "none".into(),
            api_key_env: String::new(),
            endpoint: String::new(),
            max_results: 5,
            fallback_backend: String::new(),
            fallback_endpoint: String::new(),
        }
    }
}

impl Default for WebCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 3,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: "deepseek-chat".into(),
            llm: LlmConfig::default(),
            agent: AgentConfig::default(),
            persona: "furina".into(),
            approval: ApprovalConfig::default(),
            ui: UiConfig::default(),
            web: WebConfig::default(),
            web_cache: WebCacheConfig::default(),
            vision: VisionConfig::default(),
            voice: VoiceConfig::default(),
            asr: AsrConfig::default(),
            qwen: QwenConfig::default(),
            interject: InterjectConfig::default(),
            emotion_classifier: EmotionClassifierConfig::default(),
        }
    }
}

impl Config {
    /// Load config from a YAML file; missing file falls back to defaults.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Ok(Config::default()),
        };
        let cfg: Config = serde_yaml::from_str(&text)?;
        Ok(cfg)
    }

    /// 解析当前激活的模型提供方；providers 为空时回退到旧版单提供方字段。
    pub fn active_provider(&self) -> anyhow::Result<ProviderConfig> {
        if !self.llm.providers.is_empty() {
            let id = self
                .llm
                .active_provider
                .as_deref()
                .unwrap_or(self.llm.providers[0].id.as_str());
            let p = self
                .llm
                .providers
                .iter()
                .find(|p| p.id == id)
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "未找到激活的模型提供方: {id}（请检查 .furina/config.yaml 的 llm.active_provider）"
                    )
                });
            return p.map(|p| self.resolved_provider(&p));
        }
        Ok(ProviderConfig {
            id: "default".into(),
            label: self.model.clone(),
            base_url: self.llm.base_url.clone(),
            api_key_env: self.llm.api_key_env.clone(),
            model: self.model.clone(),
            vision: false,
        })
    }

    pub fn provider(&self, id: &str) -> Option<ProviderConfig> {
        self.llm
            .providers
            .iter()
            .find(|p| p.id == id)
            .map(|p| self.resolved_provider(p))
    }

    /// 环境变量名：`{ID}_BASE_URL` / `{ID}_MODEL`（ID 大写、非字母数字转 `_`）。
    fn env_override_name(id: &str, suffix: &str) -> String {
        let norm: String = id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
            .collect();
        format!("{norm}_{suffix}")
    }

    /// 应用 secrets.env 覆盖（非空即覆盖 YAML 默认值）。
    pub fn resolved_provider(&self, p: &ProviderConfig) -> ProviderConfig {
        let mut out = p.clone();
        if let Ok(v) = std::env::var(Self::env_override_name(&p.id, "BASE_URL")) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                out.base_url = v;
            }
        }
        if let Ok(v) = std::env::var(Self::env_override_name(&p.id, "MODEL")) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                out.model = v;
            }
        }
        out
    }

    /// 该提供方是否被 secrets.env 覆盖了 base_url / model。
    pub fn provider_overridden(&self, id: &str) -> bool {
        let base = std::env::var(Self::env_override_name(id, "BASE_URL"))
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        let model = std::env::var(Self::env_override_name(id, "MODEL"))
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        base || model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_template() {
        let cfg = Config::default();
        assert_eq!(cfg.model, "deepseek-chat");
        assert_eq!(cfg.llm.api_key_env, "FURINA_API_KEY");
        assert_eq!(cfg.agent.max_repair_rounds, 3);
        assert!(cfg.approval.auto_allow.iter().any(|a| a == "pytest"));
        assert_eq!(cfg.ui.mode, "chat");
        assert_eq!(cfg.web.search_backend, "none");
        assert!(!cfg.voice.enabled);
        assert_eq!(cfg.voice.api_key_env, "FISH_AUDIO_API_KEY");
        assert_eq!(cfg.voice.endpoint, "https://api.fish.audio/v1/tts");
        assert_eq!(cfg.voice.format, "mp3");
        assert_eq!(cfg.voice.speed, 1.0);
        assert!(!cfg.voice.auto_play);
        assert!(!cfg.asr.enabled);
        assert_eq!(cfg.asr.api_key_env, "FISH_AUDIO_API_KEY");
        assert_eq!(cfg.asr.language, "zh");
        assert!(cfg.interject.enabled);
        assert_eq!(cfg.interject.max_per_task, 3);
        assert_eq!(cfg.interject.temperature, 0.9);
        assert_eq!(cfg.interject.max_chars, 60);
        let p = cfg.active_provider().unwrap();
        assert_eq!(p.id, "deepseek");
        assert_eq!(p.base_url, "https://api.deepseek.com");
    }

    #[test]
    fn legacy_voice_yaml_uses_defaults_for_new_fields() {
        let cfg: Config = serde_yaml::from_str(r#"
voice:
  enabled: true
  provider: fish
  endpoint: https://api.fish.audio/v1/tts
  model: s2.1-pro-free
asr:
  enabled: true
  provider: qwen
qwen:
  base_url: https://dashscope.example/v1
  asr_model: qwen-omni
"#).unwrap();
        assert!(cfg.voice.protocol.is_empty());
        assert!(cfg.voice.voice.is_empty());
        assert!(cfg.asr.protocol.is_empty());
        assert!(cfg.asr.model.is_empty());
        assert_eq!(cfg.qwen.asr_model, "qwen-omni");
    }

    #[test]
    fn custom_voice_yaml_round_trips_protocol_fields() {
        let mut cfg = Config::default();
        cfg.voice.provider = "Local TTS".into();
        cfg.voice.protocol = "openai".into();
        cfg.voice.voice = "furina".into();
        cfg.asr.provider = "Local ASR".into();
        cfg.asr.protocol = "openai".into();
        cfg.asr.model = "whisper-local".into();
        cfg.asr.prompt = "专有名词".into();
        let text = serde_yaml::to_string(&cfg).unwrap();
        let restored: Config = serde_yaml::from_str(&text).unwrap();
        assert_eq!(restored.voice.protocol, "openai");
        assert_eq!(restored.voice.voice, "furina");
        assert_eq!(restored.asr.model, "whisper-local");
        assert_eq!(restored.asr.prompt, "专有名词");
    }

    #[test]
    fn active_provider_resolves_from_list() {
        let mut cfg = Config::default();
        cfg.llm.providers = vec![
            ProviderConfig { id: "a".into(), label: "A".into(), base_url: "https://a".into(), api_key_env: "KEY_A".into(), model: "m-a".into(), vision: false },
            ProviderConfig { id: "b".into(), label: "B".into(), base_url: "https://b".into(), api_key_env: "KEY_B".into(), model: "m-b".into(), vision: true },
        ];
        cfg.llm.active_provider = Some("b".into());
        let p = cfg.active_provider().unwrap();
        assert_eq!(p.id, "b");
        assert_eq!(p.api_key_env, "KEY_B");
        assert!(p.vision, "vision 标志应随提供方解析");
    }

    #[test]
    fn active_provider_falls_back_to_legacy() {
        let mut cfg = Config::default();
        cfg.llm.providers = vec![];
        cfg.llm.active_provider = None;
        let p = cfg.active_provider().unwrap();
        assert_eq!(p.id, "default");
        assert_eq!(p.model, "deepseek-chat");
    }

    #[test]
    fn active_provider_missing_id_errors() {
        let mut cfg = Config::default();
        cfg.llm.active_provider = Some("nope".into());
        assert!(cfg.active_provider().is_err());
    }

    #[test]
    fn env_override_applies_and_falls_back() {
        let mut cfg = Config::default();
        cfg.llm.providers = vec![ProviderConfig {
            id: "envtest".into(),
            label: "EnvTest".into(),
            base_url: "https://yaml-default".into(),
            api_key_env: "ENVTEST_KEY".into(),
            model: "yaml-model".into(),
            vision: false,
        }];
        cfg.llm.active_provider = Some("envtest".into());

        std::env::set_var("ENVTEST_BASE_URL", "https://env-override/v1");
        std::env::set_var("ENVTEST_MODEL", "gpt-4o-mini");
        let p = cfg.active_provider().unwrap();
        assert_eq!(p.base_url, "https://env-override/v1");
        assert_eq!(p.model, "gpt-4o-mini");
        assert!(cfg.provider_overridden("envtest"));
        assert_eq!(cfg.provider("envtest").unwrap().base_url, "https://env-override/v1");

        std::env::remove_var("ENVTEST_BASE_URL");
        std::env::remove_var("ENVTEST_MODEL");
        let p = cfg.active_provider().unwrap();
        assert_eq!(p.base_url, "https://yaml-default");
        assert_eq!(p.model, "yaml-model");
        assert!(!cfg.provider_overridden("envtest"));
    }

    #[test]
    fn env_override_name_normalizes_id() {
        assert_eq!(
            Config::env_override_name("my-relay-1", "BASE_URL"),
            "MY_RELAY_1_BASE_URL"
        );
        assert_eq!(Config::env_override_name("deepseek", "MODEL"), "DEEPSEEK_MODEL");
    }

    #[test]
    fn vision_defaults_off() {
        let cfg = Config::default();
        assert!(!cfg.vision.enabled);
        assert_eq!(cfg.vision.preferred_provider, "auto");
        assert_eq!(cfg.vision.max_image_bytes, 4 * 1024 * 1024);
        assert!(cfg.vision.allowed_formats.contains(&"png".to_string()));
    }
}
