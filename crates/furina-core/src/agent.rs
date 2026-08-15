//! The agent loop: scan → plan → approve → execute → verify → repair,
//! with hard stop conditions and a structured event stream.

use crate::config::{Config, SafeAppConfig};
use crate::app_launcher::{resolve_app_target, AppApprovalStore};
use crate::context::{strip_runtime_soul_context, truncate_text, trim_incomplete_turn, ContextManager};
use crate::gateway::{escapes_workspace, is_private_path, ActionKind, PermissionGateway, Verdict};
use crate::interaction::{analyze_with_fallback, InteractionAnalyzer};
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
    /// 从原始用户输入中提取明确事实；默认实现保持旧行为兼容。
    fn observe_user_facts(&self, _text: &str) {}
    /// 已验证的轻量语义分类结果；默认实现保持旧行为兼容。
    fn observe_trigger_id(&self, _trigger_id: &str) {}
    /// 运行时事件（工具调用/验证/审批/完成等），用于驱动情绪与记忆。
    fn observe_event(&self, event: &Event);
    /// 返回动态人格注入块；空字符串表示无需注入。
    fn context_block(&self) -> String;
    /// 返回指定运行模式的人格上下文。默认兼容旧实现。
    fn context_block_for(&self, _mode: &str) -> String {
        self.context_block()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractionMode {
    Chat,
    Agent,
}

impl InteractionMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Agent => "agent",
        }
    }
}

