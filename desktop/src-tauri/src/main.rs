//! Furina Desktop：实时语音对话桌面版（Tauri 2）。
//!
//! 完全复用 furina-core：Agent 任务、灵魂状态、TTS/ASR、单实例锁与 `.furina/` 状态目录。
//! 前端负责聊天渲染、PTT 录音与音频播放；本壳负责 IPC 与 Agent 生命周期。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use async_trait::async_trait;
use furina_core::agent::{Agent, Approver};
use furina_core::app::{self, InstanceLock};
use furina_core::asr::AsrClient;
use furina_core::config::Config;
use furina_core::interject::{InterjectCtx, Interjector};
use furina_core::sidecar::EventSink;
use furina_core::voice::VoiceClient;
use furina_proto::Event;
use furina_soul::Soul;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{ipc::Response, Emitter, Manager, State};
use tokio::sync::{mpsc, oneshot};

struct AppState {
    #[allow(dead_code)]
    _lock: InstanceLock,
    app: tauri::AppHandle,
    root: PathBuf,
    ws: PathBuf,
    persona: String,
    soul: Arc<Mutex<Soul>>,
    agent: Mutex<Option<Agent>>,
    voice: Option<VoiceClient>,
    asr: Option<AsrClient>,
    interjector: Option<Arc<Interjector>>,
    interject_tx: Option<mpsc::UnboundedSender<InterjectCtx>>,
    agent_state: Arc<Mutex<String>>,
    approvals: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    next_approval_id: Arc<Mutex<u64>>,
}

/// 把核心事件流转发给前端（前端自行做流式渲染与句子级语音）。
struct DesktopSink {
    app: tauri::AppHandle,
    soul: Arc<Mutex<Soul>>,
    used_tools: Mutex<bool>,
    interjector: Option<Arc<Interjector>>,
    interject_tx: Option<mpsc::UnboundedSender<InterjectCtx>>,
    agent_state: Arc<Mutex<String>>,
}

impl EventSink for DesktopSink {
    fn emit(&self, event: Event) {
        eprintln!("[desktop-sink] {:?}", std::mem::discriminant(&event));
        match &event {
            Event::SessionStarted { .. } => {
                *self.used_tools.lock().unwrap() = false;
                *self.agent_state.lock().unwrap() = "working".into();
                if let Some(inj) = &self.interjector {
                    inj.reset_budget();
                }
            }
            Event::ToolCall { .. } => *self.used_tools.lock().unwrap() = true,
            Event::Done { .. } => *self.agent_state.lock().unwrap() = "idle".into(),
            _ => {}
        }
        // 关键节点 → LLM 人格插话（异步生成，串行 worker 保证顺序；前端渲染气泡+朗读）。
        if let Some(tx) = &self.interject_tx {
            let used_tools = *self.used_tools.lock().unwrap();
            let ctx = match &event {
                Event::ApprovalGranted { kind } => Some(desktop_interject_ctx(
                    &self.soul, "approval_granted", kind,
                )),
                Event::ApprovalDenied { kind } => Some(desktop_interject_ctx(
                    &self.soul, "approval_denied", kind,
                )),
                Event::Done { success, summary } if used_tools => Some(desktop_interject_ctx(
                    &self.soul,
                    if *success { "task_done" } else { "task_failed" },
                    summary,
                )),
                Event::Verify { passed: false, detail } => {
                    Some(desktop_interject_ctx(&self.soul, "verify_fail", detail))
                }
                _ => None,
            };
            if let Some(ctx) = ctx {
                let _ = tx.send(ctx);
            }
        }
        if let Ok(v) = serde_json::to_value(&event) {
            let _ = self.app.emit("furina-event", v);
        }
        if matches!(event, Event::Done { .. }) {
            let _ = self.app.emit("furina-soul", serde_json::json!({}));
        }
    }
}

/// 组装桌面端插话上下文（当前心情 + 事件类型 + 动作详情）。
fn desktop_interject_ctx(soul: &Mutex<Soul>, kind: &str, detail: &str) -> InterjectCtx {
    let s = soul.lock().unwrap();
    InterjectCtx {
        mood: s.mood().as_str().to_string(),
        mood_label: s.mood().label().to_string(),
        event_kind: kind.to_string(),
        action_detail: detail.to_string(),
        strategy: s.expression_strategy(),
    }
}

