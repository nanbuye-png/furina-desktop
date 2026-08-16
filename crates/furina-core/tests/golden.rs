//! Golden integration tests: real Python sidecar + scripted LLM replay.
//! These run in CI whenever Python is available (skipped otherwise).

use furina_core::agent::{Agent, Approver, PromptContextProvider};
use furina_core::config::{Config, ProviderConfig};
use furina_core::llm::{FixtureLlm, LlmClient, LlmResponse};
use furina_core::sidecar::{EventSink, Sidecar, SidecarLaunch};
use furina_core::task_journal::{CompletedAction, TaskCheckpointRecord, TaskJournalStore, TASK_JOURNAL_SCHEMA_VERSION};
use furina_core::web_cache::WebCache;
use furina_proto::{ChatMessage, Event, ToolCall, ToolFunctionCall, ToolSpec};
use sha2::{Digest, Sha256};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct AutoApprove;
#[async_trait]
impl Approver for AutoApprove {
    async fn confirm(&mut self, _prompt: &str) -> bool {
        true
    }
}

struct DenyApprove;
#[async_trait]
impl Approver for DenyApprove {
    async fn confirm(&mut self, _prompt: &str) -> bool {
        false
    }
}

struct CountingApprover {
    count: Arc<Mutex<usize>>,
}

#[async_trait]
impl Approver for CountingApprover {
    async fn confirm(&mut self, _prompt: &str) -> bool {
        *self.count.lock().unwrap() += 1;
        true
    }
}

fn spawn_web_mock(response_body: String) -> String {
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
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });
    });
    rx.recv().unwrap()
}

/// 接受 N 次连接、每次返回相同响应的 mock。
fn spawn_web_mock_loop(response_body: String, n: usize) -> String {
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
            for _ in 0..n {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
    });
    rx.recv().unwrap()
}

struct CollectSink(Arc<Mutex<Vec<Event>>>);
impl EventSink for CollectSink {
    fn emit(&self, event: Event) {
        self.0.lock().unwrap().push(event);
    }
}

/// 记录每次发送给 LLM 的消息，并直接给出最终回复（不触发工具）。
struct RecordingLlm {
    seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

#[async_trait]
impl LlmClient for RecordingLlm {
    async fn complete(&self, messages: &[ChatMessage], _tools: &[ToolSpec]) -> anyhow::Result<LlmResponse> {
        self.seen.lock().unwrap().push(messages.to_vec());
        Ok(LlmResponse {
            content: Some("明白了。".into()),
            tool_calls: vec![],
            prompt_tokens: 1,
            completion_tokens: 1,
        })
    }
}

struct FixedContext;
impl PromptContextProvider for FixedContext {
    fn observe_user_text(&self, _text: &str) {}
    fn observe_event(&self, _event: &Event) {}
    fn context_block(&self) -> String {
        "[当前灵魂状态]\n当前情绪：calm，强度 0，趋势 stable\n关系：陌生（信任 20）".into()
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate path")
        .to_path_buf()
}

fn python_available() -> bool {
    std::process::Command::new("python")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn mvn_available() -> bool {
    std::process::Command::new("mvn")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn temp_fixture(name: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let tmp = std::env::temp_dir().join(format!("furina_golden_{}_{}_{}", std::process::id(), n, name));
    let _ = std::fs::remove_dir_all(&tmp);
    copy_dir(&repo_root().join("tests/fixtures").join(name), &tmp);
    tmp
}

fn temp_fixture_ws() -> PathBuf {
    temp_fixture("sample_py")
}

async fn spawn_sidecar(ws: &Path, sink: Arc<dyn EventSink>) -> Sidecar {
    let root = repo_root();
    Sidecar::spawn(
        &SidecarLaunch::Python {
            executable: "python".into(),
            python_path: root.join("python"),
        },
        &ws.display().to_string(),
        sink,
    )
    .await
    .expect("sidecar 启动失败")
}

fn write_call(content: &str) -> ToolCall {
    ToolCall {
        id: "call_w".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "fs_write_file".into(),
            arguments: serde_json::json!({"path": "calculator.py", "content": content}).to_string(),
        },
    }
}

fn test_call() -> ToolCall {
    ToolCall {
        id: "call_t".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "term_run".into(),
            arguments: serde_json::json!({"command": "python -m unittest"}).to_string(),
        },
    }
}

fn tool_fingerprint(name: &str, arguments: &str) -> String {
    let normalized = serde_json::from_str::<serde_json::Value>(arguments)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| arguments.split_whitespace().collect::<Vec<_>>().join(" "));
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    hasher.update(b":");
    hasher.update(normalized.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[tokio::test]
async fn golden_repair_loop_fixes_calculator() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let ws = temp_fixture_ws();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let transcript = repo_root().join("tests/golden/golden_transcript.json");
    let llm: Box<dyn LlmClient> = Box::new(FixtureLlm::from_json_file(&transcript, 50).unwrap());
    let mut agent = Agent::new(
        Config::default(),
        ws.clone(),
        sidecar,
        llm,
        sink.clone(),
        Box::new(AutoApprove),
        "你是测试用的 agent。".into(),
    );

    let outcome = agent.run_task("修复测试失败").await.unwrap();
    assert!(outcome.success, "黄金场景应成功: {}", outcome.summary);
    let fixed = std::fs::read_to_string(ws.join("calculator.py")).unwrap();
    assert!(fixed.contains("return a + b"), "calculator.py 应被修复为正确的加法");
    let _ = std::fs::remove_dir_all(&ws);
    let events = sink.0.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::Verify { passed: true, .. })));
    assert!(events.iter().any(|e| matches!(e, Event::ApprovalRequired { .. })));
}

