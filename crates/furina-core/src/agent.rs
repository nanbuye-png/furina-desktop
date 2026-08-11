//! The agent loop: scan → plan → approve → execute → verify → repair,
//! with hard stop conditions and a structured event stream.

use crate::config::Config;
use crate::context::{truncate_text, trim_incomplete_turn, ContextManager};
use crate::gateway::{escapes_workspace, is_private_path, ActionKind, PermissionGateway, Verdict};
use crate::llm::LlmClient;
use crate::sidecar::Sidecar;
use crate::state::AgentState;
use crate::web::WebClient;
use crate::web_cache::{WebCache, WebCacheEntry};
use async_trait::async_trait;
use furina_proto::{ChatMessage, Event, ScanResult, TaskOutcome, ToolCall, ToolSpec};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[async_trait]
pub trait Approver: Send {
    async fn confirm(&mut self, prompt: &str) -> bool;
}

/// 人格上下文提供者：Soul Engine 实现此 trait，核心在构建 LLM 消息时调用。
/// 纯人格层——只提供语气与行为倾向，永不触碰权限/工具执行。
pub trait PromptContextProvider: Send + Sync {
    /// 用户在本次回合的输入（用于更新情绪/关系）。
    fn observe_user_text(&self, text: &str);
    /// 运行时事件（工具调用/验证/审批/完成等），用于驱动情绪与记忆。
    fn observe_event(&self, event: &Event);
    /// 返回动态人格注入块；空字符串表示无需注入。
    fn context_block(&self) -> String;
}