/// GUI 审批：向前端发弹窗事件并等待用户响应。
struct DesktopApprover {
    app: tauri::AppHandle,
    approvals: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    next_id: Arc<Mutex<u64>>,
}

#[async_trait]
impl Approver for DesktopApprover {
    async fn confirm(&mut self, prompt: &str) -> bool {
        eprintln!("[desktop-approval] 等待审批: {prompt}");
        let id = {
            let mut n = self.next_id.lock().unwrap();
            *n += 1;
            *n
        }
        .to_string();
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().unwrap().insert(id.clone(), tx);
        let _ = self.app.emit(
            "furina-approval",
            serde_json::json!({ "id": id, "prompt": prompt }),
        );
        rx.await.unwrap_or(false)
    }
}

/// 惰性构建 Agent（LLM + 侧车 + 人格注入），复用 furina-core 的装配层。
async fn ensure_agent(state: &State<'_, AppState>) -> Result<(), String> {
    if state.agent.lock().map_err(|e| e.to_string())?.is_some() {
        return Ok(());
    }
    eprintln!("[desktop] 构建 Agent（首次）…");
    let sink: Arc<dyn EventSink> = Arc::new(DesktopSink {
        app: state.app.clone(),
        soul: state.soul.clone(),
        used_tools: Mutex::new(false),
        interjector: state.interjector.clone(),
        interject_tx: state.interject_tx.clone(),
        agent_state: state.agent_state.clone(),
    });
    let approver: Box<dyn Approver> = Box::new(DesktopApprover {
        app: state.app.clone(),
        approvals: state.approvals.clone(),
        next_id: state.next_approval_id.clone(),
    });
    let agent = app::build_agent(
        &state.root,
        &state.ws,
        &state.persona,
        state.soul.clone(),
        sink,
        approver,
    )
    .await
    .map_err(|e| e.to_string())?;
    eprintln!("[desktop] Agent 就绪");
    *state.agent.lock().map_err(|e| e.to_string())? = Some(agent);
    Ok(())
}

#[tauri::command]
async fn chat_send(state: State<'_, AppState>, text: String) -> Result<(), String> {
    eprintln!("[desktop] chat_send: {text}");
    ensure_agent(&state).await?;
    let mut agent = state
        .agent
        .lock()
        .map_err(|e| e.to_string())?
        .take()
        .ok_or("Agent 未初始化")?;
    let result = agent.run_task(&text).await;
    *state.agent.lock().map_err(|e| e.to_string())? = Some(agent);
    if let Err(error) = result {
        *state.agent_state.lock().map_err(|e| e.to_string())? = "idle".into();
        let message = error.to_string();
        eprintln!("[desktop] chat_send 失败: {message}");
        let _ = state.app.emit("furina-soul", serde_json::json!({}));
        return Err(message);
    }
    eprintln!("[desktop] chat_send 完成");
    {
        let mut s = state.soul.lock().unwrap();
        let _ = s.save();
    }
    let _ = state.app.emit("furina-soul", serde_json::json!({}));
    Ok(())
}

#[tauri::command]
async fn transcribe(
    state: State<'_, AppState>,
    audio: Vec<u8>,
    mime: String,
) -> Result<String, String> {
    let asr = state
        .asr
        .as_ref()
        .ok_or("语音识别未配置（config.yaml asr.enabled: true，并设置 QWEN_API_KEY 或 FISH_AUDIO_API_KEY）")?;
    asr.transcribe(audio, &mime)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn tts_synthesize(
    state: State<'_, AppState>,
    text: String,
    emotion: String,
    speed: f64,
) -> Result<serde_json::Value, String> {
    let voice = state
        .voice
        .as_ref()
        .ok_or("语音合成未配置（config.yaml voice.enabled: true + FISH_AUDIO_API_KEY）")?;
    let format = voice.format().to_string();
    let data = voice
        .synthesize_bytes(&text, &emotion, speed)
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "format": format, "data": data }))
}

#[tauri::command]
async fn stop_speaking() {}