#[tokio::test]
async fn repair_rounds_beyond_legacy_limit_can_recover() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let ws = temp_fixture_ws();
    let broken = std::fs::read_to_string(ws.join("calculator.py")).unwrap();
    let fixed = format!("{}\n# repaired by long-loop regression\n", broken.replace("return a - b", "return a + b"));
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let mut turns: Vec<LlmResponse> = (0..4)
        .map(|index| LlmResponse {
            content: Some(format!("第 {index} 次修复尝试")),
            tool_calls: vec![write_call(&broken), test_call()],
            prompt_tokens: 10,
            completion_tokens: 10,
        })
        .collect();
    for index in 0..3 {
        turns.push(LlmResponse {
            content: Some(format!("换一种修复方案 {index}")),
            tool_calls: vec![write_call(&fixed), test_call()],
            prompt_tokens: 10,
            completion_tokens: 10,
        });
    }
    turns.extend((0..3).map(|_| LlmResponse {
        content: Some("修复完成".into()),
        tool_calls: vec![],
        prompt_tokens: 10,
        completion_tokens: 10,
    }));
    let llm: Box<dyn LlmClient> = Box::new(FixtureLlm::from_turns(turns, 10));
    let mut agent = Agent::new(
        Config::default(), ws.clone(), sidecar, llm, sink.clone(), Box::new(AutoApprove),
        "你是测试用的 agent。".into(),
    );

    let outcome = agent.run_task("修复测试失败").await.unwrap();
    assert!(outcome.success, "超过旧修复轮数后仍应允许恢复: {}", outcome.summary);
    assert!(outcome.repair_rounds >= 4);
    assert!(std::fs::read_to_string(ws.join("calculator.py")).unwrap().contains("return a + b"));
    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn more_than_sixty_four_tool_calls_continue_with_checkpoints() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let ws = temp_fixture_ws();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let mut turns = (0..65)
        .map(|index| LlmResponse {
            content: Some(format!("检查第 {index} 项")),
            tool_calls: vec![ToolCall {
                id: format!("search_{index}"),
                r#type: "function".into(),
                function: ToolFunctionCall {
                    name: "fs_search".into(),
                    arguments: serde_json::json!({"pattern": format!("never-match-{index}")}).to_string(),
                },
            }],
            prompt_tokens: 1,
            completion_tokens: 1,
        })
        .collect::<Vec<_>>();
    turns.push(LlmResponse {
        content: Some("长任务检查完成".into()),
        tool_calls: vec![],
        prompt_tokens: 1,
        completion_tokens: 1,
    });
    let llm: Box<dyn LlmClient> = Box::new(FixtureLlm::from_turns(turns, 1));
    let mut agent = Agent::new(
        Config::default(), ws.clone(), sidecar, llm, sink.clone(), Box::new(AutoApprove),
        "你是测试用的 agent。".into(),
    );

    let outcome = agent.run_task("执行长时间项目检查").await.unwrap();
    assert!(outcome.success);
    assert_eq!(outcome.steps, 65);
    assert!(outcome.checkpoint_count >= 2);
    let events = sink.0.lock().unwrap();
    assert!(events.iter().filter(|event| matches!(event, Event::Checkpoint { .. })).count() >= 2);
    drop(events);
    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn token_budget_checkpoint_can_stop_cleanly() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let ws = temp_fixture_ws();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let turns = vec![LlmResponse {
        content: Some("先检查".into()),
        tool_calls: vec![ToolCall {
            id: "budget_search".into(), r#type: "function".into(),
            function: ToolFunctionCall { name: "fs_search".into(), arguments: serde_json::json!({"pattern":"missing"}).to_string() },
        }],
        prompt_tokens: 1, completion_tokens: 1,
    }];
    let mut config = Config::default();
    config.llm.max_total_tokens = 2;
    let mut agent = Agent::new(
        config, ws.clone(), sidecar,
        Box::new(FixtureLlm::from_turns(turns, 1)), sink.clone(), Box::new(DenyApprove),
        "你是测试用的 agent。".into(),
    );

    let outcome = agent.run_task("预算测试").await.unwrap();
    assert!(!outcome.success);
    assert_eq!(outcome.stop_reason.as_deref(), Some("budget_declined"));
    assert!(sink.0.lock().unwrap().iter().any(|event| matches!(event, Event::Checkpoint { reason, .. } if reason == "token_budget")));
    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn resumed_task_skips_completed_non_idempotent_action() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let ws = temp_fixture_ws();
    let original = std::fs::read_to_string(ws.join("calculator.py")).unwrap();
    let repeated_call = write_call("this content must not be replayed");
    let fingerprint = tool_fingerprint("fs_write_file", &repeated_call.function.arguments);
    let agent_dir = ws.join(".furina/agent");
    let journal = TaskJournalStore::open(&agent_dir);
    journal.save(&TaskCheckpointRecord {
        schema_version: TASK_JOURNAL_SCHEMA_VERSION,
        task_id: "task_resume_test".into(),
        original_goal: "继续修复 calculator".into(),
        status: "checkpoint".into(),
        checkpoint_count: 2,
        steps: 7,
        total_tokens: 20,
        repair_rounds: 1,
        token_budget_limit: 100_000,
        summary: "写入动作已成功，等待确认当前状态".into(),
        blocking: "无".into(),
        wrote_files: true,
        verified: true,
        test_command: String::new(),
        scanned: true,
        known_hashes: BTreeMap::new(),
        completed_actions: vec![CompletedAction {
            fingerprint,
            tool: "fs_write_file".into(),
            result_fingerprint: "result".into(),
            summary: "先前写入已成功".into(),
            completed_at_ms: 1,
        }],
        failure_evidence: Vec::new(),
        tool_patterns: vec!["fs_write_file".into()],
        app_version: env!("CARGO_PKG_VERSION").into(),
        updated_at_ms: 1,
    }).unwrap();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let turns = vec![
        LlmResponse {
            content: Some("检查是否需要重复写入".into()),
            tool_calls: vec![repeated_call],
            prompt_tokens: 1,
            completion_tokens: 1,
        },
        LlmResponse {
            content: Some("恢复完成".into()),
            tool_calls: vec![],
            prompt_tokens: 1,
            completion_tokens: 1,
        },
    ];
    let mut agent = Agent::new(
        Config::default(), ws.clone(), sidecar, Box::new(FixtureLlm::from_turns(turns, 1)),
        sink.clone(), Box::new(AutoApprove), "你是测试用的 agent。".into(),
    );
    agent.set_task_journal(journal.clone());

    let outcome = agent.run_task("继续上次任务").await.unwrap();
    assert!(outcome.success);
    assert_eq!(std::fs::read_to_string(ws.join("calculator.py")).unwrap(), original);
    assert!(journal.load().is_none(), "成功恢复后应清除 active journal");
    let events = sink.0.lock().unwrap();
    assert!(events.iter().any(|event| matches!(event, Event::TaskRecoveryResumed { task_id, .. } if task_id == "task_resume_test")));
    assert!(events.iter().any(|event| matches!(event, Event::ToolResult { name, summary, .. } if name == "fs_write_file" && summary.contains("already_completed"))));
    drop(events);
    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn declining_recovery_discards_saved_task_without_running_llm() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let ws = temp_fixture_ws();
    let agent_dir = ws.join(".furina/agent");
    let journal = TaskJournalStore::open(&agent_dir);
    journal.save(&TaskCheckpointRecord {
        schema_version: TASK_JOURNAL_SCHEMA_VERSION,
        task_id: "task_decline_test".into(),
        original_goal: "不应继续的任务".into(),
        status: "paused".into(),
        checkpoint_count: 1,
        steps: 3,
        total_tokens: 5,
        repair_rounds: 0,
        token_budget_limit: 100,
        summary: "等待恢复".into(),
        blocking: "等待用户".into(),
        wrote_files: false,
        verified: false,
        test_command: String::new(),
        scanned: false,
        known_hashes: BTreeMap::new(),
        completed_actions: Vec::new(),
        failure_evidence: Vec::new(),
        tool_patterns: Vec::new(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        updated_at_ms: 1,
    }).unwrap();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let mut agent = Agent::new(
        Config::default(), ws.clone(), sidecar,
        Box::new(FixtureLlm::from_turns(Vec::new(), 1)), sink.clone(), Box::new(DenyApprove),
        "你是测试用的 agent。".into(),
    );
    agent.set_task_journal(journal.clone());

    let outcome = agent.run_task("恢复").await.unwrap();
    assert!(!outcome.success);
    assert_eq!(outcome.stop_reason.as_deref(), Some("recovery_declined"));
    assert!(journal.load().is_none());
    assert!(sink.0.lock().unwrap().iter().any(|event| matches!(event, Event::TaskRecoveryDiscarded { task_id } if task_id == "task_decline_test")));
    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn repeated_identical_tool_results_trigger_stall_review() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let ws = temp_fixture_ws();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let turns = (0..6).map(|index| LlmResponse {
        content: Some(format!("重复检查 {index}")),
        tool_calls: vec![ToolCall {
            id: format!("repeat_{index}"), r#type: "function".into(),
            function: ToolFunctionCall { name: "fs_search".into(), arguments: serde_json::json!({"pattern":"same-never-match"}).to_string() },
        }],
        prompt_tokens: 1, completion_tokens: 1,
    }).collect();
    let mut config = Config::default();
    config.agent.max_repeated_tool_calls = 3;
    config.agent.max_stalled_checkpoints = 1;
    let mut agent = Agent::new(
        config, ws.clone(), sidecar, Box::new(FixtureLlm::from_turns(turns, 1)),
        sink.clone(), Box::new(DenyApprove), "你是测试用的 agent。".into(),
    );

    let outcome = agent.run_task("检测死循环").await.unwrap();
    assert!(!outcome.success);
    assert_eq!(outcome.stop_reason.as_deref(), Some("stalled"));
    assert!(outcome.checkpoint_count >= 2);
    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn chat_turn_without_tools_does_not_scan() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let ws = temp_fixture_ws();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let turns = vec![LlmResponse {
        content: Some("哼，本神才不笨呢！".into()),
        tool_calls: vec![],
        prompt_tokens: 10,
        completion_tokens: 10,
    }];
    let llm: Box<dyn LlmClient> = Box::new(FixtureLlm::from_turns(turns, 10));
    let mut agent = Agent::new(
        Config::default(),
        ws.clone(),
        sidecar,
        llm,
        sink.clone(),
        Box::new(AutoApprove),
        "你是测试用的 agent。".into(),
    );

    let outcome = agent.run_task("傻芙芙").await.unwrap();
    let _ = std::fs::remove_dir_all(&ws);
    assert!(outcome.success);
    let events = sink.0.lock().unwrap();
    assert!(
        !events.iter().any(|e| matches!(e, Event::Scan { .. })),
        "纯聊天回合不应扫描项目"
    );
    assert!(
        !events.iter().any(|e| matches!(e, Event::ToolCall { .. })),
        "纯聊天回合不应调用工具"
    );
}