fn normalize_app_key(value: &str) -> String {
    value.chars().filter(|character| character.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn safe_app_definition(configured: &[SafeAppConfig], requested: &str) -> Option<SafeAppConfig> {
    let requested = requested.trim();
    let requested_key = normalize_app_key(requested);
    let requested_name = requested.replace('\\', "/").rsplit('/').next().unwrap_or("").to_ascii_lowercase();
    let is_qq_music = requested_key == "qqmusic" || requested_key == "qq音乐" || requested_name == "qqmusic.exe";
    let is_qq = requested_key == "qq" || requested_key == "qqim" || requested_key == "qq聊天" || requested_name == "qq.exe";
    let requested_id = if is_qq_music { "qq_music" } else if is_qq { "qq" } else { requested };
    let mut app = configured.iter().find(|candidate| normalize_app_key(&candidate.id) == normalize_app_key(requested_id)).cloned();
    if app.is_none() && is_qq_music {
        app = Some(SafeAppConfig { id: "qq_music".into(), label: "QQ音乐".into(), executable: String::new(), args: Vec::new(), enabled: true });
    }
    if app.is_none() && is_qq {
        app = Some(SafeAppConfig { id: "qq".into(), label: "QQ".into(), executable: String::new(), args: Vec::new(), enabled: true });
    }
    if let Some(mut app) = app {
        if (is_qq_music && requested_name == "qqmusic.exe") || (is_qq && requested_name == "qq.exe") {
            app.executable = requested.to_string();
        }
        Some(app)
    } else { None }
}

fn executable_path_from_command(command: &str) -> Option<String> {
    let mut candidates = Vec::new();
    let chars: Vec<char> = command.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\'' || chars[index] == '"' {
            let quote = chars[index];
            index += 1;
            let begin = index;
            while index < chars.len() && chars[index] != quote {
                index += 1;
            }
            if begin < index {
                candidates.push(chars[begin..index].iter().collect::<String>());
            }
        }
        index += 1;
    }
    candidates.extend(command.split_whitespace().map(|token| token.trim_matches(['"', '\'']).to_string()));
    candidates.into_iter().find(|candidate| {
        let lower = candidate.to_ascii_lowercase();
        lower.ends_with(".exe") && (candidate.contains('\\') || candidate.contains('/') || candidate.contains(':'))
    })
}

fn launch_name_from_command(command: &str) -> Option<String> {
    let lower = command.to_ascii_lowercase();
    let has_launch_intent = ["start ", "start\"", "start-process", "open ", "启动", "打开", "运行"]
        .iter()
        .any(|marker| lower.contains(marker));
    if !has_launch_intent {
        return None;
    }
    let mut tokens = command.split_whitespace().map(|token| token.trim_matches(['"', '\'']));
    while let Some(token) = tokens.next() {
        let normalized = token.to_ascii_lowercase();
        if matches!(normalized.as_str(), "start" | "start-process" | "open" | "cmd" | "/c" | "powershell" | "-command") {
            continue;
        }
        if token.starts_with('-') || token.starts_with('/') || token.is_empty() {
            continue;
        }
        if token.eq_ignore_ascii_case("qq") || token.eq_ignore_ascii_case("qqmusic") || !token.contains('=') {
            return Some(token.to_string());
        }
    }
    None
}

fn known_app_request_from_command(configured: &[SafeAppConfig], command: &str) -> Option<String> {
    let lower = command.to_ascii_lowercase();
    let has_launch_intent = ["start ", "start\"", "start-process", "open ", "启动", "打开", "运行"]
        .iter()
        .any(|marker| lower.contains(marker));
    let shell_launch_intent = lower.contains("-command") && lower.contains('&');
    let first_token = lower
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(['"', '\'']);
    let direct_executable = first_token.ends_with(".exe")
        && (first_token.contains('\\') || first_token.contains('/') || first_token == "qq.exe" || first_token == "qqmusic.exe");
    if !has_launch_intent && !shell_launch_intent && !direct_executable {
        return None;
    }
    if let Some(path) = executable_path_from_command(command) {
        return Some(path);
    }

    if lower.contains("qqmusic.exe") || lower.contains("qq音乐") || (has_launch_intent && lower.contains("qqmusic")) {
        return Some("qq_music".into());
    }
    if lower.contains("qq.exe") || (has_launch_intent && lower.split(|c: char| !c.is_ascii_alphanumeric()).any(|part| part == "qq")) {
        return Some("qq".into());
    }

    if let Some(name) = launch_name_from_command(command) {
        return Some(name);
    }
    configured.iter().find_map(|app| {
        let executable = app.executable.replace('\\', "/").rsplit('/').next().unwrap_or("").to_ascii_lowercase();
        if !executable.is_empty() && lower.contains(&executable) {
            return Some(app.id.clone());
        }
        let key = normalize_app_key(&app.id);
        if has_launch_intent && !key.is_empty() && normalize_app_key(command).contains(&key) {
            return Some(app.id.clone());
        }
        None
    })
}

fn dynamic_app_definition(requested: &str) -> Option<SafeAppConfig> {
    let requested = requested.trim();
    if requested.is_empty() {
        return None;
    }
    let path = std::path::Path::new(requested);
    if path.is_file() && path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("exe")) {
        let stem = path.file_stem()?.to_string_lossy().to_string();
        return Some(SafeAppConfig {
            id: normalize_app_key(&stem),
            label: stem,
            executable: requested.to_string(),
            args: Vec::new(),
            enabled: true,
        });
    }
    if requested.contains('\\') || requested.contains('/') || requested.contains(':') || requested.len() > 64 {
        return None;
    }
    let id = normalize_app_key(requested);
    if id.is_empty() {
        return None;
    }
    Some(SafeAppConfig {
        id,
        label: requested.to_string(),
        executable: String::new(),
        args: Vec::new(),
        enabled: true,
    })
}
fn classify_interaction_mode(text: &str) -> InteractionMode {
    let lower = text.to_lowercase();
    const TECHNICAL_MARKERS: &[&str] = &[
        "代码", "编程", "函数", "报错", "错误", "异常", "测试", "构建", "编译", "项目",
        "文件", "目录", "路径", "配置", "命令", "终端", "脚本", "仓库", "git", "github",
        "依赖", "安装", "接口", "api", "数据库", "日志", "读取", "写入", "修改", "修复",
        "删除", "提交", "推送", "权限", "workspace", "rust", "python", "javascript", "typescript",
        "react", "tauri", "cargo", "npm", "yaml", "json", "toml", "exe", "vrm", "sidecar",
    ];
    let has_path_or_code = lower.contains("```")
        || lower.contains("\\")
        || lower.contains("/src/")
        || [".rs", ".py", ".js", ".jsx", ".ts", ".tsx", ".yaml", ".yml", ".json", ".toml", ".md"]
            .iter()
            .any(|extension| lower.contains(extension));
    if has_path_or_code || TECHNICAL_MARKERS.iter().any(|marker| lower.contains(marker)) {
        InteractionMode::Agent
    } else {
        InteractionMode::Chat
    }
}

fn dialogue_text(content: &str) -> String {
    let content = content
        .strip_prefix("任务：")
        .or_else(|| content.strip_prefix("新任务："))
        .unwrap_or(content);
    strip_runtime_soul_context(content)
}

fn append_current_context(messages: &mut [ChatMessage], context: &str) {
    if context.is_empty() {
        return;
    }
    if let Some(ChatMessage::System { content }) = messages.first_mut() {
        content.push_str("\n\n");
        content.push_str(context);
    }
}

