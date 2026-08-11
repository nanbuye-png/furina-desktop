//! 执行关键节点的人格化插话（展示层辅助，非 Agent 运行时路径）。
//!
//! 审批通过/拒绝、任务完成/失败、验证失败等关键事件由 LLM 依据当前心情与上下文
//! 生成一句短小插话，替代 furina.yaml 里写死的模板句（如"很好，本神准了。"）。
//! LLM 失败或未启用时返回 `None`，由调用方回退现有模板，保证零回归。

use crate::config::Config;
use crate::llm::{DeepSeekClient, LlmClient};
use furina_proto::ChatMessage;
use std::env;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// 插话所需的最小上下文（由展示层在事件发生时组装）。
#[derive(Debug, Clone)]
pub struct InterjectCtx {
    /// 心情标识（如 "proud"）。
    pub mood: String,
    /// 心情中文名（如 "得意"）。
    pub mood_label: String,
    /// 事件类型（approval_granted / approval_denied / task_done / task_failed / verify_fail）。
    pub event_kind: String,
    /// 动作/详情摘要（审批类型、任务摘要、验证错误等）。
    pub action_detail: String,
    /// 表达策略（theatrical / casual / gentle / serious），来自 Soul 规则。
    pub strategy: String,
}

/// LLM 插话生成器：每次一个短调用，串行由调用方保证顺序。
pub struct Interjector {
    client: DeepSeekClient,
    enabled: bool,
    max_per_task: u32,
    max_chars: usize,
    budget: Arc<AtomicU32>,
}

impl Interjector {
    /// 从配置解析（复用激活 provider 的 key/端点/模型，temperature 用插话配置）。
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        let provider = cfg.active_provider()?;
        let key = env::var(&provider.api_key_env).map_err(|_| {
            anyhow::anyhow!(
                "缺少环境变量 {}（提供方 {}）。可在 .furina/secrets.env 中添加 {}=你的key",
                provider.api_key_env,
                provider.label,
                provider.api_key_env
            )
        })?;
        let client = DeepSeekClient::new(
            provider.base_url.clone(),
            key,
            provider.model.clone(),
            cfg.interject.temperature,
        )?;
        Ok(Self {
            client,
            enabled: cfg.interject.enabled,
            max_per_task: cfg.interject.max_per_task,
            max_chars: cfg.interject.max_chars.max(10),
            budget: Arc::new(AtomicU32::new(0)),
        })
    }

    /// 新任务开始时重置插话预算。
    pub fn reset_budget(&self) {
        self.budget.store(0, Ordering::SeqCst);
    }

    /// 生成一句插话；未启用 / 超出预算 / LLM 失败均返回 None（调用方回退模板）。
    pub async fn line(&self, ctx: InterjectCtx) -> Option<String> {
        if !self.enabled || !self.try_take() {
            return None;
        }
        let messages = build_messages(&ctx);
        let resp = self.client.complete(&messages, &[]).await.ok()?;
        sanitize(&resp.content?, self.max_chars)
    }

    /// 预算配额（测试用）。
    fn try_take(&self) -> bool {
        if self.max_per_task == 0 {
            return true;
        }
        let mut cur = self.budget.load(Ordering::SeqCst);
        loop {
            if cur >= self.max_per_task {
                return false;
            }
            match self.budget.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }
}

/// 组装插话提示词（System 强约束 + User 上下文）。
pub fn build_messages(ctx: &InterjectCtx) -> Vec<ChatMessage> {
    let system = "你是 Furina（芙宁娜）。用户正和你一起在电脑上完成任务，现在发生了一个事件。\
    请用一句话（不超过30字）以 Furina 的口吻回应。要求：\
    1) 语气符合给定的心情；\
    2) 只表达态度、情绪或想法，绝不编造或复述技术细节、路径、数据、结果；\
    3) 不要使用「本神准了」「审判结束」这类固定模板句；\
    4) 直接输出这句话，不要任何前缀、引号或解释。";
    let user = format!(
        "心情：{}（{}）\n事件：{}\n详情：{}\n表达策略：{}\n请回应：",
        ctx.mood_label,
        ctx.mood,
        ctx.event_kind,
        ctx.action_detail,
        ctx.strategy
    );
    vec![
        ChatMessage::System { content: system.into() },
        ChatMessage::User { content: user },
    ]
}