#[tokio::test]
async fn task_turn_scans_lazily_before_first_tool() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let ws = temp_fixture_ws();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let read = ToolCall {
        id: "call_r".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "fs_read_file".into(),
            arguments: serde_json::json!({"path": "calculator.py"}).to_string(),
        },
    };
    let turns = vec![
        LlmResponse {
            content: Some("本神看看源码。".into()),
            tool_calls: vec![read],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
        LlmResponse {
            content: Some("看完了。".into()),
            tool_calls: vec![],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
    ];
    let llm: Box<dyn LlmClient> = Box::new(FixtureLlm::from_turns(turns, 10));
    let mut agent = Agent::new(
        Config::default(),
        ws.clone(),
        sidecar,
        llm,
        sink.clone(),
        Box::new(AutoApprove),
        "你是测试用的 agent。".into(),
    );

    let outcome = agent.run_task("看看 calculator.py").await.unwrap();
    let _ = std::fs::remove_dir_all(&ws);
    assert!(outcome.success);
    let events = sink.0.lock().unwrap();
    let scan_idx = events.iter().position(|e| matches!(e, Event::Scan { .. }));
    let tool_idx = events.iter().position(|e| matches!(e, Event::ToolCall { .. }));
    assert!(scan_idx.is_some(), "任务应触发惰性扫描");
    assert!(tool_idx.is_some(), "任务应执行工具");
    assert!(scan_idx.unwrap() < tool_idx.unwrap(), "扫描应先于首次工具执行");
}

#[tokio::test]
async fn read_outside_workspace_requires_approval_and_succeeds() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let ws = temp_fixture_ws();
    let outside = std::env::temp_dir().join(format!("furina_outside_{}", std::process::id()));
    std::fs::write(&outside, "outside content").unwrap();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let read = ToolCall {
        id: "call_o".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "fs_read_file".into(),
            arguments: serde_json::json!({"path": outside.to_string_lossy().to_string()}).to_string(),
        },
    };
    let turns = vec![
        LlmResponse {
            content: Some("本神读一下。".into()),
            tool_calls: vec![read],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
        LlmResponse {
            content: Some("读完了。".into()),
            tool_calls: vec![],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
    ];
    let llm: Box<dyn LlmClient> = Box::new(FixtureLlm::from_turns(turns, 10));
    let mut agent = Agent::new(
        Config::default(),
        ws.clone(),
        sidecar,
        llm,
        sink.clone(),
        Box::new(AutoApprove),
        "你是测试用的 agent。".into(),
    );

    let outcome = agent.run_task("读外面的文件").await.unwrap();
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_file(&outside);
    assert!(outcome.success);
    let events = sink.0.lock().unwrap();
    assert!(
        events.iter().any(|e| matches!(e, Event::ApprovalRequired { .. })),
        "越界读应请求审批"
    );
    assert!(
        events.iter().any(|e| matches!(e, Event::ToolResult { ok: true, .. })),
        "审批后应读取成功"
    );
}

#[tokio::test]
async fn web_search_denied_without_approval() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let ws = temp_fixture_ws();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let search = ToolCall {
        id: "call_w".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "web_search".into(),
            arguments: serde_json::json!({"query": "test"}).to_string(),
        },
    };
    let turns = vec![
        LlmResponse {
            content: Some("搜一下。".into()),
            tool_calls: vec![search],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
        LlmResponse {
            content: Some("没搜成。".into()),
            tool_calls: vec![],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
    ];
    let llm: Box<dyn LlmClient> = Box::new(FixtureLlm::from_turns(turns, 10));
    let mut cfg = Config::default();
    cfg.approval.auto_approve_read_only_web = false;
    let mut agent = Agent::new(
        cfg,
        ws.clone(),
        sidecar,
        llm,
        sink.clone(),
        Box::new(DenyApprove),
        "你是测试用的 agent。".into(),
    );
    let outcome = agent.run_task("搜索测试").await.unwrap();
    let _ = std::fs::remove_dir_all(&ws);
    assert!(outcome.success);
    let events = sink.0.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::ApprovalRequired { .. })));
    assert!(events.iter().any(|e| matches!(e, Event::ToolResult { ok: false, .. })));
}