pub struct Agent {
    pub workspace: PathBuf,
    pub model: String,
    cfg: Config,
    sidecar: Sidecar,
    llm: Box<dyn LlmClient>,
    events: Arc<dyn crate::sidecar::EventSink>,
    approver: Box<dyn Approver>,
    prompt_context: Option<Box<dyn PromptContextProvider>>,
    system_prompt: String,
    gateway: PermissionGateway,
    /// Soul 私有/运行时配置目录（绝对路径）：工具一律禁止读写。
    private_paths: Vec<PathBuf>,
    known_hashes: HashMap<String, String>,
    web: Result<WebClient, String>,
    web_cache: Option<WebCache>,
    context: ContextManager,
    transcript: Vec<ChatMessage>,
    state: AgentState,
    repair_rounds: u32,
    steps: u32,
    total_tokens: u64,
    wrote_files: bool,
    verified: bool,
    web_approved: bool,
    test_command: String,
    scanned: bool,
    pending_scan: Option<String>,
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: Config,
        workspace: PathBuf,
        sidecar: Sidecar,
        llm: Box<dyn LlmClient>,
        events: Arc<dyn crate::sidecar::EventSink>,
        approver: Box<dyn Approver>,
        system_prompt: String,
    ) -> Self {
        let gateway = PermissionGateway::new(cfg.approval.auto_allow.clone(), cfg.approval.danger_patterns.clone());
        let web = WebClient::from_config(&cfg.web).map_err(|e| e.to_string());
        let context = ContextManager {
            per_request_max_tokens: cfg.llm.per_request_max_tokens,
            keep_recent: 20,
            summary_max_chars: 10_000,
            message_max_chars: 6_000,
        };
        let model = cfg
            .active_provider()
            .map(|p| p.model)
            .unwrap_or_else(|_| cfg.model.clone());
        Self {
            model,
            workspace,
            cfg,
            sidecar,
            llm,
            events,
            approver,
            prompt_context: None,
            system_prompt,
            gateway,
            private_paths: Vec::new(),
            known_hashes: HashMap::new(),
            web,
            web_cache: None,
            context,
            transcript: Vec::new(),
            state: AgentState::Idle,
            repair_rounds: 0,
            steps: 0,
            total_tokens: 0,
            wrote_files: false,
            verified: false,
            web_approved: false,
            test_command: String::new(),
            scanned: false,
            pending_scan: None,
        }
    }

    /// Clear session state (used for one-shot `run`; chat keeps it).
    pub fn reset(&mut self) {
        self.transcript.clear();
        self.state = AgentState::Idle;
        self.repair_rounds = 0;
        self.steps = 0;
        self.total_tokens = 0;
        self.wrote_files = false;
        self.verified = false;
        self.web_approved = false;
        self.test_command.clear();
        self.scanned = false;
        self.pending_scan = None;
    }

    /// 注入人格上下文提供者（Soul Engine 适配器）。
    pub fn set_prompt_context(&mut self, provider: Box<dyn PromptContextProvider>) {
        self.prompt_context = Some(provider);
    }

    /// 注入网页缓存（Web Intelligence Phase 2）：web.open / web.search 成功后自动落盘。
    pub fn set_web_cache(&mut self, cache: WebCache) {
        self.web_cache = Some(cache);
    }

    /// 注入 Soul 私有目录（如 `<root>/persona`、`<root>/.furina`），
    /// 这些路径对 LLM 工具完全不可读/不可写（人格机制、记忆与密钥的边界）。
    pub fn set_private_paths(&mut self, paths: Vec<PathBuf>) {
        self.private_paths = paths;
    }

    /// 当前会话转录快照（供会话结束后的记忆抽取使用）。
    pub fn transcript_snapshot(&self) -> Vec<ChatMessage> {
        self.transcript.clone()
    }

    pub async fn run_task(&mut self, task: &str) -> anyhow::Result<TaskOutcome> {
        // 丢弃上一轮被中断留下的未完成回合，保证发给 LLM 的消息序列合法。
        trim_incomplete_turn(&mut self.transcript);
        // 人格层先观察用户输入，再生成动态注入块。
        if let Some(p) = &self.prompt_context {
            p.observe_user_text(task);
        }
        let context_line = self
            .prompt_context
            .as_ref()
            .map(|p| p.context_block())
            .filter(|s| !s.is_empty())
            .map(|c| format!("\n\n{c}"))
            .unwrap_or_default();
        self.state = AgentState::Planning;
        self.emit_state();
        self.emit(Event::SessionStarted {
            workspace: self.workspace.display().to_string(),
            model: self.model.clone(),
        });

        if self.transcript.is_empty() {
            self.transcript.push(ChatMessage::System { content: self.system_prompt.clone() });
            self.transcript.push(ChatMessage::User { content: format!("任务：{task}{context_line}") });
        } else {
            self.transcript.push(ChatMessage::User { content: format!("新任务：{task}{context_line}") });
        }

        let tools = self.llm_tools();
        let mut final_text = String::new();

        loop {
            if self.steps >= self.cfg.agent.max_steps_per_task {
                return self.finish(false, format!("超过最大步骤数（{}）", self.cfg.agent.max_steps_per_task)).await;
            }
            if self.total_tokens >= self.cfg.llm.max_total_tokens {
                return self.finish(false, format!("超过 token 预算（{}）", self.cfg.llm.max_total_tokens)).await;
            }

            let messages = self.context.fit(&self.transcript);
            // 流式 LLM：增量文本实时发射 MessageDelta（UI 打字机 + 逐句语音），
            // 完整内容仍在返回后写入转录（上下文/记忆/日志语义不变）。
            let events = self.events.clone();
            let resp = {
                let mut on_delta = |chunk: &str| {
                    if !chunk.is_empty() {
                        events.emit(Event::MessageDelta {
                            content: chunk.to_string(),
                        });
                    }
                };
                self.llm
                    .complete_stream(&messages, &tools, &mut on_delta)
                    .await?
            };
            self.total_tokens = self.total_tokens.saturating_add(resp.prompt_tokens + resp.completion_tokens);
            self.emit(Event::Tokens {
                prompt: resp.prompt_tokens,
                completion: resp.completion_tokens,
                total: self.total_tokens,
            });

            let has_tools = !resp.tool_calls.is_empty();
            self.transcript.push(ChatMessage::Assistant {
                content: resp.content.clone(),
                tool_calls: if has_tools { Some(resp.tool_calls.clone()) } else { None },
            });
            if let Some(text) = resp.content.clone() {
                self.emit(Event::Message { role: "assistant".into(), content: text.clone() });
                final_text = text;
            }

            if !has_tools {
                // 最终消息：若修改过文件且从未验证通过，自动运行一次测试。
                if self.wrote_files && !self.verified && !self.test_command.is_empty() {
                    let test_cmd = self.test_command.clone();
                    self.run_verification(&test_cmd).await?;
                    if self.repair_rounds > self.cfg.agent.max_repair_rounds {
                        return self
                            .finish(false, format!("修复轮数超过上限（{}）", self.cfg.agent.max_repair_rounds))
                            .await;
                    }
                    if !self.verified {
                        continue; // 让 LLM 根据验证结果继续修复
                    }
                }
                return self.finish(true, final_text).await;
            }

            // 惰性扫描：任务真正需要调用工具时才扫描项目。
            // 纯聊天回合不会走到这里，因此对话保持纯文本。
            if !self.scanned {
                self.scan_project().await?;
            }

            for call in &resp.tool_calls {
                self.steps += 1;
                if self.steps >= self.cfg.agent.max_steps_per_task {
                    return self.finish(false, format!("超过最大步骤数（{}）", self.cfg.agent.max_steps_per_task)).await;
                }
                self.handle_tool_call(call).await?;
            }
            if self.repair_rounds > self.cfg.agent.max_repair_rounds {
                return self.finish(false, format!("修复轮数超过上限（{}）", self.cfg.agent.max_repair_rounds)).await;
            }
        }
    }

    /// 惰性项目扫描：仅在首次执行工具前运行一次，结果拼进首个工具结果，
    /// 供模型在后续回合使用（测试命令、技术栈等）。
    async fn scan_project(&mut self) -> anyhow::Result<()> {
        let scan_value = self.sidecar.call("scan.start", json!({})).await?;
        let scan: ScanResult = serde_json::from_value(scan_value)?;
        self.test_command = scan.test_command.clone();
        let summary = format!(
            "项目类型: {}; 语言: {}; 清单: {}; 文件数: {}",
            scan.project_type,
            scan.language,
            scan.manifests.join(","),
            scan.total_files
        );
        self.emit(Event::Scan {
            project_type: scan.project_type.clone(),
            test_command: scan.test_command.clone(),
            summary: summary.clone(),
        });
        self.pending_scan = Some(format!("[项目扫描]\n{summary}\n\n"));
        self.scanned = true;
        Ok(())
    }

    async fn handle_tool_call(&mut self, call: &ToolCall) -> anyhow::Result<()> {
        let name = call.function.name.clone();
        let args: Value = serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
        let scan_prefix = self.pending_scan.take().unwrap_or_default();
        self.emit(Event::ToolCall { name: name.clone(), summary: args.to_string() });
        self.state = AgentState::Executing;
        self.emit_state();

        let result: Result<Value, String> = match name.as_str() {
            "fs_read_file" | "fs_search" | "fs_diff" => {
                let path = args.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                let private = is_private_path(&self.private_paths, &self.workspace, &path);
                let escape = escapes_workspace(&self.workspace, &path);
                let result = if private {
                    Ok(json!({"denied": true, "reason": "soul-private 目录（人格配置/记忆/密钥）禁止访问"}))
                } else if escape {
                    let detail = format!("读取工作区外路径：{path}");
                    if self.ask_approval("读取文件", &detail).await {
                        self.sidecar
                            .call(&sidecar_method(&name), with_escape_flag(args, true))
                            .await
                            .map_err(|e| e.to_string())
                    } else {
                        Ok(json!({"denied": true, "message": "用户拒绝读取"}))
                    }
                } else {
                    self.sidecar.call(&sidecar_method(&name), args).await.map_err(|e| e.to_string())
                };
                // 记录读取后的文件哈希，供写入前并发修改检测
                if name == "fs_read_file" && result.is_ok() {
                    if let Ok(h) = self.sidecar.call("fs.hash", json!({"path": path.clone()})).await {
                        if let Some(hash) = h["hash"].as_str() {
                            if !hash.is_empty() {
                                self.known_hashes.insert(path, hash.to_string());
                            }
                        }
                    }
                }
                result
            }
            "git_status" | "git_diff" => {
                self.sidecar.call(&sidecar_method(&name), args).await.map_err(|e| e.to_string())
            }
            "fs_write_file" | "fs_create_file" => {
                let path = args.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                let private = is_private_path(&self.private_paths, &self.workspace, &path);
                let escape = escapes_workspace(&self.workspace, &path);
                // 外部并发修改检测：读取后哈希变化则在审批详情中警示
                let current_hash = self
                    .sidecar
                    .call("fs.hash", json!({"path": path.clone()}))
                    .await
                    .ok()
                    .and_then(|h| h["hash"].as_str().map(|s| s.to_string()));
                let warning = external_change_warning(
                    self.known_hashes.get(&path).map(|s| s.as_str()),
                    current_hash.as_deref(),
                );
                if private {
                    Ok(json!({"denied": true, "reason": "soul-private 目录（人格配置/记忆/密钥）禁止写入"}))
                } else {
                    match self.gateway.check(&ActionKind::WriteFile { path: path.clone() }) {
                        Verdict::RequireApproval => {
                        let mut detail = self.approval_detail_for_write(&args).await;
                        if let Some(w) = &warning {
                            detail.push_str(w);
                        }
                        if self.ask_approval(&format!("写入文件 {path}"), &detail).await {
                            let r = self.sidecar
                                .call(&sidecar_method(&name), with_escape_flag(args, escape))
                                .await
                                .map_err(|e| e.to_string());
                            self.refresh_hash_after_write(&path, &r).await;
                            r
                        } else {
                            Ok(json!({"denied": true, "message": "用户拒绝写入"}))
                        }
                    }
                        Verdict::Block(reason) => Ok(json!({"blocked": reason})),
                        Verdict::Allow => {
                        let r = self.sidecar
                            .call(&sidecar_method(&name), with_escape_flag(args, escape))
                            .await
                            .map_err(|e| e.to_string());
                        self.refresh_hash_after_write(&path, &r).await;
                        r
                        }
                    }
                }
            }
            "term_run" => {
                let command = args.get("command").and_then(Value::as_str).unwrap_or("").to_string();
                match self.gateway.check(&ActionKind::RunCommand { command: command.clone() }) {
                    Verdict::Allow => self.run_term(&command, args).await,
                    Verdict::RequireApproval => {
                        if self.ask_approval("运行命令", &command).await {
                            self.run_term(&command, args).await
                        } else {
                            Ok(json!({"denied": true, "message": "用户拒绝执行命令"}))
                        }
                    }
                    Verdict::Block(reason) => Ok(json!({"blocked": reason})),
                }
            }
            "git_commit" => {
                let message = args.get("message").and_then(Value::as_str).unwrap_or("").to_string();
                if self.ask_approval("执行 git commit", &message).await {
                    self.sidecar.call("git.commit", args).await.map_err(|e| e.to_string())
                } else {
                    Ok(json!({"denied": true, "message": "用户拒绝提交"}))
                }
            }
            "web_search" => {
                let query = args.get("query").and_then(Value::as_str).unwrap_or("").to_string();
                if self.web_approved || self.ask_approval("联网搜索", &query).await {
                    self.web_approved = true;
                    match &self.web {
                        Ok(client) => match client.search(&query).await {
                            Ok(results) => {
                                if let Some(cache) = &self.web_cache {
                                    for r in &results {
                                        let _ = cache.put(WebCacheEntry {
                                            url: r.url.clone(),
                                            title: r.title.clone(),
                                            snippet: r.snippet.clone(),
                                            content: String::new(),
                                            fetched_at_ms: now_ms(),
                                        });
                                    }
                                }
                                Ok(json!({
                                    "results": results.iter().map(|r| json!({"title": r.title, "url": r.url, "snippet": r.snippet})).collect::<Vec<_>>()
                                }))
                            }
                            Err(e) => Err(e.to_string()),
                        },
                        Err(e) => Ok(json!({"error": e})),
                    }
                } else {
                    Ok(json!({"denied": true, "message": "用户拒绝联网搜索"}))
                }
            }
            "web_open" => {
                let url = args.get("url").and_then(Value::as_str).unwrap_or("").to_string();
                if self.web_approved || self.ask_approval("打开网页", &url).await {
                    self.web_approved = true;
                    match &self.web {
                        Ok(client) => match client.open(&url).await {
                            Ok(text) => {
                                if let Some(cache) = &self.web_cache {
                                    let _ = cache.put(WebCacheEntry {
                                        url: url.clone(),
                                        title: extract_open_title(&text),
                                        snippet: String::new(),
                                        content: text.clone(),
                                        fetched_at_ms: now_ms(),
                                    });
                                }
                                Ok(json!({"url": url, "content": text}))
                            }
                            Err(e) => Err(e.to_string()),
                        },
                        Err(e) => Ok(json!({"error": e})),
                    }
                } else {
                    Ok(json!({"denied": true, "message": "用户拒绝打开网页"}))
                }
            }
            _ => Err(format!("未知工具: {name}")),
        };

        match result {
            Ok(v) => {
                let denied = v.get("denied").is_some();
                let blocked = v.get("blocked").is_some();
                let ok = !denied && !blocked;
                let summary = summarize_value(&v);
                self.emit(Event::ToolResult { name: name.clone(), ok, summary: summary.clone() });
                if (name == "fs_write_file" || name == "fs_create_file") && ok {
                    self.wrote_files = true;
                }
                self.transcript.push(ChatMessage::Tool {
                    tool_call_id: call.id.clone(),
                    content: format!("{scan_prefix}{}", truncate_text(&summary, self.cfg.llm.tool_output_truncate)),
                });
            }
            Err(e) => {
                self.emit(Event::ToolResult { name: name.clone(), ok: false, summary: e.clone() });
                self.transcript.push(ChatMessage::Tool {
                    tool_call_id: call.id.clone(),
                    content: format!("{scan_prefix}{}", truncate_text(&e, self.cfg.llm.tool_output_truncate)),
                });
            }
        }
        Ok(())
    }

    async fn run_term(&mut self, command: &str, args: Value) -> Result<Value, String> {
        let is_test = self.gateway.is_test_command(command);
        if is_test {
            self.state = AgentState::Verifying;
            self.emit_state();
        }
        let result = match self.sidecar.call("term.run", args).await {
            Ok(v) => v,
            Err(e) => {
                // 测试命令执行本身出错（RPC 错误）也计入修复轮数，避免空转
                if is_test {
                    self.repair_rounds += 1;
                    self.state = AgentState::Repairing;
                    self.emit(Event::Verify {
                        passed: false,
                        detail: format!("测试命令执行出错: {e}"),
                    });
                }
                return Err(e.to_string());
            }
        };
        if is_test {
            let exit = result["exit_code"].as_i64().unwrap_or(-1);
            let stdout = result["stdout"].as_str().unwrap_or("").to_string();
            let stderr = result["stderr"].as_str().unwrap_or("").to_string();
            self.emit_test_report(command, &stdout, &stderr, exit).await;
            let detail = truncate_text(&format!("命令: {command}\n退出码: {exit}\n{stdout}{stderr}"), 6_000);
            if exit == 0 {
                self.verified = true;
                self.emit(Event::Verify { passed: true, detail });
            } else {
                self.repair_rounds += 1;
                self.state = AgentState::Repairing;
                self.emit(Event::Verify { passed: false, detail });
            }
        }
        Ok(result)
    }

    async fn run_verification(&mut self, command: &str) -> anyhow::Result<()> {
        self.state = AgentState::Verifying;
        self.emit_state();
        let result = self.sidecar.call("term.run", json!({"command": command})).await?;
        let exit = result["exit_code"].as_i64().unwrap_or(-1);
        let stdout = result["stdout"].as_str().unwrap_or("").to_string();
        let stderr = result["stderr"].as_str().unwrap_or("").to_string();
        self.emit_test_report(command, &stdout, &stderr, exit).await;
        let detail = truncate_text(&format!("命令: {command}\n退出码: {exit}\n{stdout}{stderr}"), 6_000);
        if exit == 0 {
            self.verified = true;
            self.emit(Event::Verify { passed: true, detail: detail.clone() });
        } else {
            self.repair_rounds += 1;
            self.state = AgentState::Repairing;
            self.emit(Event::Verify { passed: false, detail: detail.clone() });
        }
        self.transcript.push(ChatMessage::User {
            content: format!("[自动验证] 运行 {command}：\n{detail}"),
        });
        Ok(())
    }

    /// 解析测试输出为结构化报告并发出 TestReport 事件（解析失败不阻断任务）。
    async fn emit_test_report(&mut self, command: &str, stdout: &str, stderr: &str, exit: i64) {
        let Ok(report) = self
            .sidecar
            .call(
                "tests.parse",
                json!({"command": command, "stdout": stdout, "stderr": stderr, "exit_code": exit}),
            )
            .await
        else {
            return;
        };
        self.emit(Event::TestReport {
            command: command.to_string(),
            framework: report["framework"].as_str().unwrap_or("unknown").to_string(),
            passed: exit == 0,
            total: report["total"].as_i64().unwrap_or(0),
            failed: report["failed_count"].as_i64().unwrap_or(0),
            summary: report["summary"].as_str().unwrap_or("").to_string(),
        });
    }

    async fn approval_detail_for_write(&mut self, args: &Value) -> String {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("").to_string();
        let content = args.get("content").and_then(Value::as_str).unwrap_or("").to_string();
        match self.sidecar.call("fs.diff", json!({"path": path, "content": content})).await {
            Ok(v) => v["diff"].as_str().unwrap_or("（无差异）").to_string(),
            Err(e) => format!("无法预览差异: {e}"),
        }
    }

    async fn ask_approval(&mut self, kind: &str, detail: &str) -> bool {
        self.state = AgentState::AwaitingApproval;
        self.emit_state();
        self.emit(Event::ApprovalRequired { kind: kind.to_string(), detail: detail.to_string() });
        let ok = self
            .approver
            .as_mut()
            .confirm(&format!("【{kind}】\n{detail}"))
            .await;
        if ok {
            self.emit(Event::ApprovalGranted { kind: kind.to_string() });
        } else {
            self.emit(Event::ApprovalDenied { kind: kind.to_string() });
        }
        self.state = AgentState::Executing;
        ok
    }

    /// 写入成功后刷新已知哈希（下次写入以新哈希为基准）。
    async fn refresh_hash_after_write(&mut self, path: &str, result: &Result<Value, String>) {
        if result.is_err() {
            return;
        }
        if let Ok(h) = self.sidecar.call("fs.hash", json!({"path": path})).await {
            if let Some(hash) = h["hash"].as_str() {
                if !hash.is_empty() {
                    self.known_hashes.insert(path.to_string(), hash.to_string());
                }
            }
        }
    }

    fn llm_tools(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "fs_read_file".into(),
                description: "读取文本文件（工作区内直接读取；工作区外路径需用户审批）".into(),
                parameters: json!({"type":"object","properties":{"path":{"type":"string","description":"相对 workspace 的文件路径"},"max_bytes":{"type":"integer"}},"required":["path"]}),
            },
            ToolSpec {
                name: "fs_write_file".into(),
                description: "写入/覆盖 workspace 内文件（需要用户审批）".into(),
                parameters: json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}),
            },
            ToolSpec {
                name: "fs_create_file".into(),
                description: "创建新文件（需要审批，已存在则失败）".into(),
                parameters: json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}),
            },
            ToolSpec {
                name: "fs_search".into(),
                description: "在 workspace 内搜索文本或正则（工作区外需用户审批）".into(),
                parameters: json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"regex":{"type":"boolean"}},"required":["pattern"]}),
            },
            ToolSpec {
                name: "fs_diff".into(),
                description: "预览新内容与现有文件的差异（工作区外需用户审批）".into(),
                parameters: json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}),
            },
            ToolSpec {
                name: "term_run".into(),
                description: "在 workspace 内运行命令（测试/只读命令自动放行，其余需要审批）".into(),
                parameters: json!({"type":"object","properties":{"command":{"type":"string"},"cwd":{"type":"string"},"timeout_ms":{"type":"integer"}},"required":["command"]}),
            },
            ToolSpec {
                name: "git_status".into(),
                description: "查看 git 状态".into(),
                parameters: json!({"type":"object"}),
            },
            ToolSpec {
                name: "git_diff".into(),
                description: "查看工作区未暂存差异".into(),
                parameters: json!({"type":"object","properties":{"path":{"type":"string"}}}),
            },
            ToolSpec {
                name: "git_commit".into(),
                description: "暂存所有改动并提交（需要审批）".into(),
                parameters: json!({"type":"object","properties":{"message":{"type":"string"}},"required":["message"]}),
            },
            ToolSpec {
                name: "web_search".into(),
                description: "联网搜索（需用户审批）：返回标题/链接/摘要".into(),
                parameters: json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}),
            },
            ToolSpec {
                name: "web_open".into(),
                description: "打开网页并返回正文文本（需用户审批）".into(),
                parameters: json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}),
            },
        ]
    }

    fn emit(&self, event: Event) {
        if let Some(p) = &self.prompt_context {
            p.observe_event(&event);
        }
        self.events.emit(event);
    }

    fn emit_state(&self) {
        self.emit(Event::StateChanged { state: self.state.as_str().to_string() });
    }

    async fn finish(&mut self, success: bool, summary: String) -> anyhow::Result<TaskOutcome> {
        self.state = if success { AgentState::Done } else { AgentState::Failed };
        self.emit_state();
        self.emit(Event::Done { success, summary: summary.clone() });
        Ok(TaskOutcome {
            success,
            summary,
            steps: self.steps,
            repair_rounds: self.repair_rounds,
            total_tokens: self.total_tokens,
        })
    }
}

