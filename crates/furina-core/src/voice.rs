//! 语音合成（TTS）：fish.audio 或 Qwen-Omni（DashScope，免费固定音色）→ 音频。
//!
//! 人格情绪只通过句首情绪标记（如 `[happy]`）影响语气，不改变任何技术事实；
//! 合成请求外发到对应 API，生成文件只落在 `.furina/voice/`（已 gitignore）。
//!
//! provider 选择（config.yaml `voice.provider`）：
//! - `fish`（默认）：fish.audio `/v1/tts`。免费模型 `s2.1-pro-free` 不传 reference_id
//!   时每次随机音色；自购音色包传 reference_id 后音色固定但按量计费（无额度返回 402）。
//! - `qwen`：DashScope qwen3.5-omni-flash（免费额度），`audio.voice` 指定固定音色，
//!   必须 `stream: true`，SSE 里取 `delta.audio.data`（base64 → wav）。

use crate::config::Config;
use crate::proxy::apply_system_proxy;
use base64::Engine as _;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// TTS 客户端：按 `voice.provider` 选择后端。
#[derive(Clone, Debug)]
pub struct VoiceClient {
    client: reqwest::Client,
    kind: VoiceKind,
    provider: String,
    /// 输出格式：fish/openai 为配置值；qwen_omni 固定 wav。
    format: String,
    max_text_len: usize,
    output_dir: PathBuf,
}

#[derive(Clone, Debug)]
enum VoiceKind {
    Fish {
        api_key: String,
        endpoint: String,
        model: String,
        reference_id: String,
    },
    Qwen {
        api_key: String,
        base_url: String,
        model: String,
        voice: String,
    },
    OpenAi {
        api_key: String,
        endpoint: String,
        model: String,
        voice: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VoiceSynthesisProfile {
    pub emotion: String,
    pub speed: f64,
    pub volume: Option<f64>,
    pub normalize_loudness: Option<bool>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
}

impl Default for VoiceSynthesisProfile {
    fn default() -> Self {
        Self {
            emotion: String::new(),
            speed: 1.0,
            volume: None,
            normalize_loudness: None,
            temperature: None,
            top_p: None,
        }
    }
}

impl VoiceSynthesisProfile {
    pub fn legacy(emotion: &str, speed: f64) -> Self {
        Self {
            emotion: emotion.to_string(),
            speed,
            ..Self::default()
        }
    }
}

impl VoiceClient {
    /// 从配置解析语音客户端；未启用或缺 key 时返回错误。
    pub fn from_config(cfg: &Config, output_dir: &Path) -> anyhow::Result<Self> {
        if !cfg.voice.enabled {
            anyhow::bail!("语音合成未启用（config.yaml 中 voice.enabled: true 后可启用）");
        }
        let provider = cfg.voice.provider.trim().to_string();
        let protocol = voice_protocol(cfg);
        let client = apply_system_proxy(
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(120)),
        )
        .build()?;
        let (kind, format) = match protocol.as_str() {
            "fish" => {
                let api_key = env_key(&cfg.voice.api_key_env, "FISH_AUDIO_API_KEY")?;
                let reference_id = std::env::var("FURINA_VOICE_REFERENCE_ID")
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| cfg.voice.reference_id.trim().to_string());
                let model = required_or(&cfg.voice.model, "s2.1-pro-free", "TTS 模型")?;
                let endpoint = required(&cfg.voice.endpoint, "TTS API 地址")?;
                let format = value_or(&cfg.voice.format, "mp3").to_lowercase();
                (VoiceKind::Fish { api_key, endpoint, model, reference_id }, format)
            }
            "qwen_omni" => {
                let legacy = cfg.voice.protocol.trim().is_empty();
                let api_key_env = if legacy { &cfg.qwen.api_key_env } else { &cfg.voice.api_key_env };
                let api_key = env_key(api_key_env, "QWEN_API_KEY")?;
                let endpoint = if legacy {
                    required(&cfg.qwen.base_url, "Qwen API 地址")?
                } else {
                    required_or(&cfg.voice.endpoint, &cfg.qwen.base_url, "Qwen API 地址")?
                };
                let model = if legacy {
                    required_or(&cfg.qwen.tts_model, "qwen3.5-omni-flash", "TTS 模型")?
                } else {
                    required_or(&cfg.voice.model, &cfg.qwen.tts_model, "TTS 模型")?
                };
                let voice = if legacy {
                    required_or(&cfg.qwen.tts_voice, "Tina", "TTS 音色")?
                } else {
                    required_or(&cfg.voice.voice, &cfg.qwen.tts_voice, "TTS 音色")?
                };
                (VoiceKind::Qwen { api_key, base_url: endpoint, model, voice }, "wav".into())
            }
            "openai" => {
                let api_key = env_key(&cfg.voice.api_key_env, "FURINA_TTS_API_KEY")?;
                let endpoint = required(&cfg.voice.endpoint, "TTS API 地址")?;
                let model = required(&cfg.voice.model, "TTS 模型")?;
                let voice = required(&cfg.voice.voice, "TTS 音色")?;
                let format = value_or(&cfg.voice.format, "mp3").to_lowercase();
                (VoiceKind::OpenAi { api_key, endpoint, model, voice }, format)
            }
            other => anyhow::bail!("未知 TTS protocol: {other}（支持 fish / qwen_omni / openai）"),
        };
        Ok(Self {
            client,
            kind,
            provider: if provider.is_empty() { protocol } else { provider },
            format,
            max_text_len: cfg.voice.max_text_len.max(10),
            output_dir: output_dir.to_path_buf(),
        })
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// 输出格式（mp3 / wav），用于文件名与前端播放类型。
    pub fn format(&self) -> &str {
        &self.format
    }

    /// 声音模型 ID（fish reference_id 或 qwen 音色名，用于展示）。
    pub fn reference_id(&self) -> &str {
        match &self.kind {
            VoiceKind::Fish { reference_id, .. } => reference_id,
            VoiceKind::Qwen { voice, .. } => voice,
            VoiceKind::OpenAi { voice, .. } => voice,
        }
    }

    pub fn model(&self) -> &str {
        match &self.kind {
            VoiceKind::Fish { model, .. } => model,
            VoiceKind::Qwen { model, .. } => model,
            VoiceKind::OpenAi { model, .. } => model,
        }
    }

    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }

    /// 规范化情绪标记（S2 模型方括号语法）：`happy` → `[happy]`，`[sad]` → `[sad]`。
    /// 兼容旧的圆括号写法（自动转方括号）；空串保持空。
    pub fn emotion_tag(emotion: &str) -> String {
        let t = emotion.trim();
        if t.is_empty() {
            return String::new();
        }
        let t = t
            .trim_start_matches(['[', '('])
            .trim_end_matches([']', ')']);
        if t.is_empty() {
            return String::new();
        }
        format!("[{t}]")
    }
    /// 合成语音并写入输出目录，返回音频文件路径。
    /// `speed`：朗读语速（0.5–2.0，1.0 为正常）。
    pub async fn synthesize(&self, text: &str, emotion: &str, speed: f64) -> anyhow::Result<PathBuf> {
        let profile = VoiceSynthesisProfile::legacy(emotion, speed);
        let bytes = self.request_audio(text, &profile).await?;
        std::fs::create_dir_all(&self.output_dir)?;
        let ext = match self.format.as_str() {
            "wav" => "wav",
            "flac" => "flac",
            _ => "mp3",
        };
        let path = self
            .output_dir
            .join(format!("furina_voice_{}.{ext}", unix_ms()));
        std::fs::write(&path, &bytes)?;
        Ok(path)
    }

    /// 合成语音并返回音频字节（不落盘，供桌面版前端直接播放）。
    pub async fn synthesize_bytes(
        &self,
        text: &str,
        emotion: &str,
        speed: f64,
    ) -> anyhow::Result<Vec<u8>> {
        let profile = VoiceSynthesisProfile::legacy(emotion, speed);
        self.request_audio(text, &profile).await
    }

    pub async fn synthesize_bytes_with_profile(
        &self,
        text: &str,
        profile: &VoiceSynthesisProfile,
    ) -> anyhow::Result<Vec<u8>> {
        self.request_audio(text, profile).await
    }

    /// 共享请求逻辑：文本清理 → 情绪标记 → 按 provider 请求 → 音频字节。
    async fn request_audio(
        &self,
        text: &str,
        profile: &VoiceSynthesisProfile,
    ) -> anyhow::Result<Vec<u8>> {
        let cleaned = clean_for_tts(text, self.max_text_len);
        if cleaned.is_empty() {
            anyhow::bail!("没有可合成的文本");
        }
        match &self.kind {
            VoiceKind::Fish { api_key, endpoint, model, reference_id } => {
                // fish S2 系列支持方括号情绪标记（如 [happy]）；qwen omni 不支持，
                // 加了会被当成正文朗读出来。
                let tag = Self::emotion_tag(&profile.emotion);
                let payload = if tag.is_empty() {
                    cleaned
                } else {
                    format!("{tag}{cleaned}")
                };
                self.request_fish(&payload, profile, api_key, endpoint, model, reference_id)
                    .await
            }
            VoiceKind::Qwen { api_key, base_url, model, voice } => {
                self.request_qwen(&cleaned, api_key, base_url, model, voice).await
            }
            VoiceKind::OpenAi { api_key, endpoint, model, voice } => {
                self.request_openai(&cleaned, profile.speed, api_key, endpoint, model, voice).await
            }
        }
    }