#[tokio::test]
async fn web_search_approved_hits_mock_backend() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let body = r#"{"results":[{"title":"Rust","url":"https://rust-lang.org","content":"系统编程语言"}]}"#.to_string();
    let addr = spawn_web_mock(body);
    let mut cfg = Config::default();
    cfg.approval.auto_approve_read_only_web = false;
    cfg.web = furina_core::config::WebConfig {
        search_backend: "tavily".into(),
        api_key_env: "FURINA_TEST_WEB_KEY".into(),
        endpoint: format!("http://{addr}"),
        max_results: 5,
        fallback_backend: String::new(),
        fallback_endpoint: String::new(),
    };
    std::env::set_var("FURINA_TEST_WEB_KEY", "k");

    let ws = temp_fixture_ws();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let search = ToolCall {
        id: "call_w2".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "web_search".into(),
            arguments: serde_json::json!({"query": "rust"}).to_string(),
        },
    };
    let turns = vec![
        LlmResponse {
            content: Some("搜一下。".into()),
            tool_calls: vec![search],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
        LlmResponse {
            content: Some("搜到了。".into()),
            tool_calls: vec![],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
    ];
    let llm: Box<dyn LlmClient> = Box::new(FixtureLlm::from_turns(turns, 10));
    let mut agent = Agent::new(
        cfg,
        ws.clone(),
        sidecar,
        llm,
        sink.clone(),
        Box::new(AutoApprove),
        "你是测试用的 agent。".into(),
    );
    let outcome = agent.run_task("搜索").await.unwrap();
    let _ = std::fs::remove_dir_all(&ws);
    std::env::remove_var("FURINA_TEST_WEB_KEY");
    assert!(outcome.success);
    let events = sink.0.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::ApprovalRequired { .. })));
    assert!(
        events.iter().any(|e| match e {
            Event::ToolResult { ok: true, summary, .. } => summary.contains("rust-lang"),
            _ => false,
        }),
        "审批通过后应调用搜索后端并返回结果"
    );
}

