//! LLM gateway: OpenAI/DeepSeek-compatible streaming client plus a
//! scripted FixtureLlm used by tests and golden replays.

use async_trait::async_trait;
use furina_proto::{ChatMessage, ToolCall, ToolFunctionCall, ToolSpec};
use futures_util::StreamExt;
use serde::Deserialize;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, messages: &[ChatMessage], tools: &[ToolSpec]) -> anyhow::Result<LlmResponse>;
    /// 流式完成：`on_delta` 按 LLM 增量文本块回调（用于打字机渲染与逐句语音）。
    /// 默认实现 = 非流式 complete 后一次性回调整段，保证所有客户端行为一致。
    async fn complete_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let resp = self.complete(messages, tools).await?;
        if let Some(c) = &resp.content {
            on_delta(c);
        }
        Ok(resp)
    }
    /// 连通性检查：发起一次最小请求，返回模型标识；不支持时返回错误。
    async fn ping(&self) -> anyhow::Result<String> {
        anyhow::bail!("当前 LLM 客户端不支持连通性检查")
    }
}

pub struct DeepSeekClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    temperature: f64,
}

impl DeepSeekClient {
    pub fn new(base_url: String, api_key: String, model: String, temperature: f64) -> anyhow::Result<Self> {
        let client = crate::proxy::apply_system_proxy(
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(20))
                .timeout(std::time::Duration::from_secs(600)),
        )
        .build()?;
        Ok(Self {
            client,
            base_url,
            api_key,
            model,
            temperature,
        })
    }
}

#[derive(Deserialize)]
struct SseChunk {
    #[serde(default)]
    choices: Vec<SseChoice>,
    #[serde(default)]
    usage: Option<SseUsage>,
}

#[derive(Deserialize)]
struct SseChoice {
    delta: SseDelta,
}

#[derive(Deserialize)]
struct SseDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<SseDeltaToolCall>,
}

#[derive(Deserialize)]
struct SseDeltaToolCall {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<SseDeltaFunction>,
}

#[derive(Deserialize)]
struct SseDeltaFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize, Default)]
struct SseUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

#[async_trait]
impl LlmClient for DeepSeekClient {
    async fn complete(&self, messages: &[ChatMessage], tools: &[ToolSpec]) -> anyhow::Result<LlmResponse> {
        self.stream_inner(messages, tools, None).await
    }

    async fn complete_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.stream_inner(messages, tools, Some(on_delta)).await
    }

    async fn ping(&self) -> anyhow::Result<String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1,
            "stream": false,
            "temperature": 0.0,
        });
        let fut = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send();
        let resp = tokio::time::timeout(std::time::Duration::from_secs(20), fut)
            .await
            .map_err(|_| anyhow::anyhow!("连通性检查超时（20s）"))??;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("HTTP {status}: {}", short_error(&text));
        }
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|_| anyhow::anyhow!("响应不是有效 JSON"))?;
        let model = v["model"].as_str().unwrap_or(&self.model);
        Ok(format!("{model} 连通正常"))
    }
}