#[tauri::command]
async fn approval_respond(state: State<'_, AppState>, id: String, ok: bool) -> Result<(), String> {
    if let Some(tx) = state.approvals.lock().unwrap().remove(&id) {
        let _ = tx.send(ok);
    }
    Ok(())
}

#[tauri::command]
async fn get_soul_state(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let s = state.soul.lock().unwrap();
    let mood = s.mood();
    let stage = s.stage();
    let e = &s.emotion;
    let root_name = state
        .root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".into());
    Ok(serde_json::json!({
        "schema_version": "1.0",
        "timestamp": furina_soul::now_ms(),
        "workspace": {
            "root": state.root.display().to_string(),
            "current_ws": state.ws.display().to_string(),
            "memory_scope": format!("workspace_{root_name}"),
            "active_memory_count": s.memory.records.len(),
        },
        "mood": mood.as_str(),
        "mood_label": mood.label(),
        "intensity": mood_intensity(mood.as_str(), e),
        "stage": {
            "id": stage.id,
            "label": stage.label,
            "trust": e.trust,
            "hint": stage.hint,
        },
        "emotions": {
            "confidence": e.confidence,
            "trust": e.trust,
            "attachment": e.attachment,
            "energy": e.energy,
            "stress": e.stress,
            "pride": e.pride,
        },
        "memory_count": s.memory.records.len(),
        "last_intent": s.last_intent.as_ref().map(|i| serde_json::json!({
            "intent": i.intent,
            "cause": i.cause,
            "value": i.value,
        })),
        "agent_status": {
            "state": *state.agent_state.lock().unwrap(),
            "action": null,
            "detail": null,
        },
        "interaction_count": s.relationship.interaction_count,
    }))
}

/// 情绪投影强度（spec §6）：心情对应维度偏离基线的归一化幅度，0–1。
fn mood_intensity(mood: &str, e: &furina_soul::EmotionState) -> f64 {
    let get = |d: &str| match d {
        "confidence" => e.confidence,
        "trust" => e.trust,
        "attachment" => e.attachment,
        "energy" => e.energy,
        "stress" => e.stress,
        "pride" => e.pride,
        _ => 0.0,
    };
    let base = |d: &str| match d {
        "confidence" => 45.0,
        "trust" => 20.0,
        "attachment" => 10.0,
        "energy" => 60.0,
        "stress" => 20.0,
        "pride" => 40.0,
        _ => 0.0,
    };
    let dims: &[&str] = match mood {
        "proud" => &["pride", "confidence"],
        "happy" => &["energy", "confidence"],
        "hurt" => &["stress", "trust"],
        "sad" => &["energy"],
        "annoyed" => &["stress", "energy"],
        _ => &[],
    };
    let v = if dims.is_empty() {
        (get("pride") - base("pride")).abs() / 100.0
    } else {
        dims.iter().map(|d| (get(d) - base(d)).abs() / 100.0).sum::<f64>()
            / dims.len() as f64
    };
    v.clamp(0.0, 1.0)
}

#[tauri::command]
async fn get_memories(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let s = state.soul.lock().unwrap();
    let recs: Vec<serde_json::Value> = s
        .memory
        .records
        .iter()
        .rev()
        .take(30)
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "type": r.kind,
                "content": r.content,
                "importance": r.importance_score,
            })
        })
        .collect();
    Ok(serde_json::json!(recs))
}

const AVATAR_RELATIVE_PATH: &str = ".furina/avatar/Furina.vrm";
const MAX_AVATAR_BYTES: u64 = 256 * 1024 * 1024;

fn avatar_asset_path(root: &Path) -> PathBuf {
    root.join(AVATAR_RELATIVE_PATH)
}

fn validate_avatar_asset(root: &Path) -> Result<(PathBuf, std::fs::Metadata), String> {
    let asset_path = avatar_asset_path(root);
    if asset_path.extension().and_then(|value| value.to_str()) != Some("vrm") {
        return Err("Avatar 资产必须使用 .vrm 扩展名".into());
    }

    let avatar_root = root.join(".furina/avatar");
    let canonical_root = avatar_root
        .canonicalize()
        .map_err(|error| format!("Avatar 目录不可用: {error}"))?;
    let canonical_asset = asset_path
        .canonicalize()
        .map_err(|error| format!("Avatar 资产不可用: {error}"))?;
    if !canonical_asset.starts_with(&canonical_root) {
        return Err("Avatar 资产路径越过了受信任目录".into());
    }

    let metadata = canonical_asset
        .metadata()
        .map_err(|error| format!("无法读取 Avatar 元数据: {error}"))?;
    if !metadata.is_file() {
        return Err("Avatar 资产不是普通文件".into());
    }
    if metadata.len() > MAX_AVATAR_BYTES {
        return Err(format!(
            "Avatar 资产超过 {} MiB 上限",
            MAX_AVATAR_BYTES / 1024 / 1024
        ));
    }

    Ok((canonical_asset, metadata))
}