#[tokio::test]
async fn web_search_auto_approves_after_first_confirm() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let body = r#"{"results":[{"title":"R","url":"https://r","content":"c"}]}"#.to_string();
    let addr = spawn_web_mock_loop(body, 2);
    let mut cfg = Config::default();
    cfg.approval.auto_approve_read_only_web = false;
    cfg.web = furina_core::config::WebConfig {
        search_backend: "tavily".into(),
        api_key_env: "FURINA_TEST_WEB_KEY2".into(),
        endpoint: format!("http://{addr}"),
        max_results: 5,
        fallback_backend: String::new(),
        fallback_endpoint: String::new(),
    };
    std::env::set_var("FURINA_TEST_WEB_KEY2", "k");

    let ws = temp_fixture_ws();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let search = ToolCall {
        id: "call_w3".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "web_search".into(),
            arguments: serde_json::json!({"query": "rust"}).to_string(),
        },
    };
    let turns = vec![
        LlmResponse {
            content: Some("搜一下。".into()),
            tool_calls: vec![search.clone()],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
        LlmResponse {
            content: Some("再搜一下。".into()),
            tool_calls: vec![search.clone()],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
        LlmResponse {
            content: Some("搜到了。".into()),
            tool_calls: vec![],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
    ];
    let llm: Box<dyn LlmClient> = Box::new(FixtureLlm::from_turns(turns, 10));
    let confirm_count = Arc::new(Mutex::new(0usize));
    let mut agent = Agent::new(
        cfg,
        ws.clone(),
        sidecar,
        llm,
        sink.clone(),
        Box::new(CountingApprover { count: confirm_count.clone() }),
        "你是测试用的 agent。".into(),
    );
    let outcome = agent.run_task("搜索").await.unwrap();
    let _ = std::fs::remove_dir_all(&ws);
    std::env::remove_var("FURINA_TEST_WEB_KEY2");
    assert!(outcome.success);
    assert_eq!(
        *confirm_count.lock().unwrap(),
        1,
        "两次 web_search 只应审批一次（会话级首次）"
    );
}

#[tokio::test]
async fn web_tools_populate_cache() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let page_body = "<html><head><title>Rust 官网</title></head><body><p>系统编程语言正文内容</p></body></html>".to_string();
    let page_addr = spawn_web_mock(page_body);
    let search_body = r#"{"results":[{"title":"Rust 语言","url":"https://rust-lang.org","content":"系统编程语言"}]}"#.to_string();
    let search_addr = spawn_web_mock(search_body);
    let page_url = format!("http://{page_addr}/page");

    let mut cfg = Config::default();
    cfg.web = furina_core::config::WebConfig {
        search_backend: "tavily".into(),
        api_key_env: "FURINA_TEST_CACHE_KEY".into(),
        endpoint: format!("http://{search_addr}"),
        max_results: 5,
        fallback_backend: String::new(),
        fallback_endpoint: String::new(),
    };
    std::env::set_var("FURINA_TEST_CACHE_KEY", "k");

    let ws = temp_fixture_ws();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let open = ToolCall {
        id: "call_o".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "web_open".into(),
            arguments: serde_json::json!({"url": page_url}).to_string(),
        },
    };
    let search = ToolCall {
        id: "call_s".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "web_search".into(),
            arguments: serde_json::json!({"query": "rust"}).to_string(),
        },
    };
    let turns = vec![
        LlmResponse {
            content: Some("打开。".into()),
            tool_calls: vec![open],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
        LlmResponse {
            content: Some("搜索。".into()),
            tool_calls: vec![search],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
        LlmResponse {
            content: Some("完成。".into()),
            tool_calls: vec![],
            prompt_tokens: 10,
            completion_tokens: 10,
        },
    ];
    let llm: Box<dyn LlmClient> = Box::new(FixtureLlm::from_turns(turns, 10));
    let cache_dir = std::env::temp_dir().join(format!("furina_golden_cache_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache_dir);
    let mut agent = Agent::new(
        cfg,
        ws.clone(),
        sidecar,
        llm,
        sink.clone(),
        Box::new(AutoApprove),
        "你是测试用的 agent。".into(),
    );
    agent.set_web_cache(WebCache::open(&cache_dir));
    let outcome = agent.run_task("读网页").await.unwrap();
    let _ = std::fs::remove_dir_all(&ws);
    std::env::remove_var("FURINA_TEST_CACHE_KEY");
    assert!(outcome.success);

    let cache = WebCache::open(&cache_dir);
    assert!(cache.count() >= 2, "web_open 与 web_search 都应写入缓存");
    let opened = cache.get(&page_url).expect("打开的网页应已缓存");
    assert!(opened.content.contains("系统编程语言正文"), "应缓存正文");
    assert_eq!(opened.title, "Rust 官网", "应提取网页标题");
    assert!(cache.search("rust").len() >= 2, "关键词应能检索到缓存");
    let _ = std::fs::remove_dir_all(&cache_dir);
}

#[tokio::test]
async fn provider_switch_keeps_prompt_identical() {
    if !python_available() {
        return;
    }
    let base = Config::load(&repo_root().join("desktop/resources/defaults/config.yaml")).unwrap();

    let mut cfg_a = base.clone();
    cfg_a.llm.providers = vec![ProviderConfig {
        id: "deepseek".into(),
        label: "DeepSeek".into(),
        base_url: "https://api.deepseek.com".into(),
        api_key_env: "KEY_A".into(),
        model: "deepseek-chat".into(),
        vision: false,
    }];
    cfg_a.llm.active_provider = Some("deepseek".into());

    let mut cfg_b = base.clone();
    cfg_b.llm.providers = vec![ProviderConfig {
        id: "relay".into(),
        label: "中转站".into(),
        base_url: "https://relay.example.com/v1".into(),
        api_key_env: "KEY_B".into(),
        model: "gpt-4o-mini".into(),
        vision: false,
    }];
    cfg_b.llm.active_provider = Some("relay".into());

    let ws_a = temp_fixture_ws();
    let ws_b = temp_fixture_ws();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let seen_a = Arc::new(Mutex::new(Vec::new()));
    let seen_b = Arc::new(Mutex::new(Vec::new()));

    let mut agent_a = Agent::new(
        cfg_a,
        ws_a.clone(),
        spawn_sidecar(&ws_a, sink.clone()).await,
        Box::new(RecordingLlm { seen: seen_a.clone() }),
        sink.clone(),
        Box::new(AutoApprove),
        "system".into(),
    );
    agent_a.set_prompt_context(Box::new(FixedContext));

    let mut agent_b = Agent::new(
        cfg_b,
        ws_b.clone(),
        spawn_sidecar(&ws_b, sink.clone()).await,
        Box::new(RecordingLlm { seen: seen_b.clone() }),
        sink.clone(),
        Box::new(AutoApprove),
        "system".into(),
    );
    agent_b.set_prompt_context(Box::new(FixedContext));

    let out_a = agent_a.run_task("介绍一下你自己").await.unwrap();
    let out_b = agent_b.run_task("介绍一下你自己").await.unwrap();
    assert!(out_a.success && out_b.success);

    let msgs_a = serde_json::to_string(&*seen_a.lock().unwrap()).unwrap();
    let msgs_b = serde_json::to_string(&*seen_b.lock().unwrap()).unwrap();
    assert!(!msgs_a.is_empty(), "应至少发送一轮消息");
    assert_eq!(
        msgs_a, msgs_b,
        "切换模型提供方不应改变发送给 LLM 的消息（人格一致性）"
    );
}

#[tokio::test]
async fn runtime_soul_context_is_current_request_only() {
    if !python_available() {
        return;
    }
    let cfg = Config::load(&repo_root().join("desktop/resources/defaults/config.yaml")).unwrap();
    let ws = temp_fixture_ws();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut agent = Agent::new(
        cfg,
        ws.clone(),
        spawn_sidecar(&ws, sink.clone()).await,
        Box::new(RecordingLlm { seen: seen.clone() }),
        sink,
        Box::new(AutoApprove),
        "system".into(),
    );
    agent.set_prompt_context(Box::new(FixedContext));

    agent.run_task("你好").await.unwrap();
    agent.run_task("今天怎么样").await.unwrap();

    let transcript = serde_json::to_string(&agent.transcript_snapshot()).unwrap();
    assert!(!transcript.contains("当前灵魂状态"));

    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        let serialized = serde_json::to_string(request).unwrap();
        assert_eq!(serialized.matches("当前灵魂状态").count(), 1);
        assert!(!serialized.contains("表达策略"));
        assert!(!serialized.contains("回复预算"));
    }
}

#[tokio::test]
async fn js_repair_loop_fixes_calculator() {
    if !python_available() || !node_available() {
        eprintln!("skip: python/node 不可用");
        return;
    }
    let ws = temp_fixture("sample_js");
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let read = ToolCall {
        id: "call_r1".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "fs_read_file".into(),
            arguments: serde_json::json!({"path": "calculator.js"}).to_string(),
        },
    };
    let read2 = ToolCall {
        id: "call_r2".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "fs_read_file".into(),
            arguments: serde_json::json!({"path": "calculator.test.js"}).to_string(),
        },
    };
    let fixed = "// A tiny calculator module used as a Furina Agent golden fixture.\nexport function add(a, b) {\n  return a + b;\n}\n\nexport function multiply(a, b) {\n  return a * b;\n}\n";
    let write = ToolCall {
        id: "call_w".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "fs_write_file".into(),
            arguments: serde_json::json!({"path": "calculator.js", "content": fixed}).to_string(),
        },
    };
    let test = ToolCall {
        id: "call_t".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "term_run".into(),
            arguments: serde_json::json!({"command": "npm test"}).to_string(),
        },
    };
    let turns = vec![
        LlmResponse { content: Some("本神看看源码。".into()), tool_calls: vec![read, read2], prompt_tokens: 10, completion_tokens: 10 },
        LlmResponse { content: Some("修正它。".into()), tool_calls: vec![write], prompt_tokens: 10, completion_tokens: 10 },
        LlmResponse { content: Some("跑测试。".into()), tool_calls: vec![test], prompt_tokens: 10, completion_tokens: 10 },
        LlmResponse { content: Some("通过了。".into()), tool_calls: vec![], prompt_tokens: 10, completion_tokens: 10 },
    ];
    let llm: Box<dyn LlmClient> = Box::new(FixtureLlm::from_turns(turns, 10));
    let mut agent = Agent::new(
        Config::default(),
        ws.clone(),
        sidecar,
        llm,
        sink.clone(),
        Box::new(AutoApprove),
        "你是测试用的 agent。".into(),
    );
    let outcome = agent.run_task("修复测试失败").await.unwrap();
    let fixed_ok = std::fs::read_to_string(ws.join("calculator.js")).unwrap();
    let _ = std::fs::remove_dir_all(&ws);
    assert!(outcome.success, "JS 黄金场景应成功: {}", outcome.summary);
    assert!(fixed_ok.contains("return a + b"), "calculator.js 应被修复为加法");
    let events = sink.0.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::Verify { passed: true, .. })));
}