impl DeepSeekClient {
    /// 共享的 SSE 流式解析：`on_delta` 为 None 时等价于原 complete（聚合返回）。
    /// 对发送与读取阶段的瞬时传输错误自动重试（最多 2 次退避），覆盖
    /// 代理/节点抖动的 `peer closed connection`、连接重置、流中断、超时等场景。
    async fn stream_inner(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        mut on_delta: Option<&mut (dyn for<'a> FnMut(&'a str) + Send)>,
    ) -> anyhow::Result<LlmResponse> {
        let mut last_msg = String::new();
        for attempt in 0..3 {
            // 显式短借用重借：避免 Option<&mut> 的借用跨迭代存活。
            let result = match &mut on_delta {
                Some(cb) => self.request_once(messages, tools, Some(&mut *cb)).await,
                None => self.request_once(messages, tools, None).await,
            };
            match result {
                Ok(r) => return Ok(r),
                Err(LlmRequestError { kind, message }) => {
                    let transient = matches!(kind, LlmRequestErrorKind::Transient);
                    last_msg = message;
                    if !transient || attempt >= 2 {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(400 * (attempt + 1))).await;
                }
            }
        }
        Err(anyhow::anyhow!(
            "模型网络请求失败（已自动重试）——请检查网络/代理后重试：{last_msg}"
        ))
    }

    /// 单次请求：发送 + 解析 SSE。传输层错误标记为 Transient（可重试），HTTP 状态错误为 Fatal。
    async fn request_once(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        mut on_delta: Option<&mut (dyn for<'a> FnMut(&'a str) + Send)>,
    ) -> Result<LlmResponse, LlmRequestError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let tool_objs: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| serde_json::json!({"type": "function", "function": t}))
            .collect();
        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "tools": tool_objs,
            "tool_choice": "auto",
            "stream": true,
            "temperature": self.temperature,
        });
        let resp = match self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Err(LlmRequestError::transient(e.to_string())),
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmRequestError::fatal(format!(
                "LLM API 错误 {status}: {}",
                short_error(&text)
            )));
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut content = String::new();
        let mut tool_calls: Vec<(usize, ToolCall)> = Vec::new();
        let mut usage = SseUsage::default();

        'outer: while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) if content.is_empty() => {
                    return Err(LlmRequestError::transient(e.to_string()))
                }
                Err(e) => {
                    return Err(LlmRequestError::fatal(format!(
                        "模型响应流中断（已收到部分文本，为避免重复输出未自动重试）: {e}"
                    )))
                }
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                let line = line.trim();
                if !line.starts_with("data:") {
                    continue;
                }
                let data = line[5..].trim();
                if data == "[DONE]" {
                    break 'outer;
                }
                let c: SseChunk = match serde_json::from_str(data) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if let Some(u) = c.usage {
                    usage = u;
                }
                for choice in c.choices {
                    if let Some(d) = choice.delta.content {
                        content.push_str(&d);
                        if let Some(cb) = on_delta.as_deref_mut() {
                            cb(&d);
                        }
                    }
                    for tc in choice.delta.tool_calls {
                        while tool_calls.len() <= tc.index {
                            tool_calls.push((
                                tc.index,
                                ToolCall {
                                    id: String::new(),
                                    r#type: "function".into(),
                                    function: ToolFunctionCall { name: String::new(), arguments: String::new() },
                                },
                            ));
                        }
                        let (_, call) = &mut tool_calls[tc.index];
                        if let Some(id) = tc.id {
                            call.id = id;
                        }
                        if let Some(f) = tc.function {
                            if let Some(n) = f.name {
                                call.function.name = n;
                            }
                            if let Some(a) = f.arguments {
                                call.function.arguments.push_str(&a);
                            }
                        }
                    }
                }
            }
        }

        let calls: Vec<ToolCall> = tool_calls.into_iter().map(|(_, c)| c).collect();
        Ok(LlmResponse {
            content: if content.is_empty() { None } else { Some(content) },
            tool_calls: calls,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
        })
    }

}

enum LlmRequestErrorKind {
    Transient,
    Fatal,
}

struct LlmRequestError {
    kind: LlmRequestErrorKind,
    message: String,
}

impl LlmRequestError {
    fn transient(message: String) -> Self {
        Self { kind: LlmRequestErrorKind::Transient, message }
    }

    fn fatal(message: String) -> Self {
        Self { kind: LlmRequestErrorKind::Fatal, message }
    }
}

fn short_error(text: &str) -> String {
    let t: String = text.chars().take(300).collect();
    if text.chars().count() > 300 {
        format!("{t}…")
    } else {
        t
    }
}

/// Scripted LLM for golden tests: replays assistant turns from a JSON file.
pub struct FixtureLlm {
    turns: Mutex<VecDeque<LlmResponse>>,
    usage_per_turn: u64,
}

impl FixtureLlm {
    pub fn from_turns(turns: Vec<LlmResponse>, usage_per_turn: u64) -> Self {
        Self { turns: Mutex::new(turns.into()), usage_per_turn }
    }

    pub fn from_json_file(path: &Path, usage_per_turn: u64) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let v: serde_json::Value = serde_json::from_str(&text)?;
        let mut turns = VecDeque::new();
        if let Some(arr) = v["turns"].as_array() {
            for t in arr {
                let content = t["content"].as_str().map(|s| s.to_string());
                let mut calls = Vec::new();
                if let Some(tc) = t["tool_calls"].as_array() {
                    for c in tc {
                        calls.push(ToolCall {
                            id: c["id"].as_str().unwrap_or("call_0").to_string(),
                            r#type: "function".into(),
                            function: ToolFunctionCall {
                                name: c["function"]["name"].as_str().unwrap_or("").to_string(),
                                arguments: c["function"]["arguments"].as_str().unwrap_or("{}").to_string(),
                            },
                        });
                    }
                }
                turns.push_back(LlmResponse {
                    content,
                    tool_calls: calls,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                });
            }
        }
        if turns.is_empty() {
            anyhow::bail!("fixture 中没有 turns");
        }
        Ok(Self { turns: Mutex::new(turns), usage_per_turn })
    }
}