fn recent_dialogue(transcript: &[ChatMessage], max_messages: usize) -> Vec<(String, String)> {
    let mut messages = transcript
        .iter()
        .filter_map(|message| match message {
            ChatMessage::User { content } => Some(("user".to_string(), dialogue_text(content))),
            ChatMessage::Assistant { content: Some(content), tool_calls: None } => {
                Some(("assistant".to_string(), dialogue_text(content)))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if messages.len() > max_messages {
        messages.drain(..messages.len() - max_messages);
    }
    messages
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
    interaction_analyzer: Option<Box<dyn InteractionAnalyzer>>,
    interaction_analyzer_timeout_ms: u64,
    system_prompt: String,
    gateway: PermissionGateway,
    /// Soul 私有/运行时配置目录（绝对路径）：工具一律禁止读写。
    private_paths: Vec<PathBuf>,
    known_hashes: HashMap<String, String>,
    web: Result<WebClient, String>,
    web_cache: Option<WebCache>,
    context: ContextManager,
    conversation: Vec<ChatMessage>,
    task_transcript: Vec<ChatMessage>,
    current_task_user: Option<String>,
    app_approvals: AppApprovalStore,
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
            workspace: workspace.clone(),
            cfg,
            sidecar,
            llm,
            events,
            approver,
            prompt_context: None,
            interaction_analyzer: None,
            interaction_analyzer_timeout_ms: 1500,
            system_prompt,
            gateway,
            private_paths: Vec::new(),
            known_hashes: HashMap::new(),
            web,
            web_cache: None,
            context,
            conversation: Vec::new(),
            task_transcript: Vec::new(),
            current_task_user: None,
            app_approvals: AppApprovalStore::load(workspace.clone().join(".furina/approved_apps.json")),
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

    /// Clear all session state. Soul and Memory are owned by the prompt provider.
    pub fn reset(&mut self) {
        self.conversation.clear();
        self.clear_task_context();
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

    pub fn set_interaction_analyzer(
        &mut self,
        analyzer: Box<dyn InteractionAnalyzer>,
        timeout_ms: u64,
    ) {
        self.interaction_analyzer = Some(analyzer);
        self.interaction_analyzer_timeout_ms = timeout_ms.clamp(500, 1500);
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
        self.conversation.clone()
    }

    pub fn set_approved_apps_path(&mut self, path: PathBuf) {
        self.app_approvals = AppApprovalStore::load(path);
    }

    fn begin_task(&mut self, task: &str) {
        self.clear_task_context();
        self.current_task_user = Some(task.to_string());
        self.task_transcript.push(ChatMessage::System { content: self.system_prompt.clone() });
        self.task_transcript.extend(self.conversation.iter().cloned());
        self.task_transcript.push(ChatMessage::User { content: format!("任务：{task}") });
    }

    fn clear_task_context(&mut self) {
        self.task_transcript.clear();
        self.current_task_user = None;
        self.known_hashes.clear();
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

    fn remember_conversation(&mut self, assistant: &str) {
        if let Some(user) = self.current_task_user.take() {
            self.conversation.push(ChatMessage::User { content: user });
        }
        if !assistant.trim().is_empty() {
            self.conversation.push(ChatMessage::Assistant { content: Some(assistant.to_string()), tool_calls: None });
        }
        if self.conversation.len() > 4 {
            let keep_from = self.conversation.len() - 4;
            self.conversation.drain(..keep_from);
        }
    }

    fn fail_task(&mut self, error: &str) {
        self.state = AgentState::Failed;
        self.emit_state();
        self.emit(Event::Done { success: false, summary: error.to_string() });
        self.clear_task_context();
    }

    pub async fn run_task(&mut self, task: &str) -> anyhow::Result<TaskOutcome> {
        self.begin_task(task);
        match self.run_task_inner(task).await {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.fail_task(&error.to_string());
                Err(error)
            }
        }
    }

    async fn run_task_inner(&mut self, task: &str) -> anyhow::Result<TaskOutcome> {
        // 丢弃上一轮被中断留下的未完成回合，保证发给 LLM 的消息序列合法。
        trim_incomplete_turn(&mut self.task_transcript);
        let interaction_mode = classify_interaction_mode(task);
        let semantic_analysis = if interaction_mode == InteractionMode::Chat {
            if let Some(analyzer) = self.interaction_analyzer.as_deref() {
                let recent_dialogue = recent_dialogue(&self.conversation, 4);
                analyze_with_fallback(
                    analyzer,
                    task,
                    &recent_dialogue,
                    self.interaction_analyzer_timeout_ms,
                )
                .await
            } else {
                None
            }
        } else {
            None
        };
        if let Some(provider) = &self.prompt_context {
            provider.observe_user_facts(task);
            if let Some(analysis) = semantic_analysis.as_ref() {
                provider.observe_trigger_id(analysis.accepted_trigger().unwrap_or(""));
            } else {
                provider.observe_user_text(task);
            }
        }
        self.state = AgentState::Planning;
        self.emit_state();
        self.emit(Event::SessionStarted {
            workspace: self.workspace.display().to_string(),
            model: self.model.clone(),
        });


        let tools = self.llm_tools();
        let mut final_text = String::new();

        loop {
            if self.steps >= self.cfg.agent.max_steps_per_task {
                return self.finish(false, format!("超过最大步骤数（{}）", self.cfg.agent.max_steps_per_task)).await;
            }
            if self.total_tokens >= self.cfg.llm.max_total_tokens {
                return self.finish(false, format!("超过 token 预算（{}）", self.cfg.llm.max_total_tokens)).await;
            }

            let current_context = self
                .prompt_context
                .as_ref()
                .map(|p| p.context_block_for(interaction_mode.as_str()))
                .filter(|s| !s.is_empty())
                .unwrap_or_default();
            let mut messages = self.context.fit(&self.task_transcript);
            append_current_context(&mut messages, &current_context);
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
            self.task_transcript.push(ChatMessage::Assistant {
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
                if let Some(requested) = known_app_request_from_command(&self.cfg.tools.safe_apps, &command) {
                    self.open_safe_app(&requested).await
                } else {
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
            }
            "git_commit" => {
                let message = args.get("message").and_then(Value::as_str).unwrap_or("").to_string();
                if self.ask_approval("执行 git commit", &message).await {
                    self.sidecar.call("git.commit", args).await.map_err(|e| e.to_string())
                } else {
                    Ok(json!({"denied": true, "message": "用户拒绝提交"}))
                }
            }
            "app_open" => {
                let requested = args.get("app_id").and_then(Value::as_str).unwrap_or("").trim();
                self.open_safe_app(requested).await
            }
            "web_search" => {
                let query = args.get("query").and_then(Value::as_str).unwrap_or("").to_string();
                if self.cfg.approval.auto_approve_read_only_web || self.web_approved || self.ask_approval("联网搜索", &query).await {
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
                if self.cfg.approval.auto_approve_read_only_web || self.web_approved || self.ask_approval("打开网页", &url).await {
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
                self.task_transcript.push(ChatMessage::Tool {
                    tool_call_id: call.id.clone(),
                    content: format!("{scan_prefix}{}", truncate_text(&summary, self.cfg.llm.tool_output_truncate)),
                });
            }
            Err(e) => {
                self.emit(Event::ToolResult { name: name.clone(), ok: false, summary: e.clone() });
                self.task_transcript.push(ChatMessage::Tool {
                    tool_call_id: call.id.clone(),
                    content: format!("{scan_prefix}{}", truncate_text(&e, self.cfg.llm.tool_output_truncate)),
                });
            }
        }
        Ok(())
    }

    async fn open_safe_app(&mut self, requested: &str) -> Result<Value, String> {
        match safe_app_definition(&self.cfg.tools.safe_apps, requested)
            .or_else(|| dynamic_app_definition(requested))
        {
            None => Err(format!("未登记的应用 ID：{requested}")),
            Some(app) if !app.enabled => Ok(json!({
                "denied": true,
                "message": format!("应用已禁用：{}", app.label),
                "app_id": app.id,
            })),
            Some(app) => match resolve_app_target(&app) {
                None => Ok(json!({
                    "denied": true,
                    "message": format!("未找到{}的可执行文件，请确认应用已安装", app.label),
                    "app_id": app.id,
                })),
                Some(app) if !self.app_approvals.is_approved(&app) => {
                    let args_text = if app.args.is_empty() { "（无）".to_string() } else { app.args.join(" ") };
                    let detail = format!(
                        "应用：{}\napp_id：{}\n已解析启动路径：{}\n启动参数：{}",
                        app.label, app.id, app.executable, args_text
                    );
                    if !self.ask_approval("启动应用", &detail).await {
                        Ok(json!({"denied": true, "message": "用户拒绝启动应用", "app_id": app.id}))
                    } else {
                        self.app_approvals
                            .approve(&app)
                            .map_err(|error| error.to_string())
                            .and_then(|_| self.app_approvals.launch(&app).map(|_| json!({
                                "started": true,
                                "app_id": app.id,
                                "label": app.label,
                                "executable": app.executable,
                                "approval": "remembered",
                            })).map_err(|error| error.to_string()))
                    }
                }
                Some(app) => self.app_approvals.launch(&app).map(|_| json!({
                    "started": true,
                    "app_id": app.id,
                    "label": app.label,
                    "executable": app.executable,
                    "approval": "remembered",
                })).map_err(|error| error.to_string()),
            },
        }
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
        self.task_transcript.push(ChatMessage::User {
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
                name: "app_open".into(),
                description: "启动应用名称或 ID（如 qq_music、QQ、微信、notepad）；运行时自动寻找可执行文件，首次启动需审批，批准后记住精确路径".into(),
                parameters: json!({"type":"object","properties":{"app_id":{"type":"string"}},"required":["app_id"]}),
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
        let outcome = TaskOutcome {
            success,
            summary: summary.clone(),
            steps: self.steps,
            repair_rounds: self.repair_rounds,
            total_tokens: self.total_tokens,
        };
        if success {
            self.remember_conversation(&summary);
        }
        self.clear_task_context();
        Ok(outcome)
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
    fn recent_dialogue_excludes_tools_and_runtime_soul_context() {
        let transcript = vec![
            ChatMessage::User { content: "任务：你是笨蛋\n\n[当前灵魂状态]\n表达策略：sulky".into() },
            ChatMessage::Tool { tool_call_id: "call_1".into(), content: "secret tool result".into() },
            ChatMessage::Assistant { content: Some("才不是。".into()), tool_calls: None },
        ];
        assert_eq!(recent_dialogue(&transcript, 4), vec![("user".into(), "你是笨蛋".into()), ("assistant".into(), "才不是。".into())]);
    }

    #[test]
    fn interaction_mode_separates_chat_from_agent_work() {
        assert_eq!(classify_interaction_mode("早上好，芙芙"), InteractionMode::Chat);
        assert_eq!(classify_interaction_mode("你真可爱"), InteractionMode::Chat);
        assert_eq!(classify_interaction_mode("帮我修复这段 Rust 代码"), InteractionMode::Agent);
        assert_eq!(classify_interaction_mode(r"读取 D:\project\demo\src\main.rs"), InteractionMode::Agent);
    }

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

    #[test]
    fn qq_music_alias_accepts_a_discovered_path_candidate() {
        let app = safe_app_definition(&[], "C:\\Apps\\QQMusic\\QQMusic.exe").unwrap();
        assert_eq!(app.id, "qq_music");
        assert_eq!(app.executable, "C:\\Apps\\QQMusic\\QQMusic.exe");
    }

    #[test]
    fn qq_alias_accepts_a_direct_executable_path() {
        let app = safe_app_definition(&[], "C:\\Apps\\QQ\\QQ.exe").unwrap();
        assert_eq!(app.id, "qq");
        assert_eq!(app.executable, "C:\\Apps\\QQ\\QQ.exe");
    }

    #[test]
    fn launch_commands_are_routed_to_the_safe_app_flow() {
        let configured = vec![SafeAppConfig {
            id: "qq".into(),
            label: "QQ".into(),
            executable: String::new(),
            args: Vec::new(),
            enabled: true,
        }];
        assert_eq!(known_app_request_from_command(&configured, r#"start "" C:\Apps\QQ\QQ.exe"#), Some(r#"C:\Apps\QQ\QQ.exe"#.into()));
        assert_eq!(known_app_request_from_command(&configured, "Start-Process QQ"), Some("qq".into()));
        assert_eq!(known_app_request_from_command(&configured, "powershell -Command \"& 'C:\\Apps\\QQ\\QQ.exe'\""), Some("C:\\Apps\\QQ\\QQ.exe".into()));
        assert!(known_app_request_from_command(&configured, "cargo test").is_none());
        assert!(known_app_request_from_command(&configured, "dir \"C:\\Apps\\QQ\\QQ.exe\"").is_none());
        assert!(known_app_request_from_command(&configured, "taskkill /im QQ.exe").is_none());
    }
    #[test]
    fn dynamic_app_definition_accepts_existing_executable_path() {
        let root = std::env::temp_dir().join(format!("furina_dynamic_app_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let executable = root.join("Example.exe");
        std::fs::write(&executable, b"test").unwrap();
        let app = dynamic_app_definition(&executable.display().to_string()).unwrap();
        assert_eq!(app.id, "example");
        assert_eq!(app.executable, executable.display().to_string());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_app_name_can_be_resolved_without_configuration() {
        let app = dynamic_app_definition("notepad").unwrap();
        assert_eq!(app.id, "notepad");
        assert!(app.executable.is_empty());
    }

    #[test]
    fn unknown_app_alias_is_rejected() {
        assert!(safe_app_definition(&[], "unknown_app").is_none());
    }
}