fn with_escape_flag(mut v: Value, escape: bool) -> Value {
    if escape {
        if let Some(o) = v.as_object_mut() {
            o.insert("allow_escape".into(), json!(true));
        }
    }
    v
}

fn summarize_value(v: &Value) -> String {
    let mut s = v.to_string();
    if s.chars().count() > 2_000 {
        s = truncate_text(&s, 2_000);
    }
    s
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 从 web.open 返回的文本中提取 "标题：xxx" 前缀。
fn extract_open_title(text: &str) -> String {
    if let Some(rest) = text.strip_prefix("标题：") {
        rest.lines().next().unwrap_or("").trim().to_string()
    } else {
        String::new()
    }
}

/// Map LLM-facing tool names (underscore, DeepSeek-compatible) to sidecar
/// JSON-RPC method names (dotted).
fn sidecar_method(tool: &str) -> String {
    match tool {
        "fs_read_file" => "fs.read_file".into(),
        "fs_write_file" => "fs.write_file".into(),
        "fs_create_file" => "fs.create_file".into(),
        "fs_search" => "fs.search".into(),
        "fs_diff" => "fs.diff".into(),
        "term_run" => "term.run".into(),
        "git_status" => "git.status".into(),
        "git_diff" => "git.diff".into(),
        "git_commit" => "git.commit".into(),
        _ => tool.to_string(),
    }
}

/// 外部并发修改检测：读取后哈希与当前不一致时给出警示文案。
fn external_change_warning(known: Option<&str>, current: Option<&str>) -> Option<String> {
    match (known, current) {
        (Some(k), Some(c)) if !c.is_empty() && k != c => {
            Some("\n\n⚠️ 文件自读取后已被外部修改（哈希不一致）。仍要覆盖吗？".to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_change_warns_when_hash_differs() {
        let w = external_change_warning(Some("aaa"), Some("bbb"));
        assert!(w.unwrap().contains("外部修改"));
    }

    #[test]
    fn external_change_no_warning_when_same_or_unknown() {
        assert!(external_change_warning(Some("aaa"), Some("aaa")).is_none());
        assert!(external_change_warning(None, Some("bbb")).is_none());
        assert!(external_change_warning(Some("aaa"), None).is_none());
    }
}
