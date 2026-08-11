//! 语音识别（ASR）：fish.audio `/v1/asr` 或 Qwen-Omni（DashScope，免费额度）→ 文本。
//!
//! 供桌面版 PTT 语音输入使用：前端录音 → 字节上传 → 这里转文字 → 进入对话。
//!
//! provider 选择（config.yaml `asr.provider`）：
//! - `qwen`（默认，免费）：POST DashScope OpenAI 兼容 `chat/completions`，
//!   消息含 `input_audio` base64（仅支持 wav，16kHz 单声道 PCM）。
//! - `fish`：fish.audio `/v1/asr` multipart（按量计费）。

use crate::config::Config;
use crate::proxy::apply_system_proxy;
use base64::Engine as _;
use reqwest::multipart::{Form, Part};
use serde_json::json;
use std::time::Duration;

/// 语音识别客户端（按 `asr.provider` 选择后端）。
#[derive(Clone, Debug)]
pub struct AsrClient {
    client: reqwest::Client,
    kind: AsrKind,
    language: String,
}

#[derive(Clone, Debug)]
enum AsrKind {
    Fish { api_key: String, endpoint: String },
    Qwen { api_key: String, base_url: String, model: String, prompt: String },
}

impl AsrClient {
    /// 从配置解析 ASR 客户端；未启用或缺 key 时返回错误。
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        if !cfg.asr.enabled {
            anyhow::bail!("语音识别未启用（config.yaml 中 asr.enabled: true 后可启用）");
        }
        let provider = cfg.asr.provider.trim().to_lowercase();
        let language = {
            let l = cfg.asr.language.trim().to_string();
            if l.is_empty() { "zh".into() } else { l }
        };
        let client = apply_system_proxy(
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(90)),
        )
        .build()?;
        let kind = match provider.as_str() {
            "qwen" => {
                let api_key = env_key(&cfg.qwen.api_key_env, "QWEN_API_KEY")?;
                let model = {
                    let m = cfg.qwen.asr_model.trim().to_string();
                    if m.is_empty() { "qwen3.5-omni-flash".into() } else { m }
                };
                AsrKind::Qwen {
                    api_key,
                    base_url: cfg.qwen.base_url.trim_end_matches('/').to_string(),
                    model,
                    prompt: cfg.qwen.asr_prompt.trim().to_string(),
                }
            }
            "fish" => {
                let api_key = env_key(&cfg.asr.api_key_env, "FISH_AUDIO_API_KEY")?;
                AsrKind::Fish {
                    api_key,
                    endpoint: cfg.asr.endpoint.trim_end_matches('/').to_string(),
                }
            }
            other => anyhow::bail!("未知 ASR provider: {other}（支持 qwen / fish）"),
        };
        Ok(Self { client, kind, language })
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    /// 当前识别后端名（qwen / fish），用于展示与诊断。
    pub fn provider(&self) -> &str {
        match self.kind {
            AsrKind::Fish { .. } => "fish",
            AsrKind::Qwen { .. } => "qwen",
        }
    }

    /// 识别一段音频为文本。`mime` 用于 fish 后端文件名后缀；
    /// qwen 后端仅接受 wav（16kHz 单声道 PCM），其他格式返回明确错误。
    pub async fn transcribe(&self, audio: Vec<u8>, mime: &str) -> anyhow::Result<String> {
        if audio.is_empty() {
            anyhow::bail!("没有可识别的音频");
        }
        match &self.kind {
            AsrKind::Fish { api_key, endpoint } => {
                self.transcribe_fish(audio, mime, api_key, endpoint).await
            }
            AsrKind::Qwen { api_key, base_url, model, prompt } => {
                self.transcribe_qwen(audio, mime, api_key, base_url, model, prompt)
                    .await
            }
        }
    }

    /// fish.audio `/v1/asr`（multipart，按量计费）。
    async fn transcribe_fish(
        &self,
        audio: Vec<u8>,
        mime: &str,
        api_key: &str,
        endpoint: &str,
    ) -> anyhow::Result<String> {
        let ext = match mime {
            "audio/wav" | "audio/x-wav" => "wav",
            "audio/mp4" | "audio/m4a" => "m4a",
            "audio/ogg" => "ogg",
            _ => "webm",
        };
        let file = Part::bytes(audio)
            .file_name(format!("furina_input.{ext}"))
            .mime_str(mime)
            .map_err(|e| anyhow::anyhow!("音频 MIME 无效: {e}"))?;
        let form = Form::new()
            .part("audio", file)
            .text("language", self.language.clone());
        let resp = self
            .client
            .post(endpoint)
            .bearer_auth(api_key)
            .multipart(form)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("语音识别 API 错误 {status}: {}", short(&text));
        }
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|_| anyhow::anyhow!("语音识别响应不是有效 JSON"))?;
        let out = v["text"]
            .as_str()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if out.is_empty() {
            anyhow::bail!("语音识别返回空文本");
        }
        Ok(out)
    }

    /// Qwen-Omni（DashScope OpenAI 兼容接口）：`input_audio` base64 → 转写文本。
    async fn transcribe_qwen(
        &self,
        audio: Vec<u8>,
        mime: &str,
        api_key: &str,
        base_url: &str,
        model: &str,
        prompt: &str,
    ) -> anyhow::Result<String> {
        if !mime.eq_ignore_ascii_case("audio/wav") && !mime.eq_ignore_ascii_case("audio/x-wav") {
            anyhow::bail!(
                "Qwen 语音识别仅支持 wav 音频（16kHz 单声道 PCM），收到 {mime}；\
                 请先用浏览器/工具把录音转成 wav 再发送"
            );
        }
        // DashScope qwen-omni 系列要求：type 为 input_audio，data 带 `data:;base64,` 前缀；
        // 必须声明 modalities: ["text"] 强制文本输出（参考 vibetalking 生产实现）。
        let data = format!(
            "data:;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(audio)
        );
        let mut content = json!([{
            "type": "input_audio",
            "input_audio": { "data": data, "format": "wav" },
        }]);
        let p = prompt.trim();
        if !p.is_empty() {
            content
                .as_array_mut()
                .expect("content is array")
                .push(json!({ "type": "text", "text": p }));
        }
        let body = json!({
            "model": model,
            "messages": [{ "role": "user", "content": content }],
            "max_tokens": 300,
            "modalities": ["text"],
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
            anyhow::bail!("Qwen 语音识别错误 {status}: {}", short(&text));
        }
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|_| anyhow::anyhow!("Qwen 语音识别响应不是有效 JSON"))?;
        let out = extract_text_content(&v["choices"][0]["message"]["content"]);
        if out.trim().is_empty() {
            anyhow::bail!("Qwen 语音识别返回空文本");
        }
        Ok(out.trim().to_string())
    }
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

