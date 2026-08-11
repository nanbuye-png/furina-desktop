//! 视觉预处理代理：图片 → 视觉模型 → 文字描述。
//!
//! 图片数据只发送到配置里标记 `vision: true` 的提供方；主模型（人格载体）
//! 只看到描述文本，因此人格与主链路完全不受影响。

use crate::config::{Config, ProviderConfig};
use serde_json::json;
use std::path::Path;
use std::time::Duration;

pub struct VisionClient {
    client: reqwest::Client,
    provider: ProviderConfig,
    api_key: String,
    temperature: f64,
}

impl VisionClient {
    /// 从配置解析视觉提供方；未启用或未配置时返回错误（调用方按"未配置"处理）。
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        if !cfg.vision.enabled {
            anyhow::bail!("视觉识别未启用（config.yaml 中 vision.enabled: true 后可启用）");
        }
        let provider = match cfg.vision.preferred_provider.as_str() {
            "auto" => cfg
                .llm
                .providers
                .iter()
                .filter(|p| p.vision)
                .map(|p| cfg.resolved_provider(p))
                .find(|p| std::env::var(&p.api_key_env).map(|k| !k.is_empty()).unwrap_or(false))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "未找到可用的视觉提供方：请为某个 provider 标记 vision: true 并配置 api_key_env"
                    )
                })?,
            id => cfg
                .provider(id)
                .filter(|p| p.vision)
                .ok_or_else(|| anyhow::anyhow!("未找到 vision: true 的提供方 {id}"))?,
        };
        let api_key = std::env::var(&provider.api_key_env).map_err(|_| {
            anyhow::anyhow!(
                "缺少环境变量 {}（视觉提供方 {}）",
                provider.api_key_env,
                provider.label
            )
        })?;
        let client = crate::proxy::apply_system_proxy(
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(20))
                .timeout(Duration::from_secs(120)),
        )
        .build()?;
        Ok(Self {
            client,
            provider,
            api_key,
            temperature: cfg.llm.temperature,
        })
    }

    pub fn provider_label(&self) -> &str {
        &self.provider.label
    }

    pub fn provider_base_url(&self) -> &str {
        &self.provider.base_url
    }

    pub fn provider_model(&self) -> &str {
        &self.provider.model
    }

    /// 把图片字节转为 OpenAI 兼容的 data URI。
    pub fn data_uri(bytes: &[u8], mime: &str) -> String {
        use base64::Engine;
        format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    /// 请求视觉模型生成图片描述（OpenAI 兼容、非流式）。
    pub async fn describe_image(&self, bytes: &[u8], mime: &str, prompt: &str) -> anyhow::Result<String> {
        let url = format!("{}/chat/completions", self.provider.base_url.trim_end_matches('/'));
        let body = json!({
            "model": self.provider.model,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": prompt},
                    {"type": "image_url", "image_url": {"url": Self::data_uri(bytes, mime)}}
                ]
            }],
            "max_tokens": 800,
            "stream": false,
            "temperature": self.temperature.min(0.3),
        });
        let fut = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send();
        let resp = tokio::time::timeout(Duration::from_secs(30), fut)
            .await
            .map_err(|_| anyhow::anyhow!("视觉模型请求超时（30s）"))??;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("视觉模型 API 错误 {status}: {}", short(&text));
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|_| anyhow::anyhow!("视觉模型响应不是有效 JSON"))?;
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.trim().to_string())
            .ok_or_else(|| anyhow::anyhow!("视觉模型响应缺少 content"))?;
        if content.is_empty() {
            anyhow::bail!("视觉模型返回了空描述");
        }
        Ok(content)
    }
}

/// 按文件扩展名推断图片 MIME。
pub fn mime_for(path: &Path) -> anyhow::Result<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "png" => Ok("image/png".into()),
        "jpg" | "jpeg" => Ok("image/jpeg".into()),
        "webp" => Ok("image/webp".into()),
        other => anyhow::bail!("不支持的图片格式: {other}"),
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

    fn spawn_http_mock(status: u16, body: String) -> String {
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
                let mut buf = [0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let reason = if status == 200 { "OK" } else { "Error" };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        });
        rx.recv().unwrap()
    }

    fn vision_config(base_url: String, key_env: &str) -> Config {
        let mut cfg = Config::default();
        cfg.vision.enabled = true;
        cfg.vision.preferred_provider = "vision-test".into();
        cfg.llm.providers = vec![ProviderConfig {
            id: "vision-test".into(),
            label: "VisionTest".into(),
            base_url,
            api_key_env: key_env.into(),
            model: "glm-4v-flash".into(),
            vision: true,
        }];
        cfg.llm.active_provider = Some("vision-test".into());
        cfg
    }

    #[test]
    fn data_uri_construction() {
        let uri = VisionClient::data_uri(b"abc", "image/png");
        assert!(uri.starts_with("data:image/png;base64,"));
        assert!(uri.ends_with("YWJj"), "abc 的 base64 应为 YWJj: {uri}");
    }

    #[test]
    fn from_config_disabled_errors() {
        let cfg = Config::default();
        assert!(VisionClient::from_config(&cfg).is_err());
    }

    #[test]
    fn from_config_requires_vision_flag_and_key() {
        std::env::set_var("VISION_TEST_KEY", "k");
        let cfg = vision_config("https://x".into(), "VISION_TEST_KEY");
        let vc = VisionClient::from_config(&cfg).unwrap();
        assert_eq!(vc.provider_label(), "VisionTest");
        assert_eq!(vc.provider_model(), "glm-4v-flash");

        let mut cfg_no_flag = cfg.clone();
        cfg_no_flag.llm.providers[0].vision = false;
        assert!(VisionClient::from_config(&cfg_no_flag).is_err());

        std::env::remove_var("VISION_TEST_KEY");
        let mut cfg_no_key = cfg.clone();
        cfg_no_key.llm.providers[0].api_key_env = "VISION_NO_KEY".into();
        assert!(VisionClient::from_config(&cfg_no_key).is_err());
    }

    #[tokio::test]
    async fn describe_image_success() {
        let addr = spawn_http_mock(200, r#"{"choices":[{"message":{"content":"一只猫在窗台上"}}]}"#.into());
        std::env::set_var("VISION_TEST_KEY", "k");
        let cfg = vision_config(format!("http://{addr}"), "VISION_TEST_KEY");
        let vc = VisionClient::from_config(&cfg).unwrap();
        let desc = vc.describe_image(b"\x89PNG", "image/png", "描述图片").await.unwrap();
        assert_eq!(desc, "一只猫在窗台上");
        std::env::remove_var("VISION_TEST_KEY");
    }

    #[tokio::test]
    async fn describe_image_http_error() {
        let addr = spawn_http_mock(401, r#"{"error":{"message":"bad key"}}"#.into());
        std::env::set_var("VISION_TEST_KEY", "bad");
        let cfg = vision_config(format!("http://{addr}"), "VISION_TEST_KEY");
        let vc = VisionClient::from_config(&cfg).unwrap();
        let err = vc.describe_image(b"x", "image/png", "描述图片").await.unwrap_err().to_string();
        assert!(err.contains("401"), "应包含状态码: {err}");
        std::env::remove_var("VISION_TEST_KEY");
    }

    #[test]
    fn mime_inference() {
        assert_eq!(mime_for(Path::new("a.PNG")).unwrap(), "image/png");
        assert_eq!(mime_for(Path::new("a.jpg")).unwrap(), "image/jpeg");
        assert!(mime_for(Path::new("a.gif")).is_err());
    }
}