/// 清理 LLM 输出：去首尾空白/引号、只取第一行、按字符截断。
pub fn sanitize(text: &str, max_chars: usize) -> Option<String> {
    let mut s = text.trim();
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= 2 {
        let (first, last) = (chars[0], chars[chars.len() - 1]);
        let open = matches!(first, '“' | '"' | '「' | '『');
        let close = matches!(last, '”' | '"' | '」' | '』');
        if open && close {
            let start = first.len_utf8();
            let end = s.len() - last.len_utf8();
            if end > start {
                s = s[start..end].trim();
            }
        }
    }
    let first_line = s.lines().next().unwrap_or("").trim().to_string();
    let truncated: String = first_line.chars().take(max_chars).collect();
    let out = truncated.trim();
    if out.is_empty() {
        None
    } else {
        Some(out.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// 最小 SSE mock：返回固定内容增量。
    fn spawn_sse_mock(content: &str) -> String {
        let body = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{content}\"}}}}]}}\n\n\
             data: [DONE]\n\n"
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
        });
        rx.recv().unwrap()
    }

        fn ctx(kind: &str) -> InterjectCtx {
        InterjectCtx {
            mood: "proud".into(),
            mood_label: "得意".into(),
            event_kind: kind.into(),
            action_detail: "term.run pytest".into(),
            strategy: "theatrical".into(),
        }
    }

    #[test]
    fn prompt_includes_mood_event_and_guardrails() {
        let msgs = build_messages(&ctx("approval_granted"));
        assert_eq!(msgs.len(), 2);
        let system = match &msgs[0] {
            ChatMessage::System { content } => content.clone(),
            _ => panic!("first message should be system"),
        };
        let user = match &msgs[1] {
            ChatMessage::User { content } => content.clone(),
            _ => panic!("second message should be user"),
        };
        assert!(system.contains("绝不编造或复述技术细节"));
        assert!(system.contains("固定模板句"));
        assert!(user.contains("得意"));
        assert!(user.contains("approval_granted"));
        assert!(user.contains("term.run pytest"));
        assert!(user.contains("表达策略：theatrical"));
    }

    #[test]
    fn sanitize_strips_quotes_lines_and_truncates() {
        assert_eq!(sanitize("  「哼，准了。」  ", 60).unwrap(), "哼，准了。");
        assert_eq!(sanitize("\"哼，准了。\"", 60).unwrap(), "哼，准了。");
        let multi = "第一行\n第二行";
        assert_eq!(sanitize(multi, 60).unwrap(), "第一行");
        assert_eq!(sanitize("一二三四五六", 3).unwrap(), "一二三");
        assert_eq!(sanitize("   ", 60), None);
        assert_eq!(sanitize("", 60), None);
    }

    #[tokio::test]
    async fn budget_caps_per_task_and_resets() {
        let mut cfg = Config::default();
        cfg.interject.enabled = true;
        cfg.interject.max_per_task = 2;
        cfg.llm.providers = Vec::new(); // 走 legacy 字段，便于测试指定端点
        cfg.llm.base_url = "http://127.0.0.1:1".into(); // 不会真正发请求：预算先耗尽
        cfg.llm.api_key_env = "FURINA_TEST_INTERJECT_KEY_BUDGET".into();
        cfg.model = "deepseek-chat".into();
        unsafe {
            std::env::set_var("FURINA_TEST_INTERJECT_KEY_BUDGET", "k");
        }
        let inj = Interjector::from_config(&cfg).unwrap();
        assert!(inj.try_take());
        assert!(inj.try_take());
        assert!(!inj.try_take(), "超出预算应拒绝");
        inj.reset_budget();
        assert!(inj.try_take(), "重置后应恢复配额");
        unsafe {
            std::env::remove_var("FURINA_TEST_INTERJECT_KEY_BUDGET");
        }
    }

    #[tokio::test]
    async fn disabled_returns_none_without_request() {
        let mut cfg = Config::default();
        cfg.interject.enabled = false;
        cfg.interject.max_per_task = 0;
        cfg.llm.providers = Vec::new();
        cfg.llm.base_url = "http://127.0.0.1:1".into();
        cfg.llm.api_key_env = "FURINA_TEST_INTERJECT_KEY_DISABLED".into();
        cfg.model = "deepseek-chat".into();
        unsafe {
            std::env::set_var("FURINA_TEST_INTERJECT_KEY_DISABLED", "k");
        }
        let inj = Interjector::from_config(&cfg).unwrap();
        assert!(inj.line(ctx("task_done")).await.is_none());
        unsafe {
            std::env::remove_var("FURINA_TEST_INTERJECT_KEY_DISABLED");
        }
    }

    #[tokio::test]
    async fn llm_error_returns_none() {
        let mut cfg = Config::default();
        cfg.interject.enabled = true;
        cfg.interject.max_per_task = 0;
        cfg.llm.providers = Vec::new();
        cfg.llm.base_url = "http://127.0.0.1:1".into(); // 不可达 → 网络错误 → None
        cfg.llm.api_key_env = "FURINA_TEST_INTERJECT_KEY_ERR".into();
        cfg.model = "deepseek-chat".into();
        unsafe {
            std::env::set_var("FURINA_TEST_INTERJECT_KEY_ERR", "k");
        }
        let inj = Interjector::from_config(&cfg).unwrap();
        assert!(inj.line(ctx("task_done")).await.is_none());
        unsafe {
            std::env::remove_var("FURINA_TEST_INTERJECT_KEY_ERR");
        }
    }

    #[tokio::test]
    async fn llm_fixed_reply_passes_through() {
        let addr = spawn_sse_mock("哼，准了。");
        let mut cfg = Config::default();
        cfg.interject.enabled = true;
        cfg.interject.max_per_task = 0;
        cfg.llm.providers = Vec::new();
        cfg.llm.base_url = format!("http://{addr}");
        cfg.llm.api_key_env = "FURINA_TEST_INTERJECT_KEY_FIXED".into();
        cfg.model = "deepseek-chat".into();
        unsafe {
            std::env::set_var("FURINA_TEST_INTERJECT_KEY_FIXED", "k");
        }
        let inj = Interjector::from_config(&cfg).unwrap();
        let out = inj.line(ctx("approval_granted")).await;
        assert_eq!(out.as_deref(), Some("哼，准了。"));
        unsafe {
            std::env::remove_var("FURINA_TEST_INTERJECT_KEY_FIXED");
        }
    }

    /// 手动 E2E：真实 LLM 生成一句插话（需 FURINA_API_KEY）。
    ///   cargo test -p furina-core interject_e2e_manual -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn interject_e2e_manual() {
        let key = std::env::var("FURINA_API_KEY").unwrap_or_default();
        if key.is_empty() {
            eprintln!("跳过：需要 FURINA_API_KEY");
            return;
        }
        let cfg = Config::default();
        let inj = Interjector::from_config(&cfg).unwrap();
        let out = inj.line(ctx("approval_granted")).await;
        println!("真实插话: {out:?}");
        assert!(out.is_some(), "应生成一句插话");
        assert!(out.unwrap().chars().count() <= 60, "插话不应超长");
    }
}