/// Qwen 响应的 content 可能是字符串，也可能是 `[{type:text, text:...}]` 数组。
fn extract_text_content(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|it| {
                it["text"]
                    .as_str()
                    .or_else(|| it["type"].as_str().and_then(|t| (t == "text").then(|| "")))
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn short(text: &str) -> String {
    let t: String = text.chars().take(300).collect();
    if text.chars().count() > 300 {
        format!("{t}…")
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn spawn_asr_mock(status: u16, body: String) -> (String, std::sync::Arc<std::sync::Mutex<String>>) {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let cap = captured.clone();
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
                let mut buf = [0u8; 256 * 1024];
                let n = sock.read(&mut buf).await.unwrap();
                if let Ok(s) = std::str::from_utf8(&buf[..n]) {
                    *cap.lock().unwrap() = s.to_string();
                }
                let reason = if status == 200 { "OK" } else { "Error" };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        });
        (rx.recv().unwrap(), captured)
    }

    fn fish_config(base_url: String) -> Config {
        let mut cfg = Config::default();
        cfg.asr.enabled = true;
        cfg.asr.provider = "fish".into();
        cfg.asr.endpoint = format!("{base_url}/v1/asr");
        cfg.asr.language = "zh".into();
        cfg
    }

    fn qwen_config(base_url: String) -> Config {
        let mut cfg = Config::default();
        cfg.asr.enabled = true;
        cfg.asr.provider = "qwen".into();
        cfg.qwen.base_url = base_url;
        cfg.qwen.asr_model = "qwen3.5-omni-flash".into();
        cfg
    }

    #[test]
    fn from_config_gates() {
        let mut cfg = Config::default();
        cfg.asr.api_key_env = "FURINA_TEST_FISH_KEY_GATES".into();
        assert!(AsrClient::from_config(&cfg).is_err(), "未启用应报错");
        cfg.asr.enabled = true;
        // provider=qwen 时读 QWEN_API_KEY（唯一名，避免测试并行竞态）
        cfg.qwen.api_key_env = "FURINA_TEST_QWEN_KEY_GATES".into();
        assert!(AsrClient::from_config(&cfg).is_err(), "缺 key 应报错");
        unsafe {
            std::env::set_var("FURINA_TEST_QWEN_KEY_GATES", "qk");
        }
        let c = AsrClient::from_config(&cfg).unwrap();
        assert_eq!(c.language(), "zh");
        assert_eq!(c.provider(), "qwen");
        unsafe {
            std::env::remove_var("FURINA_TEST_QWEN_KEY_GATES");
        }
    }

    #[test]
    fn unknown_provider_rejected() {
        let mut cfg = Config::default();
        cfg.asr.enabled = true;
        cfg.asr.provider = "nonexistent".into();
        cfg.qwen.api_key_env = "FURINA_TEST_QWEN_KEY_UNKNOWN".into();
        unsafe {
            std::env::set_var("FURINA_TEST_QWEN_KEY_UNKNOWN", "qk");
        }
        let err = AsrClient::from_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("未知 ASR provider"), "{err}");
        unsafe {
            std::env::remove_var("FURINA_TEST_QWEN_KEY_UNKNOWN");
        }
    }

    #[tokio::test]
    async fn fish_transcribe_success() {
        let (addr, captured) = spawn_asr_mock(200, r#"{"text":"你好，芙芙"}"#.into());
        let mut cfg = fish_config(format!("http://{addr}"));
        cfg.asr.api_key_env = "FURINA_TEST_FISH_KEY_OK".into();
        unsafe {
            std::env::set_var("FURINA_TEST_FISH_KEY_OK", "test-key");
        }
        let client = AsrClient::from_config(&cfg).unwrap();
        let text = client.transcribe(b"fake-webm-bytes".to_vec(), "audio/webm").await.unwrap();
        assert_eq!(text, "你好，芙芙");
        let req = captured.lock().unwrap().clone();
        assert!(req.contains("name=\"audio\""), "multipart 应含 audio 字段");
        assert!(req.contains("furina_input.webm"), "应带音频文件名");
        assert!(req.contains("name=\"language\""), "应带 language 字段");
        assert!(req.contains("zh"), "language 应为 zh");
        assert!(req.to_lowercase().contains("bearer test-key"), "应带 Bearer 鉴权");
        unsafe {
            std::env::remove_var("FURINA_TEST_FISH_KEY_OK");
        }
    }

    #[tokio::test]
    async fn fish_transcribe_surfaces_api_error() {
        let (addr, _) = spawn_asr_mock(400, r#"{"message":"bad audio"}"#.into());
        let mut cfg = fish_config(format!("http://{addr}"));
        cfg.asr.api_key_env = "FURINA_TEST_FISH_KEY_ERR".into();
        unsafe {
            std::env::set_var("FURINA_TEST_FISH_KEY_ERR", "k");
        }
        let client = AsrClient::from_config(&cfg).unwrap();
        let err = client.transcribe(vec![1, 2, 3], "audio/webm").await.unwrap_err().to_string();
        assert!(err.contains("400"), "{err}");
        unsafe {
            std::env::remove_var("FURINA_TEST_FISH_KEY_ERR");
        }
    }

    #[tokio::test]
    async fn qwen_transcribe_success() {
        let (addr, captured) = spawn_asr_mock(
            200,
            r#"{"choices":[{"message":{"content":"你好，芙芙","role":"assistant"}}]}"#.into(),
        );
        let mut cfg = qwen_config(format!("http://{addr}"));
        cfg.qwen.api_key_env = "FURINA_TEST_QWEN_KEY_OK".into();
        unsafe {
            std::env::set_var("FURINA_TEST_QWEN_KEY_OK", "qwen-key");
        }
        let client = AsrClient::from_config(&cfg).unwrap();
        assert_eq!(client.provider(), "qwen");
        let text = client
            .transcribe(b"fake-wav-bytes".to_vec(), "audio/wav")
            .await
            .unwrap();
        assert_eq!(text, "你好，芙芙");
        let req = captured.lock().unwrap().clone();
        assert!(req.contains("/chat/completions"), "应请求 OpenAI 兼容端点: {req}");
        assert!(req.contains("qwen3.5-omni-flash"), "应带模型名");
        assert!(req.contains("input_audio"), "应含 input_audio");
        assert!(req.contains("\"type\":\"input_audio\""), "类型应为 input_audio");
        let expected_b64 =
            base64::engine::general_purpose::STANDARD.encode(b"fake-wav-bytes");
        assert!(
            req.contains(&format!("data:;base64,{expected_b64}")),
            "应带 data:;base64, 前缀"
        );
        assert!(req.contains("\"modalities\":[\"text\"]"), "应声明文本输出");
        assert!(req.to_lowercase().contains("bearer qwen-key"), "应带 Bearer 鉴权");
        unsafe {
            std::env::remove_var("FURINA_TEST_QWEN_KEY_OK");
        }
    }

    #[tokio::test]
    async fn qwen_rejects_non_wav() {
        let mut cfg = qwen_config("http://127.0.0.1:1".into());
        cfg.qwen.api_key_env = "FURINA_TEST_QWEN_KEY_NONWAV".into();
        unsafe {
            std::env::set_var("FURINA_TEST_QWEN_KEY_NONWAV", "qk");
        }
        let client = AsrClient::from_config(&cfg).unwrap();
        let err = client
            .transcribe(vec![1, 2, 3], "audio/webm")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("仅支持 wav"), "{err}");
        unsafe {
            std::env::remove_var("FURINA_TEST_QWEN_KEY_NONWAV");
        }
    }

    #[tokio::test]
    async fn qwen_handles_array_content() {
        let (addr, _) = spawn_asr_mock(
            200,
            r#"{"choices":[{"message":{"content":[{"type":"text","text":"今天天气真好"}]}}]}"#.into(),
        );
        let mut cfg = qwen_config(format!("http://{addr}"));
        cfg.qwen.api_key_env = "FURINA_TEST_QWEN_KEY_ARR".into();
        unsafe {
            std::env::set_var("FURINA_TEST_QWEN_KEY_ARR", "qk");
        }
        let client = AsrClient::from_config(&cfg).unwrap();
        let text = client
            .transcribe(b"wav".to_vec(), "audio/wav")
            .await
            .unwrap();
        assert_eq!(text, "今天天气真好");
        unsafe {
            std::env::remove_var("FURINA_TEST_QWEN_KEY_ARR");
        }
    }

    #[tokio::test]
    async fn transcribe_empty_audio_fails() {
        let mut cfg = Config::default();
        cfg.asr.enabled = true;
        cfg.qwen.api_key_env = "FURINA_TEST_QWEN_KEY_EMPTY".into();
        unsafe {
            std::env::set_var("FURINA_TEST_QWEN_KEY_EMPTY", "qk");
        }
        let client = AsrClient::from_config(&cfg).unwrap();
        let err = client.transcribe(vec![], "audio/wav").await.unwrap_err().to_string();
        assert!(err.contains("没有可识别"));
        unsafe {
            std::env::remove_var("FURINA_TEST_QWEN_KEY_EMPTY");
        }
    }

    /// 手动 E2E（需要真实 key 与 wav 文件，不进 CI）：
    ///   $env:FURINA_TEST_ASR_WAV="C:\path\speech.wav"
    ///   cargo test -p furina-core qwen_e2e_manual -- --ignored --nocapture
    /// 环境变量 QWEN_API_KEY 需已设置（或 secrets.env 由 furina 加载）。
    #[tokio::test]
    #[ignore]
    async fn qwen_e2e_manual() {
        let key = std::env::var("QWEN_API_KEY").unwrap_or_default();
        let wav = std::env::var("FURINA_TEST_ASR_WAV").unwrap_or_default();
        if key.is_empty() || wav.is_empty() {
            eprintln!("跳过：需要 QWEN_API_KEY 与 FURINA_TEST_ASR_WAV");
            return;
        }
        let mut cfg = Config::default();
        cfg.asr.enabled = true;
        cfg.asr.provider = "qwen".into();
        let client = AsrClient::from_config(&cfg).unwrap();
        let audio = std::fs::read(&wav).unwrap();
        let text = client.transcribe(audio, "audio/wav").await.unwrap();
        println!("ASR 结果: {text}");
        assert!(!text.trim().is_empty(), "转写结果为空");
    }
}
