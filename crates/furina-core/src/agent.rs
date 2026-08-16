//! The agent loop: scan → plan → approve → execute → verify → repair,
//! with hard stop conditions and a structured event stream.

use crate::audit::AuditLog;
use crate::config::{Config, SafeAppConfig};
use crate::app_launcher::{resolve_app_target, AppApprovalStore};
use crate::experience::{AgentExperienceStore, TaskTrace};
use crate::context::{strip_runtime_soul_context, truncate_text, trim_incomplete_turn, ContextManager};
use crate::gateway::{escapes_workspace, is_private_path, ActionKind, PermissionGateway, Verdict};
use crate::interaction::{analyze_with_fallback, InteractionAnalyzer};
use crate::llm::LlmClient;
use crate::self_inspect::{SelfChangeInput, SelfInspector};
use crate::sidecar::Sidecar;
use crate::state::AgentState;
use crate::task_journal::{sanitize_text as sanitize_journal_text, CompletedAction, TaskCheckpointRecord, TaskJournalStore, TaskRecoverySummary, TASK_JOURNAL_SCHEMA_VERSION};
use crate::web::WebClient;
use crate::web_cache::{WebCache, WebCacheEntry};
use async_trait::async_trait;
use furina_proto::{ChatMessage, Event, ScanResult, TaskOutcome, ToolCall, ToolSpec};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::hash::{Hash, Hasher};
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
    self_inspector: Option<SelfInspector>,
    experience_store: Option<AgentExperienceStore>,
    task_journal: Option<TaskJournalStore>,
    audit_log: Option<AuditLog>,
    context: ContextManager,
    conversation: Vec<ChatMessage>,
    task_transcript: Vec<ChatMessage>,
    current_task_user: Option<String>,
    current_task_id: Option<String>,
    completed_actions: HashMap<String, CompletedAction>,
    resumed_from_checkpoint: bool,
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
    checkpoint_count: u32,
    token_budget_limit: u64,
    last_tool_signature: String,
    last_tool_result_digest: String,
    repeated_tool_calls: u32,
    progress_revision: u64,
    checkpoint_progress_revision: u64,
    stalled_checkpoints: u32,
    last_repair_reviewed: u32,
    tool_patterns: HashSet<String>,
    failure_evidence: Vec<String>,
    stop_reason: Option<String>,
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
            self_inspector: None,
            experience_store: None,
            task_journal: None,
            audit_log: None,
            context,
            conversation: Vec::new(),
            task_transcript: Vec::new(),
            current_task_user: None,
            current_task_id: None,
            completed_actions: HashMap::new(),
            resumed_from_checkpoint: false,
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
            checkpoint_count: 0,
            token_budget_limit: 0,
            last_tool_signature: String::new(),
            last_tool_result_digest: String::new(),
            repeated_tool_calls: 0,
            progress_revision: 0,
            checkpoint_progress_revision: 0,
            stalled_checkpoints: 0,
            last_repair_reviewed: 0,
            tool_patterns: HashSet::new(),
            failure_evidence: Vec::new(),
            stop_reason: None,
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
        self.checkpoint_count = 0;
        self.token_budget_limit = self.cfg.llm.max_total_tokens.max(1);
        self.last_tool_signature.clear();
        self.last_tool_result_digest.clear();
        self.repeated_tool_calls = 0;
        self.progress_revision = 0;
        self.checkpoint_progress_revision = 0;
        self.stalled_checkpoints = 0;
        self.last_repair_reviewed = 0;
        self.tool_patterns.clear();
        self.failure_evidence.clear();
        self.stop_reason = None;
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

    pub fn set_self_inspector(&mut self, inspector: SelfInspector) {
        self.self_inspector = Some(inspector);
    }

    pub fn set_experience_store(&mut self, store: AgentExperienceStore) {
        self.experience_store = Some(store);
    }

    pub fn set_task_journal(&mut self, store: TaskJournalStore) {
        self.task_journal = Some(store);
    }

    pub fn set_audit_log(&mut self, log: AuditLog) {
        self.audit_log = Some(log);
    }

    pub fn recoverable_task(&self) -> Option<TaskRecoverySummary> {
        self.task_journal.as_ref().and_then(TaskJournalStore::summary)
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

    fn persist_task_snapshot(&self, status: &str, summary: &str) {
        let Some(store) = &self.task_journal else { return };
        let Some(task_id) = &self.current_task_id else { return };
        let Some(goal) = &self.current_task_user else { return };
        let mut completed_actions = self.completed_actions.values().cloned().collect::<Vec<_>>();
        completed_actions.sort_by(|left, right| left.fingerprint.cmp(&right.fingerprint));
        let record = TaskCheckpointRecord {
            schema_version: TASK_JOURNAL_SCHEMA_VERSION,
            task_id: task_id.clone(),
            original_goal: sanitize_journal_text(goal, 4_000),
            status: status.to_string(),
            checkpoint_count: self.checkpoint_count,
            steps: self.steps,
            total_tokens: self.total_tokens,
            repair_rounds: self.repair_rounds,
            token_budget_limit: self.token_budget_limit,
            summary: sanitize_journal_text(summary, 2_000),
            blocking: sanitize_journal_text(self.failure_evidence.last().map(String::as_str).unwrap_or("无明确阻塞"), 500),
            wrote_files: self.wrote_files,
            verified: self.verified,
            test_command: sanitize_journal_text(&self.test_command, 500),
            scanned: self.scanned,
            known_hashes: self.known_hashes.iter().map(|(path, hash)| (path.clone(), hash.clone())).collect::<BTreeMap<_, _>>(),
            completed_actions,
            failure_evidence: self.failure_evidence.iter().map(|item| sanitize_journal_text(item, 500)).collect(),
            tool_patterns: self.tool_patterns.iter().cloned().collect(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            updated_at_ms: now_ms(),
        };
        let _ = store.save(&record);
    }

    fn begin_task(&mut self, task: &str) {
        self.clear_task_context();
        self.current_task_id = Some(format!("task_{}", now_ms()));
        self.current_task_user = Some(task.to_string());
        self.task_transcript.push(ChatMessage::System { content: self.system_prompt.clone() });
        self.task_transcript.extend(self.conversation.iter().cloned());
        self.task_transcript.push(ChatMessage::User { content: format!("任务：{task}") });
        self.persist_task_snapshot("active", "任务已开始，尚未执行工具");
    }

    fn restore_task(&mut self, record: TaskCheckpointRecord) {
        self.clear_task_context();
        self.current_task_id = Some(record.task_id);
        self.current_task_user = Some(record.original_goal.clone());
        self.task_transcript.push(ChatMessage::System { content: self.system_prompt.clone() });
        self.task_transcript.extend(self.conversation.iter().cloned());
        self.task_transcript.push(ChatMessage::User { content: format!("任务：{}", record.original_goal) });
        self.task_transcript.push(ChatMessage::User { content: format!(
            "[恢复检查点 #{}]
{}
当前阻塞：{}
请先检查当前文件与运行状态，不要重复已经成功的写入、命令、提交或应用启动。",
            record.checkpoint_count, record.summary, record.blocking,
        ) });
        self.known_hashes = record.known_hashes.into_iter().collect();
        self.completed_actions = record.completed_actions.into_iter().map(|action| (action.fingerprint.clone(), action)).collect();
        self.repair_rounds = record.repair_rounds;
        self.steps = record.steps;
        self.total_tokens = record.total_tokens;
        self.wrote_files = record.wrote_files;
        self.verified = record.verified;
        self.test_command = record.test_command;
        self.scanned = record.scanned;
        self.checkpoint_count = record.checkpoint_count;
        self.token_budget_limit = record.token_budget_limit.max(self.cfg.llm.max_total_tokens.max(1));
        self.progress_revision = self.steps as u64;
        self.checkpoint_progress_revision = self.progress_revision;
        self.tool_patterns = record.tool_patterns.into_iter().collect();
        self.failure_evidence = record.failure_evidence;
        self.resumed_from_checkpoint = true;
        self.persist_task_snapshot("active", "任务已从持久化检查点恢复");
    }

    fn clear_task_context(&mut self) {
        self.task_transcript.clear();
        self.current_task_user = None;
        self.current_task_id = None;
        self.completed_actions.clear();
        self.resumed_from_checkpoint = false;
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
        self.checkpoint_count = 0;
        self.token_budget_limit = self.cfg.llm.max_total_tokens.max(1);
        self.last_tool_signature.clear();
        self.last_tool_result_digest.clear();
        self.repeated_tool_calls = 0;
        self.progress_revision = 0;
        self.checkpoint_progress_revision = 0;
        self.stalled_checkpoints = 0;
        self.last_repair_reviewed = 0;
        self.tool_patterns.clear();
        self.failure_evidence.clear();
        self.stop_reason = None;
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
        self.stop_reason = Some("error".into());
        self.persist_task_snapshot("interrupted", error);
        self.record_experience(false, error, None);
        self.state = AgentState::Failed;
        self.emit_state();
        self.emit(Event::Done { success: false, summary: error.to_string() });
        self.clear_task_context();
    }

    pub async fn run_task(&mut self, task: &str) -> anyhow::Result<TaskOutcome> {
        if is_resume_request(task) {
            if let Some(record) = self.task_journal.as_ref().and_then(TaskJournalStore::load) {
                let detail = format!(
                    "检测到未完成任务：{}\n状态：{}，检查点 {}，已执行 {} 步。\n恢复时会阻止重复执行已成功的非幂等动作。\n\n是否恢复？",
                    sanitize_journal_text(&record.original_goal, 300), record.status, record.checkpoint_count, record.steps,
                );
                self.current_task_id = Some(record.task_id.clone());
                if self.ask_approval("恢复上次任务", &detail).await {
                    let goal = record.original_goal.clone();
                    self.restore_task(record);
                    self.emit(Event::TaskRecoveryResumed {
                        task_id: self.current_task_id.clone().unwrap_or_default(),
                        checkpoint_count: self.checkpoint_count,
                        steps: self.steps,
                    });
                    return match self.run_task_inner(&goal).await {
                        Ok(outcome) => Ok(outcome),
                        Err(error) => {
                            self.fail_task(&error.to_string());
                            Err(error)
                        }
                    };
                }
                if let Some(store) = &self.task_journal {
                    let _ = store.clear();
                }
                self.emit(Event::TaskRecoveryDiscarded { task_id: record.task_id });
                self.clear_task_context();
                let summary = "已放弃上次未完成任务".to_string();
                self.state = AgentState::Done;
                self.emit_state();
                self.emit(Event::Done { success: false, summary: summary.clone() });
                return Ok(TaskOutcome {
                    success: false,
                    summary,
                    steps: 0,
                    repair_rounds: 0,
                    total_tokens: 0,
                    checkpoint_count: 0,
                    stop_reason: Some("recovery_declined".into()),
                });
            }
        }
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
            if self.total_tokens >= self.token_budget_limit {
                if !self.checkpoint("token_budget", true).await? {
                    return self.finish_with_reason(
                        false,
                        "用户选择在 token 预算检查点停止任务".into(),
                        Some("budget_declined".into()),
                    ).await;
                }
                self.token_budget_limit = self
                    .token_budget_limit
                    .saturating_add(self.cfg.llm.max_total_tokens.max(1));
                self.task_transcript.push(ChatMessage::User {
                    content: format!("[预算已扩展] 新的累计 token 预算为 {}，请继续任务。", self.token_budget_limit),
                });
            }

            let current_context = self
                .prompt_context
                .as_ref()
                .map(|provider| provider.context_block_for(interaction_mode.as_str()))
                .filter(|context| !context.is_empty())
                .unwrap_or_default();
            let experience_context = if self.cfg.agent.experience_learning_enabled {
                self.experience_store
                    .as_ref()
                    .map(|store| store.context_for(task, 3, 2_000))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let combined_context = [current_context, experience_context]
                .into_iter()
                .filter(|context| !context.is_empty())
                .collect::<Vec<_>>()
                .join("

");
            let mut messages = self.context.fit(&self.task_transcript);
            append_current_context(&mut messages, &combined_context);
            let events = self.events.clone();
            let resp = {
                let mut on_delta = |chunk: &str| {
                    if !chunk.is_empty() {
                        events.emit(Event::MessageDelta { content: chunk.to_string() });
                    }
                };
                self.llm.complete_stream(&messages, &tools, &mut on_delta).await?
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
                if self.wrote_files && !self.verified && !self.test_command.is_empty() {
                    let test_command = self.test_command.clone();
                    self.run_verification(&test_command).await?;
                    if self.repair_review_due() && !self.checkpoint("repair_review", false).await? {
                        return self.finish_with_reason(
                            false,
                            "连续验证失败后用户选择停止任务".into(),
                            Some("repair_review_declined".into()),
                        ).await;
                    }
                    if !self.verified {
                        continue;
                    }
                }
                return self.finish(true, final_text).await;
            }

            if !self.scanned {
                self.scan_project().await?;
            }

            for call in &resp.tool_calls {
                self.steps = self.steps.saturating_add(1);
                self.handle_tool_call(call).await?;

                let repeated_limit = self.cfg.agent.max_repeated_tool_calls.max(2);
                if self.repeated_tool_calls >= repeated_limit {
                    self.task_transcript.push(ChatMessage::User {
                        content: "[停滞检测] 相同工具调用及结果已重复出现。不要再次执行同一动作，请重新分析假设并选择不同路径。".into(),
                    });
                    if !self.checkpoint("repeated_tool_call", false).await? {
                        return self.finish_with_reason(
                            false,
                            "检测到重复工具死循环，用户选择停止任务".into(),
                            Some("stalled".into()),
                        ).await;
                    }
                    self.repeated_tool_calls = 0;
                } else {
                    let interval = self.cfg.agent.checkpoint_interval_steps.max(1);
                    if self.steps % interval == 0 && !self.checkpoint("step_interval", false).await? {
                        return self.finish_with_reason(
                            false,
                            "长任务停滞检查点被用户终止".into(),
                            Some("stalled".into()),
                        ).await;
                    }
                }

                if self.repair_review_due() && !self.checkpoint("repair_review", false).await? {
                    return self.finish_with_reason(
                        false,
                        "连续验证失败后用户选择停止任务".into(),
                        Some("repair_review_declined".into()),
                    ).await;
                }
            }
        }
    }

    fn repair_review_due(&mut self) -> bool {
        let interval = self.cfg.agent.repair_review_after.max(1);
        if self.repair_rounds >= self.last_repair_reviewed.saturating_add(interval) {
            self.last_repair_reviewed = self.repair_rounds;
            true
        } else {
            false
        }
    }

    async fn checkpoint(&mut self, reason: &str, force_review: bool) -> anyhow::Result<bool> {
        self.checkpoint_count = self.checkpoint_count.saturating_add(1);
        let progressed = self.progress_revision > self.checkpoint_progress_revision;
        if progressed {
            self.stalled_checkpoints = 0;
        } else {
            self.stalled_checkpoints = self.stalled_checkpoints.saturating_add(1);
        }
        self.checkpoint_progress_revision = self.progress_revision;
        let goal = self.current_task_user.as_deref().unwrap_or("当前任务");
        let blocking = self.failure_evidence.last().cloned().unwrap_or_else(|| "无明确阻塞".into());
        let summary = format!(
            "原始目标：{}
已完成：执行 {} 次工具调用，累计 {} tokens，修复 {} 轮
待处理：继续完成原始目标并验证结果
当前阻塞：{}
文件变化：{}
验证状态：{}
下一步：基于最新证据重新规划，避免重复无效动作",
            truncate_text(goal, 500),
            self.steps,
            self.total_tokens,
            self.repair_rounds,
            truncate_text(&blocking, 500),
            if self.wrote_files { "已产生变更" } else { "尚未修改文件" },
            if self.verified { "已通过" } else { "尚未通过" },
        );
        self.emit(Event::Checkpoint {
            sequence: self.checkpoint_count,
            steps: self.steps,
            tokens: self.total_tokens,
            reason: reason.to_string(),
            summary: summary.clone(),
        });
        let compact = self.context.fit(&self.task_transcript);
        self.task_transcript = compact;
        trim_incomplete_turn(&mut self.task_transcript);
        self.task_transcript.push(ChatMessage::User {
            content: format!("[任务检查点 #{} · {}]
{}", self.checkpoint_count, reason, summary),
        });
        self.persist_task_snapshot("checkpoint", &summary);

        let stalled_limit = self.cfg.agent.max_stalled_checkpoints.max(1);
        if force_review || self.stalled_checkpoints >= stalled_limit {
            let detail = format!(
                "任务已运行 {} 步、使用 {} tokens。
{}

是否允许芙芙重新规划并继续？",
                self.steps, self.total_tokens, summary,
            );
            if !self.ask_approval("继续长任务", &detail).await {
                self.persist_task_snapshot("paused", &summary);
                self.stop_reason = Some(if force_review { reason.into() } else { "stalled".into() });
                return Ok(false);
            }
            self.stalled_checkpoints = 0;
            self.task_transcript.push(ChatMessage::User {
                content: "[用户已批准继续] 请基于检查点重新规划，优先选择能产生新证据的动作。".into(),
            });
        }
        Ok(true)
    }

    fn observe_tool_result(&mut self, name: &str, arguments: &str, ok: bool, summary: &str) {
        self.tool_patterns.insert(name.to_string());
        let signature = format!("{}:{}", name, normalize_tool_arguments(arguments));
        let digest = stable_digest(summary);
        if signature == self.last_tool_signature && digest == self.last_tool_result_digest {
            self.repeated_tool_calls = self.repeated_tool_calls.saturating_add(1);
        } else {
            self.repeated_tool_calls = 1;
            self.progress_revision = self.progress_revision.saturating_add(1);
            self.last_tool_signature = signature;
            self.last_tool_result_digest = digest;
        }
        if !ok {
            self.failure_evidence.push(format!("{}: {}", name, truncate_text(summary, 500)));
            if self.failure_evidence.len() > 20 {
                self.failure_evidence.remove(0);
            }
        }
    }

    async fn reflect_on_task(&mut self, success: bool, summary: &str) -> Option<String> {
        let significant = !success || self.repair_rounds > 0 || self.checkpoint_count > 0;
        if !significant { return None; }
        let task = redact_reflection_text(self.current_task_user.as_deref().unwrap_or(""), 500);
        let summary = redact_reflection_text(summary, 500);
        let failures = self.failure_evidence.iter().take(8)
            .map(|evidence| redact_reflection_text(evidence, 300)).collect::<Vec<_>>().join("\n- ");
        let tools = self.tool_patterns.iter().cloned().collect::<Vec<_>>().join(", ");
        let messages = vec![
            ChatMessage::System { content: "你是 Agent 任务复盘器。只输出 JSON，不调用工具。格式：{\"lesson\":\"可复用且具体的经验\"}。不得包含密钥、完整源码或原始长输出。".into() },
            ChatMessage::User { content: format!(
                "任务：{}\n结果：{}\n摘要：{}\n工具：{}\n修复轮数：{}\n检查点：{}\n失败证据：\n- {}",
                task, if success { "成功" } else { "失败" }, summary, tools,
                self.repair_rounds, self.checkpoint_count, failures,
            ) },
        ];
        let response = self.llm.complete(&messages, &[]).await.ok()?;
        self.total_tokens = self.total_tokens.saturating_add(response.prompt_tokens + response.completion_tokens);
        self.emit(Event::Tokens {
            prompt: response.prompt_tokens,
            completion: response.completion_tokens,
            total: self.total_tokens,
        });
        let content = response.content?;
        let start = content.find('{')?;
        let end = content.rfind('}')?;
        let value: Value = serde_json::from_str(&content[start..=end]).ok()?;
        value.get("lesson").and_then(Value::as_str)
            .map(|lesson| redact_reflection_text(lesson, 800))
            .filter(|lesson| !lesson.trim().is_empty())
    }

    fn record_experience(&mut self, success: bool, summary: &str, lesson_override: Option<String>) {
        if !self.cfg.agent.experience_learning_enabled || self.steps == 0 {
            return;
        }
        let trace = TaskTrace {
            task: self.current_task_user.clone().unwrap_or_default(),
            success,
            summary: truncate_text(summary, 500),
            tool_patterns: self.tool_patterns.iter().cloned().collect(),
            failure_evidence: self.failure_evidence.clone(),
            repair_rounds: self.repair_rounds,
            checkpoint_count: self.checkpoint_count,
            lesson_override,
        };
        let recorded = self.experience_store.as_mut().and_then(|store| {
            store.record(trace).ok().map(|record| {
                let candidate = store.proposal_candidate(&record);
                (record, candidate)
            })
        });
        let Some((record, candidate)) = recorded else { return };
        self.emit(Event::ExperienceLearned { id: record.id.clone(), summary: record.lesson.clone() });
        if candidate && self.cfg.agent.self_change_proposals_enabled {
            if let Some(inspector) = self.self_inspector.clone() {
                let input = SelfChangeInput {
                    problem: format!("重复出现的 Agent 失败模式：{}", record.lesson),
                    evidence: record.failure_evidence.clone(),
                    changes: Vec::new(),
                    config_updates: std::collections::BTreeMap::new(),
                    tests: Vec::new(),
                    risk: "这是证据触发的诊断提案，尚未包含可应用源码变更。".into(),
                    rollback: "未修改任何自身文件。".into(),
                };
                if let Ok(proposal) = inspector.create_proposal(input) {
                    self.emit(Event::SelfChangeProposed {
                        id: proposal.id,
                        summary: proposal.problem,
                        targets: Vec::new(),
                        applicable: false,
                    });
                }
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
        let raw_arguments = call.function.arguments.clone();
        let args: Value = serde_json::from_str(&raw_arguments).unwrap_or(Value::Null);
        let scan_prefix = self.pending_scan.take().unwrap_or_default();
        self.emit(Event::ToolCall { name: name.clone(), summary: args.to_string() });
        self.state = AgentState::Executing;
        self.emit_state();

        let action_fingerprint = action_fingerprint(&name, &raw_arguments);
        let replayed_action = if self.resumed_from_checkpoint && is_non_idempotent_tool(&name) {
            self.completed_actions.get(&action_fingerprint).cloned()
        } else {
            None
        };
        let result: Result<Value, String> = if let Some(action) = replayed_action {
            Ok(json!({
                "already_completed": true,
                "tool": action.tool,
                "message": format!("恢复任务时跳过已成功执行的动作；请检查当前状态后决定下一步。摘要：{}", action.summary),
            }))
        } else {
            match name.as_str() {
            "self_status" => self
                .self_inspector
                .as_ref()
                .ok_or_else(|| "自身检查未配置".to_string())
                .map(SelfInspector::status),
            "self_read_source" => {
                let path = args.get("path").and_then(Value::as_str).unwrap_or("");
                let max_bytes = args.get("max_bytes").and_then(Value::as_u64).unwrap_or(60_000) as usize;
                self.self_inspector
                    .as_ref()
                    .ok_or_else(|| "自身检查未配置".to_string())
                    .and_then(|inspector| inspector.read_source(path, max_bytes).map_err(|error| error.to_string()))
            }
            "self_search_source" => {
                let path = args.get("path").and_then(Value::as_str).unwrap_or("");
                let pattern = args.get("pattern").and_then(Value::as_str).unwrap_or("");
                let regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize;
                self.self_inspector
                    .as_ref()
                    .ok_or_else(|| "自身检查未配置".to_string())
                    .and_then(|inspector| inspector.search_source(path, pattern, regex, limit).map_err(|error| error.to_string()))
            }
            "self_propose_change" => {
                async {
                    let inspector = self.self_inspector.clone().ok_or_else(|| "自身改进提案未配置".to_string())?;
                    let input: SelfChangeInput = serde_json::from_value(args).map_err(|error| error.to_string())?;
                    let mut proposal = inspector.create_proposal(input).map_err(|error| error.to_string())?;
                    let targets = proposal.changes.iter().map(|change| change.path.clone()).collect::<Vec<_>>();
                    self.emit(Event::SelfChangeProposed {
                        id: proposal.id.clone(),
                        summary: proposal.problem.clone(),
                        targets,
                        applicable: proposal.applicable,
                    });
                    if !proposal.applicable {
                        Ok(json!({"proposal_id": proposal.id, "exported": true, "applicable": false}))
                    } else {
                        for command in &proposal.tests {
                            if let Verdict::Block(reason) = self.gateway.check(&ActionKind::RunCommand { command: command.clone() }) {
                                inspector.mark_proposal_status(&mut proposal, "blocked").map_err(|error| error.to_string())?;
                                return Err(reason);
                            }
                        }
                        let detail = inspector.proposal_approval_detail(&proposal);
                        if !self.ask_approval("应用自身改进提案", &detail).await {
                            inspector.mark_proposal_status(&mut proposal, "rejected").map_err(|error| error.to_string())?;
                            Ok(json!({"proposal_id": proposal.id, "denied": true, "message": "用户拒绝应用自身改进提案"}))
                        } else {
                            match inspector.apply_proposal(&mut proposal).await {
                                Ok(summary) => {
                                    self.emit(Event::SelfChangeApplied { id: proposal.id.clone(), success: true, summary: summary.clone() });
                                    Ok(json!({"proposal_id": proposal.id, "applied": true, "summary": summary}))
                                }
                                Err(error) => {
                                    let summary = error.to_string();
                                    self.emit(Event::SelfChangeApplied { id: proposal.id.clone(), success: false, summary: summary.clone() });
                                    Err(summary)
                                }
                            }
                        }
                    }
                }.await
            }
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
            }
        };

        match result {
            Ok(v) => {
                let denied = v.get("denied").is_some();
                let blocked = v.get("blocked").is_some();
                let ok = !denied && !blocked;
                let summary = summarize_value(&v);
                let already_completed = v.get("already_completed").is_some();
                self.emit(Event::ToolResult { name: name.clone(), ok, summary: summary.clone() });
                self.observe_tool_result(&name, &raw_arguments, ok, &summary);
                if (name == "fs_write_file" || name == "fs_create_file") && ok && !already_completed {
                    self.wrote_files = true;
                }
                if ok && !already_completed && is_non_idempotent_tool(&name) {
                    self.completed_actions.insert(action_fingerprint.clone(), CompletedAction {
                        fingerprint: action_fingerprint.clone(),
                        tool: name.clone(),
                        result_fingerprint: stable_digest(&summary),
                        summary: sanitize_journal_text(&summary, 500),
                        completed_at_ms: now_ms(),
                    });
                }
                self.task_transcript.push(ChatMessage::Tool {
                    tool_call_id: call.id.clone(),
                    content: format!("{scan_prefix}{}", truncate_text(&summary, self.cfg.llm.tool_output_truncate)),
                });
                self.persist_task_snapshot("active", &summary);
            }
            Err(e) => {
                self.emit(Event::ToolResult { name: name.clone(), ok: false, summary: e.clone() });
                self.observe_tool_result(&name, &raw_arguments, false, &e);
                self.task_transcript.push(ChatMessage::Tool {
                    tool_call_id: call.id.clone(),
                    content: format!("{scan_prefix}{}", truncate_text(&e, self.cfg.llm.tool_output_truncate)),
                });
                self.persist_task_snapshot("active", &e);
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
                self.progress_revision = self.progress_revision.saturating_add(1);
                self.emit(Event::Verify { passed: true, detail });
            } else {
                self.repair_rounds += 1;
                self.failure_evidence.push(format!("验证失败: {}", truncate_text(&detail, 500)));
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
            self.progress_revision = self.progress_revision.saturating_add(1);
            self.emit(Event::Verify { passed: true, detail: detail.clone() });
        } else {
            self.repair_rounds += 1;
            self.failure_evidence.push(format!("自动验证失败: {}", truncate_text(&detail, 500)));
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
        self.persist_task_snapshot("awaiting_approval", kind);
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
        self.persist_task_snapshot("active", if ok { "审批已通过" } else { "审批被拒绝" });
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
        let mut tools = vec![
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
        ];
        if self.cfg.agent.self_inspection_enabled && self.self_inspector.is_some() {
            tools.extend([
                ToolSpec {
                    name: "self_status".into(),
                    description: "检查 Furina 自身版本、运行模式、工具能力和脱敏配置；不会返回密钥、人格或记忆正文".into(),
                    parameters: json!({"type":"object"}),
                },
                ToolSpec {
                    name: "self_read_source".into(),
                    description: "开发模式下只读检查白名单内的 Furina 自身源码；安装模式不可用".into(),
                    parameters: json!({"type":"object","properties":{"path":{"type":"string"},"max_bytes":{"type":"integer"}},"required":["path"]}),
                },
                ToolSpec {
                    name: "self_search_source".into(),
                    description: "开发模式下在 Furina 自身源码白名单中搜索文本或正则；安装模式不可用".into(),
                    parameters: json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"regex":{"type":"boolean"},"limit":{"type":"integer"}},"required":["pattern"]}),
                },
            ]);
            if self.cfg.agent.self_change_proposals_enabled {
                tools.push(ToolSpec {
                    name: "self_propose_change".into(),
                    description: "创建自身源码改进提案。只能提交完整新文件内容；开发模式需用户审批并验证后应用，安装模式仅导出。禁止修改 Persona、Soul、密钥或记忆".into(),
                    parameters: json!({
                        "type":"object",
                        "properties":{
                            "problem":{"type":"string"},
                            "evidence":{"type":"array","items":{"type":"string"}},
                            "changes":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"expected_sha256":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}},
                            "config_updates":{"type":"object","additionalProperties":true},
                            "tests":{"type":"array","items":{"type":"string"}},
                            "risk":{"type":"string"},
                            "rollback":{"type":"string"}
                        },
                        "required":["problem","changes"]
                    }),
                });
            }
        }
        tools
    }

    fn emit(&self, event: Event) {
        if let Some(p) = &self.prompt_context {
            p.observe_event(&event);
        }
        if let Some(log) = &self.audit_log {
            let _ = log.record_event(self.current_task_id.as_deref(), &event);
        }
        self.events.emit(event);
    }

    fn emit_state(&self) {
        self.emit(Event::StateChanged { state: self.state.as_str().to_string() });
    }

    async fn finish(&mut self, success: bool, summary: String) -> anyhow::Result<TaskOutcome> {
        self.finish_with_reason(success, summary, None).await
    }

    async fn finish_with_reason(
        &mut self,
        success: bool,
        summary: String,
        stop_reason: Option<String>,
    ) -> anyhow::Result<TaskOutcome> {
        self.stop_reason = stop_reason.clone();
        let resumable = matches!(stop_reason.as_deref(), Some("budget_declined" | "stalled" | "repair_review_declined"));
        if resumable {
            self.persist_task_snapshot("paused", &summary);
        }
        let lesson = self.reflect_on_task(success, &summary).await;
        self.record_experience(success, &summary, lesson);
        self.state = if success { AgentState::Done } else { AgentState::Failed };
        self.emit_state();
        self.emit(Event::Done { success, summary: summary.clone() });
        let outcome = TaskOutcome {
            success,
            summary: summary.clone(),
            steps: self.steps,
            repair_rounds: self.repair_rounds,
            total_tokens: self.total_tokens,
            checkpoint_count: self.checkpoint_count,
            stop_reason,
        };
        if success {
            self.remember_conversation(&summary);
        }
        if !resumable {
            if let Some(store) = &self.task_journal {
                let _ = store.clear();
            }
        }
        self.clear_task_context();
        Ok(outcome)
    }
}

fn redact_reflection_text(text: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for line in text.lines() {
        let lower = line.to_lowercase();
        if ["api_key", "apikey", "secret", "password", "token="].iter().any(|needle| lower.contains(needle)) {
            output.push_str("[REDACTED]\n");
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    truncate_text(output.trim(), max_chars)
}

fn normalize_tool_arguments(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| arguments.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn stable_digest(value: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn action_fingerprint(name: &str, arguments: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    hasher.update(b":");
    hasher.update(normalize_tool_arguments(arguments).as_bytes());
    format!("{:x}", hasher.finalize())
}

fn is_non_idempotent_tool(name: &str) -> bool {
    matches!(name, "fs_write_file" | "fs_create_file" | "term_run" | "git_commit" | "app_open" | "self_propose_change")
}

fn is_resume_request(task: &str) -> bool {
    matches!(task.trim().to_lowercase().as_str(), "继续" | "恢复" | "继续上次任务" | "恢复上次任务" | "resume" | "continue")
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