    /// fish.audio `/v1/tts`：JSON 请求，直接返回音频字节。
    async fn request_fish(
        &self,
        payload: &str,
        profile: &VoiceSynthesisProfile,
        api_key: &str,
        endpoint: &str,
        model: &str,
        reference_id: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let mut prosody = json!({
            "speed": profile.speed.clamp(0.5, 2.0),
        });
        if let Some(volume) = profile.volume {
            prosody["volume"] = json!(volume.clamp(-6.0, 6.0));
        }
        if let Some(normalize_loudness) = profile.normalize_loudness {
            prosody["normalize_loudness"] = json!(normalize_loudness);
        }
        let mut body = json!({
            "text": payload,
            "format": self.format,
            "prosody": prosody,
        });
        if let Some(temperature) = profile.temperature {
            body["temperature"] = json!(temperature.clamp(0.0, 1.0));
        }
        if let Some(top_p) = profile.top_p {
            body["top_p"] = json!(top_p.clamp(0.0, 1.0));
        }
        if !reference_id.is_empty() {
            body["reference_id"] = json!(reference_id);
        }
        let resp = self
            .client
            .post(endpoint)
            .bearer_auth(api_key)
            .header("model", model)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if status.as_u16() == 402 {
                anyhow::bail!(
                    "fish.audio 额度不足（402）：音色包 reference_id 按量计费，需充值后可用；\
                     也可在 config.yaml 把 voice.provider 改为 qwen 使用免费固定音色"
                );
            }
            anyhow::bail!("语音 API 错误 {status}: {}", short(&text));
        }
        let bytes = resp.bytes().await?;
        if bytes.is_empty() {
            anyhow::bail!("语音 API 返回空音频");
        }
        Ok(bytes.to_vec())
    }

    /// OpenAI-compatible Audio Speech：JSON 请求，直接返回音频字节。
    async fn request_openai(
        &self,
        payload: &str,
        speed: f64,
        api_key: &str,
        endpoint: &str,
        model: &str,
        voice: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let body = json!({
            "model": model,
            "input": payload,
            "voice": voice,
            "response_format": self.format,
            "speed": speed.clamp(0.25, 4.0),
        });
        let resp = self.client.post(endpoint).bearer_auth(api_key).json(&body).send().await?;
        let status = resp.status();
        let content_type = resp.headers().get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()).unwrap_or("").to_ascii_lowercase();
        if !status.is_success() || content_type.contains("application/json") {
            let text = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                anyhow::bail!("TTS API 错误 {status}: {}", short(&text));
            }
            anyhow::bail!("TTS API 返回了 JSON 而不是音频: {}", short(&text));
        }
        let bytes = resp.bytes().await?;
        if bytes.is_empty() { anyhow::bail!("TTS API 返回空音频"); }
        Ok(bytes.to_vec())
    }

    /// Qwen-Omni TTS：SSE 流式，`delta.audio.data` 为 base64 音频分段。
    async fn request_qwen(
        &self,
        payload: &str,
        api_key: &str,
        base_url: &str,
        model: &str,
        voice: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let body = json!({
            "model": model,
            "messages": [{
                "role": "user",
                "content": [{ "type": "text", "text": payload }],
            }],
            "modalities": ["text", "audio"],
            "audio": { "voice": voice, "format": "wav" },
            "stream": true,
        });
        let resp = self
            .client
            .post(format!("{base_url}/chat/completions"))
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("Qwen 语音合成错误 {status}: {}", short(&text));
        }
        let mut b64 = String::new();
        for line in text.lines() {
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else { continue };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(data)
                .map_err(|e| anyhow::anyhow!("Qwen 语音流数据无效: {e}"))?;
            if let Some(err) = v.get("error") {
                anyhow::bail!("Qwen 语音合成流错误: {err}");
            }
            if let Some(part) = v["choices"][0]["delta"]["audio"]["data"].as_str() {
                b64.push_str(part);
            }
        }
        if b64.is_empty() {
            anyhow::bail!("Qwen 语音合成未返回音频数据");
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| anyhow::anyhow!("Qwen 音频 base64 解码失败: {e}"))?;
        if bytes.is_empty() {
            anyhow::bail!("Qwen 语音合成返回空音频");
        }
        // DashScope omni 的 `format: wav/mp3` 实际都返回 24kHz 16-bit 单声道裸 PCM，
        // 这里统一包上 RIFF/WAVE 头，保证前端可直接播放。
        Ok(pcm_to_wav(&bytes, 24000))
    }
}

/// 把 16-bit 单声道裸 PCM 包装成标准 WAV（RIFF/WAVE）字节。
fn pcm_to_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