#[tokio::test]
async fn java_repair_loop_fixes_calculator() {
    if !python_available() || !mvn_available() {
        eprintln!("skip: python/mvn 不可用");
        return;
    }
    let ws = temp_fixture("sample_java");
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let main_path = "src/main/java/sample/Calculator.java";
    let read = ToolCall {
        id: "call_r1".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "fs_read_file".into(),
            arguments: serde_json::json!({"path": main_path}).to_string(),
        },
    };
    let fixed = "package sample;\n\npublic class Calculator {\n    public static int add(int a, int b) {\n        return a + b;\n    }\n\n    public static int multiply(int a, int b) {\n        return a * b;\n    }\n}\n";
    let write = ToolCall {
        id: "call_w".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "fs_write_file".into(),
            arguments: serde_json::json!({"path": main_path, "content": fixed}).to_string(),
        },
    };
    let test = ToolCall {
        id: "call_t".into(),
        r#type: "function".into(),
        function: ToolFunctionCall {
            name: "term_run".into(),
            arguments: serde_json::json!({"command": "mvn test"}).to_string(),
        },
    };
    let turns = vec![
        LlmResponse { content: Some("本神看看源码。".into()), tool_calls: vec![read], prompt_tokens: 10, completion_tokens: 10 },
        LlmResponse { content: Some("修正它。".into()), tool_calls: vec![write], prompt_tokens: 10, completion_tokens: 10 },
        LlmResponse { content: Some("跑测试。".into()), tool_calls: vec![test], prompt_tokens: 10, completion_tokens: 10 },
        LlmResponse { content: Some("通过了。".into()), tool_calls: vec![], prompt_tokens: 10, completion_tokens: 10 },
    ];
    let llm: Box<dyn LlmClient> = Box::new(FixtureLlm::from_turns(turns, 10));
    let mut agent = Agent::new(
        Config::default(),
        ws.clone(),
        sidecar,
        llm,
        sink.clone(),
        Box::new(AutoApprove),
        "你是测试用的 agent。".into(),
    );
    let outcome = agent.run_task("修复测试失败").await.unwrap();
    let fixed_ok = std::fs::read_to_string(ws.join(main_path)).unwrap();
    let _ = std::fs::remove_dir_all(&ws);
    assert!(outcome.success, "Java 黄金场景应成功: {}", outcome.summary);
    assert!(fixed_ok.contains("return a + b;"), "Calculator.java 应被修复为加法");
    let events = sink.0.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::Verify { passed: true, .. })));
}

#[tokio::test]
async fn sidecar_protocol_roundtrip() {
    if !python_available() {
        eprintln!("skip: python 不可用");
        return;
    }
    let ws = temp_fixture_ws();
    let sink = Arc::new(CollectSink(Arc::new(Mutex::new(Vec::new()))));
    let mut sidecar = spawn_sidecar(&ws, sink.clone()).await;
    let tools = sidecar.call("tools.list", serde_json::json!({})).await.unwrap();
    assert!(tools["tools"].is_array());
    let read = sidecar.call("fs.read_file", serde_json::json!({"path": "calculator.py"})).await.unwrap();
    assert!(read["content"].as_str().unwrap().contains("def add"));
    let _ = std::fs::remove_dir_all(&ws);
}