#[async_trait]
impl LlmClient for FixtureLlm {
    async fn complete(&self, _messages: &[ChatMessage], _tools: &[ToolSpec]) -> anyhow::Result<LlmResponse> {
        let mut guard = self.turns.lock().unwrap();
        let mut r = guard.pop_front().ok_or_else(|| anyhow::anyhow!("fixture turns 已耗尽"))?;
        r.prompt_tokens = self.usage_per_turn;
        r.completion_tokens = self.usage_per_turn;
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn spawn_sse_mock() -> String {
        let rt_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"fs.read_file\",\"arguments\":\"{\\\"path\\\":\\\"a.py\\\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n",
            "data: [DONE]\n\n",
        );
        let body = rt_body.to_string();
        let handle = tokio::runtime::Handle::current();
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
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
            let _ = handle;
        });
        rx.recv().unwrap()
    }

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
                let mut buf = [0u8; 4096];
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

    #[tokio::test]
    async fn parses_streaming_sse() {
        let addr = spawn_sse_mock();
        let client = DeepSeekClient::new(format!("http://{addr}"), "k".into(), "deepseek-chat".into(), 0.0).unwrap();
        let resp = client.complete(&[], &[]).await.unwrap();
        assert_eq!(resp.content.as_deref(), Some("你好"));
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].function.name, "fs.read_file");
        assert_eq!(resp.tool_calls[0].function.arguments, "{\"path\":\"a.py\"}");
        assert_eq!(resp.prompt_tokens, 10);
        assert_eq!(resp.completion_tokens, 5);
    }

    #[tokio::test]
    async fn complete_stream_forwards_deltas() {
        let addr = spawn_sse_mock();
        let client = DeepSeekClient::new(format!("http://{addr}"), "k".into(), "deepseek-chat".into(), 0.0).unwrap();
        let mut deltas: Vec<String> = Vec::new();
        let mut cb = |d: &str| deltas.push(d.to_string());
        let resp = client.complete_stream(&[], &[], &mut cb).await.unwrap();
        assert_eq!(resp.content.as_deref(), Some("你好"));
        assert!(!deltas.is_empty(), "流式回调应收到增量");
        assert_eq!(deltas.concat(), "你好");
    }

    #[tokio::test]
    async fn complete_retries_transient_connection_error() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
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
                // 第一次连接：响应头声明 100 字节但立即关闭 → 传输中断（模拟代理掐断）
                if let Ok((mut s1, _)) = listener.accept().await {
                    let mut buf = [0u8; 4096];
                    let _ = s1.read(&mut buf).await;
                    let bad = "HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\n";
                    let _ = s1.write_all(bad.as_bytes()).await;
                    drop(s1);
                }
                // 第二次连接：正常 SSE 响应
                if let Ok((mut s2, _)) = listener.accept().await {
                    let mut buf = [0u8; 4096];
                    let _ = s2.read(&mut buf).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = s2.write_all(resp.as_bytes()).await;
                }
            });
        });
        let addr = rx.recv().unwrap();
        let client =
            DeepSeekClient::new(format!("http://{addr}"), "k".into(), "deepseek-chat".into(), 0.0)
                .unwrap();
        let resp = client.complete(&[], &[]).await.unwrap();
        assert_eq!(resp.content.as_deref(), Some("你好"));
    }

    #[tokio::test]
    async fn ping_reports_ok() {
        let addr = spawn_http_mock(200, r#"{"model":"deepseek-chat","choices":[{"message":{"content":"pong"}}]}"#.into());
        let client = DeepSeekClient::new(format!("http://{addr}"), "k".into(), "deepseek-chat".into(), 0.0).unwrap();
        let msg = client.ping().await.unwrap();
        assert!(msg.contains("deepseek-chat"), "应返回模型标识: {msg}");
    }

    #[tokio::test]
    async fn ping_reports_http_error() {
        let addr = spawn_http_mock(401, r#"{"error":{"message":"invalid api key"}}"#.into());
        let client = DeepSeekClient::new(format!("http://{addr}"), "bad".into(), "deepseek-chat".into(), 0.0).unwrap();
        let err = client.ping().await.unwrap_err().to_string();
        assert!(err.contains("401"), "应包含状态码: {err}");
        assert!(err.contains("invalid api key"), "应包含错误详情: {err}");
    }
}