fn voice_protocol(cfg: &Config) -> String {
    let explicit = cfg.voice.protocol.trim().to_lowercase();
    if !explicit.is_empty() {
        return match explicit.as_str() {
            "fish_tts" => "fish".into(),
            "qwen" | "openai_chat_audio" => "qwen_omni".into(),
            "openai_speech" => "openai".into(),
            _ => explicit,
        };
    }
    match cfg.voice.provider.trim().to_lowercase().as_str() {
        "fish" => "fish".into(),
        "qwen" => "qwen_omni".into(),
        _ => "openai".into(),
    }
}

fn required(value: &str, label: &str) -> anyhow::Result<String> {
    let value = value.trim();
    if value.is_empty() { anyhow::bail!("{label}不能为空"); }
    Ok(value.trim_end_matches('/').to_string())
}

fn required_or(value: &str, fallback: &str, label: &str) -> anyhow::Result<String> {
    let selected = if value.trim().is_empty() { fallback } else { value };
    required(selected, label)
}

fn value_or(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() { fallback.into() } else { value.into() }
}

fn env_key(var: &str, fallback: &str) -> anyhow::Result<String> {
    let name = {
        let v = var.trim();
        if v.is_empty() { fallback } else { v }
    };
    let key = std::env::var(name).map_err(|_| {
        anyhow::anyhow!("缺少环境变量 {name}（请在 .furina/secrets.env 添加 {name}=xxx）")
    })?;
    if key.trim().is_empty() {
        anyhow::bail!("语音 API key 为空（{name}）");
    }
    Ok(key)
}

/// 把回复文本整理成适合朗读的形式：跳过代码块、去掉 markdown、旁白和表情、
/// 合并多余空白、按配置上限截断。
fn clean_for_tts(text: &str, max_len: usize) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    let link_re = Regex::new(r"\[([^\]\n]+)\]\((?:[^()\n]|\([^()\n]*\))*\)").expect("valid markdown link regex");
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || line.is_empty() {
            continue;
        }
        let line = link_re.replace_all(line, "$1");
        let line = line
            .trim_start_matches(['#', '-', '>', '|'])
            .trim();
        let line = line.strip_prefix("* ").unwrap_or(line).trim();
        if line.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&line);
    }
    let filtered = strip_tts_asides(&out);
    let filtered = strip_tts_emphasis_and_emoji(&filtered);
    filtered.chars().take(max_len).collect::<String>().trim().to_string()
}

fn bracket_pair(opening: char) -> Option<char> {
    match opening {
        '（' => Some('）'),
        '(' => Some(')'),
        '【' => Some('】'),
        '[' => Some(']'),
        '「' => Some('」'),
        '『' => Some('』'),
        _ => None,
    }
}

fn find_matching_bracket(chars: &[char], start: usize) -> Option<usize> {
    let mut stack = vec![chars[start]];
    for (position, character) in chars.iter().enumerate().skip(start + 1) {
        if bracket_pair(*character).is_some() {
            stack.push(*character);
            continue;
        }
        let closes = matches!(
            (*character, stack.last().copied()),
            ('）', Some('（'))
                | (')', Some('('))
                | ('】', Some('【'))
                | (']', Some('['))
                | ('」', Some('「'))
                | ('』', Some('『'))
        );
        if closes {
            stack.pop();
            if stack.is_empty() {
                return Some(position);
            }
        }
    }
    None
}

fn is_tts_emoji(character: char) -> bool {
    let code = character as u32;
    matches!(
        code,
        0x1F000..=0x1FAFF
            | 0x2300..=0x23FF
            | 0x2600..=0x27BF
            | 0xFE0E..=0xFE0F
            | 0x200D
    )
}

fn is_tts_aside_content(content: &str) -> bool {
    let normalized: String = content
        .chars()
        .filter(|character| !is_tts_emoji(*character) && !character.is_whitespace())
        .filter(|character| !character.is_ascii_punctuation())
        .collect();
    if normalized.is_empty() {
        return true;
    }
    let lowered = content.trim().to_lowercase();
    if ["动作", "旁白", "内心", "心想", "心理活动", "画外音", "os"]
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
    {
        return true;
    }
    let cues = [
        "微笑", "轻笑", "笑了", "笑着", "叹气", "点头", "摇头", "眨眼", "挑眉",
        "挥手", "看向", "靠近", "转身", "沉默", "停顿", "愣住", "脸红", "耳朵发红",
        "紧张", "困惑", "疑惑", "惊讶", "吸气", "呼气", "哭声", "笑声", "脚步声",
        "风声", "音乐声",
    ];
    cues.iter().any(|cue| content.contains(cue)) && content.chars().count() <= 40
}

fn strip_tts_asides(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut output = String::new();
    let mut position = 0;
    while position < chars.len() {
        if bracket_pair(chars[position]).is_some() {
            if let Some(closing) = find_matching_bracket(&chars, position) {
                let content: String = chars[position + 1..closing].iter().collect();
                if !is_tts_aside_content(&content) {
                    output.extend(content.chars());
                }
                position = closing + 1;
                continue;
            }
        }
        output.push(chars[position]);
        position += 1;
    }
    output
}