#[tauri::command]
async fn get_avatar_asset_info(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let asset_path = avatar_asset_path(&state.root);
    if !asset_path.exists() {
        return Ok(serde_json::json!({
            "available": false,
            "fileName": "Furina.vrm",
            "maxBytes": MAX_AVATAR_BYTES,
        }));
    }

    let (_, metadata) = validate_avatar_asset(&state.root)?;
    Ok(serde_json::json!({
        "available": true,
        "fileName": "Furina.vrm",
        "sizeBytes": metadata.len(),
        "maxBytes": MAX_AVATAR_BYTES,
    }))
}

#[tauri::command]
async fn load_avatar_asset(state: State<'_, AppState>) -> Result<Response, String> {
    let (asset_path, _) = validate_avatar_asset(&state.root)?;
    let bytes = tokio::fs::read(asset_path)
        .await
        .map_err(|error| format!("无法载入 Avatar 资产: {error}"))?;
    Ok(Response::new(bytes))
}
#[tauri::command]
async fn doctor(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "root": state.root.display().to_string(),
        "workspace": state.ws.display().to_string(),
        "persona": state.persona,
        "voice": state.voice.is_some(),
        "asr": state.asr.is_some(),
        "asr_provider": state.asr.as_ref().map(|a| a.provider().to_string()),
    }))
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let root = app::find_repo_root().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "找不到 Furina Desktop 仓库根目录（需包含 python/furina_tools 与 persona/）。可设置 FURINA_AGENT_ROOT。",
                )
            })?;
            app::load_secrets_env(&root);
            let cfg = Config::load(&root.join(".furina/config.yaml")).unwrap_or_default();
            let ws = std::env::var("FURINA_WORKSPACE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| root.clone());
            let soul = Arc::new(Mutex::new(Soul::load(app::soul_dir(&root))));
            let lock = InstanceLock::acquire(&app::soul_dir(&root))?;
            let voice = VoiceClient::from_config(&cfg, &root.join(".furina/voice")).ok();
            let asr = AsrClient::from_config(&cfg).ok();
            // 人格化插话：关键事件 → LLM 一句插话（串行 worker；前端渲染气泡+朗读）。
            let (interjector, interject_tx) = match Interjector::from_config(&cfg) {
                Ok(inj) => {
                    let inj = Arc::new(inj);
                    let (tx, mut rx) = mpsc::unbounded_channel::<InterjectCtx>();
                    let worker_inj = inj.clone();
                    let worker_app = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        while let Some(ctx) = rx.recv().await {
                            if let Some(text) = worker_inj.line(ctx).await {
                                let _ = worker_app.emit(
                                    "furina-event",
                                    serde_json::to_value(Event::Interjection { text }).unwrap(),
                                );
                            }
                        }
                    });
                    (Some(inj), Some(tx))
                }
                Err(_) => (None, None),
            };
            let state = AppState {
                _lock: lock,
                app: app.handle().clone(),
                root,
                ws,
                persona: cfg.persona.clone(),
                soul,
                agent: Mutex::new(None),
                voice,
                asr,
                interjector,
                interject_tx,
                agent_state: Arc::new(Mutex::new("idle".into())),
                approvals: Arc::new(Mutex::new(HashMap::new())),
                next_approval_id: Arc::new(Mutex::new(0)),
            };
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            chat_send,
            transcribe,
            tts_synthesize,
            stop_speaking,
            approval_respond,
            get_soul_state,
            get_memories,
            get_avatar_asset_info,
            load_avatar_asset,
            doctor
        ])
        .run(tauri::generate_context!())
        .expect("Furina Desktop 启动失败");
}