fn strip_tts_emphasis_and_emoji(text: &str) -> String {
    let emphasis_re = Regex::new(r"\*([^*\n]{1,80})\*").expect("valid emphasis regex");
    let without_emphasis = emphasis_re.replace_all(text, |captures: &regex::Captures<'_>| {
        let content = captures.get(1).map(|match_| match_.as_str()).unwrap_or_default();
        if is_tts_aside_content(content) {
            String::new()
        } else {
            content.to_string()
        }
    });
    let filtered: String = without_emphasis
        .chars()
        .filter(|character| !is_tts_emoji(*character))
        .collect();
    filtered
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn short(text: &str) -> String {
    let t: String = text.chars().take(300).collect();
    if text.chars().count() > 300 {
        format!("{t}…")
    } else {
        t
    }
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn spawn_http_mock(
        status: u16,
        body: Vec<u8>,
        captured: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tx.send(addr.to_string()).unwrap();
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 64 * 1024];
                let n = sock.read(&mut buf).await.unwrap();
                if let Ok(s) = std::str::from_utf8(&buf[..n]) {
                    *captured.lock().unwrap() = s.to_string();
                }
                let reason = if status == 200 { "OK" } else { "Error" };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/octet-stream\r\ncontent-length: {}\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.write_all(&body).await;
            });
        });
        rx.recv().unwrap()
    }

    fn fish_config(base_url: String, key_env: &str) -> Config {
        let mut cfg = Config::default();
        cfg.voice.enabled = true;
        cfg.voice.provider = "fish".into();
        cfg.voice.endpoint = format!("{base_url}/v1/tts");
        cfg.voice.reference_id = "voice-model-001".into();
        cfg.voice.api_key_env = key_env.into();
        cfg
    }

    fn qwen_config(base_url: String, key_env: &str) -> Config {
        let mut cfg = Config::default();
        cfg.voice.enabled = true;
        cfg.voice.provider = "qwen".into();
        cfg.qwen.base_url = base_url;
        cfg.qwen.api_key_env = key_env.into();
        cfg.qwen.tts_model = "qwen3.5-omni-flash".into();
        cfg.qwen.tts_voice = "Tina".into();
        cfg
    }

    #[test]
    fn emotion_tag_normalizes() {
        assert_eq!(VoiceClient::emotion_tag(""), "");
        assert_eq!(VoiceClient::emotion_tag("  "), "");
        assert_eq!(VoiceClient::emotion_tag("happy"), "[happy]");
        assert_eq!(VoiceClient::emotion_tag("[sad]"), "[sad]");
        assert_eq!(VoiceClient::emotion_tag(" (angry) "), "[angry]");
        assert_eq!(VoiceClient::emotion_tag("()"), "");
        assert_eq!(VoiceClient::emotion_tag("[]"), "");
    }

    #[test]
    fn from_config_gates() {
        let mut cfg = Config::default();
        let dir = std::env::temp_dir();
        cfg.voice.api_key_env = "FURINA_TEST_VOICE_KEY_ABSENT".into();
        assert!(VoiceClient::from_config(&cfg, &dir).is_err(), "未启用应报错");
        cfg.voice.enabled = true;
        assert!(VoiceClient::from_config(&cfg, &dir).is_err(), "缺 key 应报错");
        unsafe {
            std::env::set_var("FURINA_TEST_VOICE_KEY_ABSENT", "k");
        }
        let client = VoiceClient::from_config(&cfg, &dir).unwrap();
        assert_eq!(client.model(), "s2.1-pro-free");
        assert_eq!(client.provider(), "fish");
        unsafe {
            std::env::remove_var("FURINA_TEST_VOICE_KEY_ABSENT");
        }
    }

    #[test]
    fn qwen_provider_resolves_config() {
        let mut cfg = Config::default();
        cfg.voice.enabled = true;
        cfg.voice.provider = "qwen".into();
        cfg.qwen.api_key_env = "FURINA_TEST_QWEN_VOICE_KEY".into();
        cfg.qwen.tts_voice = "Seraphina".into();
        unsafe {
            std::env::set_var("FURINA_TEST_QWEN_VOICE_KEY", "qk");
        }
        let client = VoiceClient::from_config(&cfg, &std::env::temp_dir()).unwrap();
        assert_eq!(client.provider(), "qwen");
        assert_eq!(client.model(), "qwen3.5-omni-flash");
        assert_eq!(client.reference_id(), "Seraphina");
        assert_eq!(client.format(), "wav");
        unsafe {
            std::env::remove_var("FURINA_TEST_QWEN_VOICE_KEY");
        }
    }

    #[test]
    fn unknown_protocol_rejected() {
        let mut cfg = Config::default();
        cfg.voice.enabled = true;
        cfg.voice.provider = "Custom Service".into();
        cfg.voice.protocol = "nope".into();
        let err = VoiceClient::from_config(&cfg, &std::env::temp_dir())
            .unwrap_err()
            .to_string();
        assert!(err.contains("未知 TTS protocol"), "{err}");
    }

    #[tokio::test]
    async fn openai_speech_uses_custom_endpoint_model_and_voice() {
        let audio = b"ID3 openai audio".to_vec();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let addr = spawn_http_mock(200, audio.clone(), captured.clone());
        let mut cfg = Config::default();
        cfg.voice.enabled = true;
        cfg.voice.provider = "My TTS".into();
        cfg.voice.protocol = "openai".into();
        cfg.voice.endpoint = format!("http://{addr}/v1/audio/speech");
        cfg.voice.model = "custom-tts-model".into();
        cfg.voice.voice = "furina-voice".into();
        cfg.voice.format = "mp3".into();
        cfg.voice.api_key_env = "FURINA_TEST_OPENAI_TTS_KEY".into();
        unsafe { std::env::set_var("FURINA_TEST_OPENAI_TTS_KEY", "openai-key"); }
        let client = VoiceClient::from_config(&cfg, &std::env::temp_dir()).unwrap();
        assert_eq!(client.provider(), "My TTS");
        let bytes = client.synthesize_bytes("你好", "happy", 1.25).await.unwrap();
        assert_eq!(bytes, audio);
        let request = captured.lock().unwrap().clone();
        assert!(request.contains("/v1/audio/speech"), "{request}");
        assert!(request.contains("custom-tts-model"), "{request}");
        assert!(request.contains("furina-voice"), "{request}");
        assert!(request.contains("response_format"), "{request}");
        assert!(request.contains("\"input\":\"你好\""), "{request}");
        assert!(!request.contains("[happy]"), "通用协议不应注入 Fish 情绪标签");
        assert!(request.to_lowercase().contains("bearer openai-key"), "{request}");
        unsafe { std::env::remove_var("FURINA_TEST_OPENAI_TTS_KEY"); }
    }

    #[test]
    fn clean_for_tts_skips_code_fences_and_markdown() {
        let text = "哼，易如反掌。\n\n```rust\nlet x = 1;\n```\n\n- 第一点\n- 第二点";
        let cleaned = clean_for_tts(text, 1000);
        assert!(!cleaned.contains("```"));
        assert!(!cleaned.contains("let x"));
        assert!(cleaned.contains("哼，易如反掌"));
        assert!(cleaned.contains("第一点"));
        assert!(cleaned.contains("第二点"));
    }

    #[test]
    fn clean_for_tts_removes_narration_and_emoji_but_keeps_explanations() {
        assert_eq!(clean_for_tts("（微笑）你好 😊", 1000), "你好");
        assert_eq!(clean_for_tts("我会（轻轻叹气）。", 1000), "我会。");
        assert_eq!(clean_for_tts("版本（推荐）和函数（x）", 1000), "版本推荐和函数x");
        assert_eq!(clean_for_tts("[疑惑]你确定吗？", 1000), "你确定吗？");
        assert_eq!(clean_for_tts("*挑眉*当然可以。", 1000), "当然可以。");
    }
    #[test]
    fn clean_for_tts_truncates() {
        let text = "abcdefghij";
        assert_eq!(clean_for_tts(text, 5), "abcde");
        assert_eq!(clean_for_tts("", 5), "");
    }

    #[tokio::test]
    async fn synthesize_writes_audio_file() {
        let body = b"ID3 mock audio bytes".to_vec();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let addr = spawn_http_mock(200, body.clone(), captured.clone());
        let cfg = fish_config(format!("http://{addr}"), "FURINA_TEST_VOICE_WRITE_KEY");
        unsafe {
            std::env::set_var("FURINA_TEST_VOICE_WRITE_KEY", "test-key");
        }
        let dir = std::env::temp_dir().join(format!("furina_voice_test_{}", unix_ms()));
        let client = VoiceClient::from_config(&cfg, &dir).unwrap();
        let path = client.synthesize("你好呀", "happy", 1.1).await.unwrap();
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert!(path.extension().unwrap() == "mp3");
        let req = captured.lock().unwrap().clone();
        assert!(req.contains("reference_id"));
        assert!(req.contains("voice-model-001"));
        assert!(req.contains("[happy]"));
        assert!(req.contains("prosody") && req.contains("1.1"), "应带语速参数: {req}");
        assert!(req.to_lowercase().contains("model: s2.1-pro-free"), "model 应作为请求头");
        assert!(req.to_lowercase().contains("bearer test-key"), "应带 Bearer 鉴权");
        let _ = std::fs::remove_dir_all(&dir);
        unsafe {
            std::env::remove_var("FURINA_TEST_VOICE_WRITE_KEY");
        }
    }

    #[tokio::test]
    async fn fish_profile_sends_supported_expression_parameters() {
        let body = b"ID3 expressive audio".to_vec();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let addr = spawn_http_mock(200, body.clone(), captured.clone());
        let cfg = fish_config(format!("http://{addr}"), "FURINA_TEST_FISH_PROFILE_KEY");
        unsafe { std::env::set_var("FURINA_TEST_FISH_PROFILE_KEY", "test-key"); }
        let client = VoiceClient::from_config(&cfg, &std::env::temp_dir()).unwrap();
        let profile = VoiceSynthesisProfile {
            emotion: "[extremely angry]".into(),
            speed: 1.12,
            volume: Some(5.0),
            normalize_loudness: Some(false),
            temperature: Some(0.85),
            top_p: Some(0.9),
        };
        let bytes = client
            .synthesize_bytes_with_profile("别再这样了", &profile)
            .await
            .unwrap();
        assert_eq!(bytes, body);
        let request = captured.lock().unwrap().clone();
        assert!(request.contains("[extremely angry]"), "{request}");
        assert!(request.contains("\"speed\":1.12"), "{request}");
        assert!(request.contains("\"volume\":5.0") || request.contains("\"volume\":5"), "{request}");
        assert!(request.contains("\"normalize_loudness\":false"), "{request}");
        assert!(request.contains("\"temperature\":0.85"), "{request}");
        assert!(request.contains("\"top_p\":0.9"), "{request}");
        assert!(!request.contains("pitch"), "Fish 请求不应虚构 pitch 参数: {request}");
        unsafe { std::env::remove_var("FURINA_TEST_FISH_PROFILE_KEY"); }
    }

    #[tokio::test]
    async fn synthesize_bytes_returns_audio_without_file() {
        let body = b"ID3 bytes only".to_vec();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let addr = spawn_http_mock(200, body.clone(), captured.clone());
        let cfg = fish_config(format!("http://{addr}"), "FURINA_TEST_VOICE_BYTES_KEY");
        unsafe {
            std::env::set_var("FURINA_TEST_VOICE_BYTES_KEY", "test-key");
        }
        let dir = std::env::temp_dir().join(format!("furina_voice_bytes_{}", unix_ms()));
        let client = VoiceClient::from_config(&cfg, &dir).unwrap();
        let bytes = client.synthesize_bytes("你好", "", 1.0).await.unwrap();
        assert_eq!(bytes, body, "synthesize_bytes 应返回音频字节");
        assert!(!dir.exists(), "不落盘：输出目录不应被创建");
        unsafe {
            std::env::remove_var("FURINA_TEST_VOICE_BYTES_KEY");
        }
    }

    #[tokio::test]
    async fn synthesize_surfaces_api_error() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let addr = spawn_http_mock(402, b"{\"error\":\"Insufficient API credit\"}".to_vec(), captured);
        let cfg = fish_config(format!("http://{addr}"), "FURINA_TEST_VOICE_ERROR_KEY");
        unsafe {
            std::env::set_var("FURINA_TEST_VOICE_ERROR_KEY", "test-key");
        }
        let dir = std::env::temp_dir().join(format!("furina_voice_err_{}", unix_ms()));
        let client = VoiceClient::from_config(&cfg, &dir).unwrap();
        let err = client.synthesize("测试", "", 1.0).await.unwrap_err().to_string();
        assert!(err.contains("402"), "{err}");
        unsafe {
            std::env::remove_var("FURINA_TEST_VOICE_ERROR_KEY");
        }
    }

    #[tokio::test]
    async fn qwen_synthesize_parses_sse_audio() {
        // 两个 SSE 分片，各含一段 base64；拼接后应还原原始音频字节。
        let half1 = base64::engine::general_purpose::STANDARD.encode(b"RIFF....WAVE");
        let half2 = base64::engine::general_purpose::STANDARD.encode(b"data-bytes");
        let sse = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"audio\":{{\"data\":\"{half1}\"}}}}}}]}}\n\n\
             data: {{\"choices\":[{{\"delta\":{{\"audio\":{{\"data\":\"{half2}\"}}}}}}]}}\n\n\
             data: [DONE]\n\n"
        );
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let addr = spawn_http_mock(200, sse.into_bytes(), captured.clone());
        let cfg = qwen_config(format!("http://{addr}"), "FURINA_TEST_QWEN_VOICE_KEY");
        unsafe {
            std::env::set_var("FURINA_TEST_QWEN_VOICE_KEY", "qwen-key");
        }
        let dir = std::env::temp_dir().join(format!("furina_qwen_voice_{}", unix_ms()));
        let client = VoiceClient::from_config(&cfg, &dir).unwrap();
        let bytes = client.synthesize_bytes("你好呀", "", 1.0).await.unwrap();
        assert_eq!(&bytes[..4], b"RIFF", "应包装成 wav");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[44..], b"RIFF....WAVEdata-bytes", "原始音频应跟在 44 字节头之后");
        let req = captured.lock().unwrap().clone();
        assert!(req.contains("qwen3.5-omni-flash"), "应带模型名");
        assert!(req.contains("\"voice\":\"Tina\""), "应带固定音色");
        assert!(req.contains("\"stream\":true"), "qwen 音频必须流式");
        assert!(req.contains("\"modalities\":[\"text\",\"audio\"]"), "应声明音频输出");
        assert!(req.to_lowercase().contains("bearer qwen-key"), "应带 Bearer 鉴权");
        assert!(!dir.exists(), "不落盘");
        unsafe {
            std::env::remove_var("FURINA_TEST_QWEN_VOICE_KEY");
        }
    }

    #[test]
    fn synthesize_empty_text_fails() {
        let dir = std::env::temp_dir();
        let mut cfg = Config::default();
        cfg.voice.enabled = true;
        cfg.voice.reference_id = "m".into();
        cfg.voice.api_key_env = "FURINA_TEST_VOICE_EMPTY_KEY".into();
        unsafe {
            std::env::set_var("FURINA_TEST_VOICE_EMPTY_KEY", "k");
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = VoiceClient::from_config(&cfg, &dir).unwrap();
        let err = rt.block_on(client.synthesize("```\n```", "", 1.0)).unwrap_err().to_string();
        assert!(err.contains("没有可合成"));
        unsafe {
            std::env::remove_var("FURINA_TEST_VOICE_EMPTY_KEY");
        }
    }

    #[test]
    fn pcm_to_wav_builds_valid_header() {
        let pcm = vec![0u8; 100];
        let wav = pcm_to_wav(&pcm, 24000);
        assert_eq!(wav.len(), 44 + 100);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]), 24000);
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1, "单声道");
        assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16, "16-bit");
        assert_eq!(&wav[44..], pcm.as_slice());
    }

    /// 手动 E2E（需要真实 key，不进 CI）：
    ///   cargo test -p furina-core qwen_tts_e2e_manual -- --ignored --nocapture
    /// 环境变量 QWEN_API_KEY 需已设置（或由 furina 从 secrets.env 加载）。
    #[tokio::test]
    #[ignore]
    async fn qwen_tts_e2e_manual() {
        let key = std::env::var("QWEN_API_KEY").unwrap_or_default();
        if key.is_empty() {
            eprintln!("跳过：需要 QWEN_API_KEY");
            return;
        }
        let mut cfg = Config::default();
        cfg.voice.enabled = true;
        cfg.voice.provider = "qwen".into();
        let client = VoiceClient::from_config(&cfg, &std::env::temp_dir()).unwrap();
        let bytes = client
            .synthesize_bytes("你好呀，我是芙宁娜，今天也很开心见到你。", "happy", 1.0)
            .await
            .unwrap();
        println!("Qwen TTS 字节数: {}，provider={}", bytes.len(), client.provider());
        assert!(bytes.len() > 1000, "应返回可播放的 wav 音频");
        assert_eq!(&bytes[..4], b"RIFF", "应为 RIFF wav");
        assert_eq!(&bytes[8..12], b"WAVE", "应为 WAVE 容器");
    }

    /// 手动 E2E：fish.audio + 芙芙音色包（需 FISH_AUDIO_API_KEY 与 FURINA_VOICE_REFERENCE_ID）。
    ///   cargo test -p furina-core fish_tts_e2e_manual -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn fish_tts_e2e_manual() {
        let key = std::env::var("FISH_AUDIO_API_KEY").unwrap_or_default();
        let rid = std::env::var("FURINA_VOICE_REFERENCE_ID").unwrap_or_default();
        if key.is_empty() || rid.is_empty() {
            eprintln!("跳过：需要 FISH_AUDIO_API_KEY 与 FURINA_VOICE_REFERENCE_ID");
            return;
        }
        let mut cfg = Config::default();
        cfg.voice.enabled = true;
        cfg.voice.provider = "fish".into();
        cfg.voice.model = "s2.1-pro-free".into();
        let client = VoiceClient::from_config(&cfg, &std::env::temp_dir()).unwrap();
        let bytes = client
            .synthesize_bytes("哼，本神正忙着呢，舞台上的每一分钟都值得认真对待。", "proud", 1.0)
            .await
            .unwrap();
        println!(
            "fish TTS 字节数: {}，provider={}，音色={}",
            bytes.len(),
            client.provider(),
            client.reference_id()
        );
        assert!(bytes.len() > 1000, "应返回可播放的音频");
    }
}
