//! Furina Desktop：实时语音对话桌面版（Tauri 2）。
//!
//! 前端负责聊天、录音和音频播放；本壳负责运行目录、IPC 与 Agent 生命周期。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod migration;
mod runtime;

use async_trait::async_trait;
use furina_core::agent::{Agent, Approver};
use furina_core::app::{self, InstanceLock, RuntimePaths};
use furina_core::asr::AsrClient;
use furina_core::config::{Config, EmotionClassifierConfig, ProviderConfig};
use furina_core::interject::{InterjectCtx, Interjector};
use furina_core::sidecar::{EventSink, Sidecar};
use furina_core::voice::{VoiceClient, VoiceSynthesisProfile};
use furina_proto::Event;
use furina_soul::Soul;
use migration::MAX_AVATAR_BYTES;
use runtime::{DesktopPreferences, RuntimeInfo};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tauri::{ipc::Response, Emitter, Manager, State};
use tokio::sync::{mpsc, oneshot};

struct RuntimeServices {
    config: Config,
    persona: String,
    voice: Option<Arc<VoiceClient>>,
    asr: Option<Arc<AsrClient>>,
    interjector: Option<Arc<Interjector>>,
    interject_tx: Option<mpsc::UnboundedSender<InterjectCtx>>,
    errors: Vec<String>,
}

struct AppState {
    #[allow(dead_code)]
    _lock: InstanceLock,
    app: tauri::AppHandle,
    paths: Arc<RwLock<RuntimePaths>>,
    runtime_info: Mutex<RuntimeInfo>,
    soul: Arc<Mutex<Soul>>,
    agent: Mutex<Option<Agent>>,
    services: Mutex<RuntimeServices>,
    agent_state: Arc<Mutex<String>>,
    approvals: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    next_approval_id: Arc<Mutex<u64>>,
}

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
        match &event {
            Event::SessionStarted { .. } => {
                *self.used_tools.lock().unwrap() = false;
                *self.agent_state.lock().unwrap() = "working".into();
                if let Some(interjector) = &self.interjector { interjector.reset_budget(); }
            }
            Event::ToolCall { .. } => *self.used_tools.lock().unwrap() = true,
            Event::Done { .. } => *self.agent_state.lock().unwrap() = "idle".into(),
            _ => {}
        }
        if let Some(sender) = &self.interject_tx {
            let used_tools = *self.used_tools.lock().unwrap();
            let context = match &event {
                Event::ApprovalGranted { kind } => Some(desktop_interject_ctx(&self.soul, "approval_granted", kind)),
                Event::ApprovalDenied { kind } => Some(desktop_interject_ctx(&self.soul, "approval_denied", kind)),
                Event::Done { success, summary } if used_tools => Some(desktop_interject_ctx(
                    &self.soul, if *success { "task_done" } else { "task_failed" }, summary,
                )),
                Event::Verify { passed: false, detail } => Some(desktop_interject_ctx(&self.soul, "verify_fail", detail)),
                _ => None,
            };
            if let Some(context) = context { let _ = sender.send(context); }
        }
        if let Ok(value) = serde_json::to_value(&event) { let _ = self.app.emit("furina-event", value); }
        if matches!(event, Event::Done { .. }) { let _ = self.app.emit("furina-soul", serde_json::json!({})); }
    }
}

fn desktop_interject_ctx(soul: &Mutex<Soul>, kind: &str, detail: &str) -> InterjectCtx {
    let soul = soul.lock().unwrap();
    InterjectCtx {
        mood: soul.mood().as_str().to_string(),
        mood_label: soul.mood().label().to_string(),
        event_kind: kind.to_string(),
        action_detail: detail.to_string(),
        strategy: soul.expression_strategy(),
    }
}

struct DesktopApprover {
    app: tauri::AppHandle,
    approvals: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    next_id: Arc<Mutex<u64>>,
}

#[async_trait]
impl Approver for DesktopApprover {
    async fn confirm(&mut self, prompt: &str) -> bool {
        let id = {
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            next.to_string()
        };
        let (sender, receiver) = oneshot::channel();
        self.approvals.lock().unwrap().insert(id.clone(), sender);
        let _ = self.app.emit("furina-approval", serde_json::json!({ "id": id, "prompt": prompt }));
        receiver.await.unwrap_or(false)
    }
}

fn create_services(
    app_handle: &tauri::AppHandle,
    paths: &RuntimePaths,
    config: Config,
    strict: bool,
) -> anyhow::Result<RuntimeServices> {
    let mut errors = Vec::new();
    if strict {
        let _ = app::build_llm(&config)?;
        app::build_system_prompt(&paths.resource_root, &config.persona)?;
    }
    let voice = if config.voice.enabled {
        match VoiceClient::from_config(&config, &paths.voice_dir()) {
            Ok(client) => Some(Arc::new(client)),
            Err(error) => { errors.push(format!("TTS: {error}")); None }
        }
    } else { None };
    let asr = if config.asr.enabled {
        match AsrClient::from_config(&config) {
            Ok(client) => Some(Arc::new(client)),
            Err(error) => { errors.push(format!("ASR: {error}")); None }
        }
    } else { None };
    let (interjector, interject_tx) = if config.interject.enabled {
        match Interjector::from_config(&config) {
            Ok(interjector) => {
                let interjector = Arc::new(interjector);
                let (sender, mut receiver) = mpsc::unbounded_channel::<InterjectCtx>();
                let worker = interjector.clone();
                let worker_app = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    while let Some(context) = receiver.recv().await {
                        if let Some(text) = worker.line(context).await {
                            let event = serde_json::to_value(Event::Interjection { text }).unwrap_or_default();
                            let _ = worker_app.emit("furina-event", event);
                        }
                    }
                });
                (Some(interjector), Some(sender))
            }
            Err(error) if strict => return Err(error),
            Err(error) => { errors.push(format!("插话: {error}")); (None, None) }
        }
    } else { (None, None) };
    Ok(RuntimeServices {
        persona: config.persona.clone(),
        config,
        voice,
        asr,
        interjector,
        interject_tx,
        errors,
    })
}

async fn ensure_agent(state: &State<'_, AppState>) -> Result<(), String> {
    if state.agent.lock().map_err(|error| error.to_string())?.is_some() { return Ok(()); }
    let paths = state.paths.read().map_err(|error| error.to_string())?.clone();
    let (persona, interjector, interject_tx) = {
        let services = state.services.lock().map_err(|error| error.to_string())?;
        (services.persona.clone(), services.interjector.clone(), services.interject_tx.clone())
    };
    let sink: Arc<dyn EventSink> = Arc::new(DesktopSink {
        app: state.app.clone(),
        soul: state.soul.clone(),
        used_tools: Mutex::new(false),
        interjector,
        interject_tx,
        agent_state: state.agent_state.clone(),
    });
    let approver: Box<dyn Approver> = Box::new(DesktopApprover {
        app: state.app.clone(),
        approvals: state.approvals.clone(),
        next_id: state.next_approval_id.clone(),
    });
    let agent = app::build_agent(&paths, &persona, state.soul.clone(), sink, approver)
        .await
        .map_err(|error| error.to_string())?;
    *state.agent.lock().map_err(|error| error.to_string())? = Some(agent);
    Ok(())
}

#[tauri::command]
async fn chat_send(state: State<'_, AppState>, text: String) -> Result<(), String> {
    {
        let mut status = state.agent_state.lock().map_err(|error| error.to_string())?;
        if *status != "idle" { return Err("Furina 正在处理上一条消息".into()); }
        *status = "starting".into();
    }
    if let Err(error) = ensure_agent(&state).await {
        *state.agent_state.lock().unwrap() = "idle".into();
        return Err(error);
    }
    let mut agent = state.agent.lock().map_err(|error| error.to_string())?.take().ok_or("Agent 未初始化")?;
    let result = agent.run_task(&text).await;
    *state.agent.lock().map_err(|error| error.to_string())? = Some(agent);
    *state.agent_state.lock().unwrap() = "idle".into();
    let save_result = state.soul.lock().unwrap().save();
    let _ = state.app.emit("furina-soul", serde_json::json!({}));
    if let Err(error) = result {
        if let Err(save_error) = save_result {
            return Err(format!("{}；灵魂状态保存失败：{}", error, save_error));
        }
        return Err(error.to_string());
    }
    save_result.map_err(|error| format!("灵魂状态保存失败：{error}"))?;
    Ok(())
}

#[tauri::command]
async fn transcribe(state: State<'_, AppState>, audio: Vec<u8>, mime: String) -> Result<String, String> {
    let asr = state.services.lock().map_err(|error| error.to_string())?.asr.clone()
        .ok_or("语音识别未配置，请在首次设置中启用 ASR")?;
    asr.transcribe(audio, &mime).await.map_err(|error| error.to_string())
}

#[tauri::command]
async fn tts_synthesize(
    state: State<'_, AppState>,
    text: String,
    profile: Option<VoiceSynthesisProfile>,
    emotion: Option<String>,
    speed: Option<f64>,
) -> Result<serde_json::Value, String> {
    let voice = state.services.lock().map_err(|error| error.to_string())?.voice.clone()
        .ok_or("语音合成未配置，请在首次设置中启用 TTS")?;
    let format = voice.format().to_string();
    let profile = profile.unwrap_or_else(|| VoiceSynthesisProfile::legacy(
        emotion.as_deref().unwrap_or_default(),
        speed.unwrap_or(1.0),
    ));
    let data = voice
        .synthesize_bytes_with_profile(&text, &profile)
        .await
        .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "format": format, "data": data }))
}

#[tauri::command]
async fn stop_speaking() {}

#[tauri::command]
async fn approval_respond(state: State<'_, AppState>, id: String, ok: bool) -> Result<(), String> {
    if let Some(sender) = state.approvals.lock().unwrap().remove(&id) { let _ = sender.send(ok); }
    Ok(())
}

#[tauri::command]
async fn get_soul_state(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let paths = state.paths.read().map_err(|error| error.to_string())?.clone();
    let mut soul = state.soul.lock().unwrap();
    soul.advance_time(furina_soul::now_ms());
    let mood = soul.mood();
    let stage = soul.stage();
    let emotions = &soul.emotion;
    let recent_emotion_events: Vec<_> = soul
        .recent_emotion_events(3)
        .into_iter()
        .map(|event| serde_json::json!({
            "timestamp": event.timestamp,
            "source": event.source,
            "triggerId": event.trigger_id,
            "cause": event.cause,
            "moodBefore": event.mood_before,
            "moodAfter": event.mood_after,
            "intensityBefore": event.intensity_before,
            "intensityAfter": event.intensity_after,
            "deltas": event.deltas,
            "trend": event.trend,
            "unresolved": event.unresolved,
            "important": event.important,
        }))
        .collect();
    let recent_emotional_memories: Vec<_> = soul
        .recent_emotional_memories(2)
        .into_iter()
        .map(|record| serde_json::json!({
            "id": record.id,
            "content": record.content,
            "importance": record.importance_score,
            "valence": record.valence,
            "tags": record.tags,
            "emotion": record.emotion,
        }))
        .collect();
    let root_name = paths.workspace_root.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_else(|| "workspace".into());
    let valence = emotion_valence(emotions);
    let arousal = emotion_arousal(emotions);
    let intensity = (soul.affect.intensity / 100.0).clamp(0.0, 1.0);
    Ok(serde_json::json!({
        "schema_version": "1.2",
        "timestamp": furina_soul::now_ms(),
        "workspace": {
            "root": paths.data_root.display().to_string(),
            "current_ws": paths.workspace_root.display().to_string(),
            "memory_scope": format!("workspace_{root_name}"),
            "active_memory_count": soul.memory.records.len(),
        },
        "mood": mood.as_str(),
        "mood_label": mood.label(),
        "intensity": intensity,
        "emotion_profile": {
            "valence": valence,
            "arousal": arousal,
            "intensity": intensity,
            "trend": soul.affect.trend,
        },
        "affect": {
            "primary": soul.affect.primary,
            "secondary": soul.affect.secondary,
            "intensity": soul.affect.intensity,
            "trend": soul.affect.trend,
            "recoverAfter": soul.affect.recover_after,
            "conflictLevel": soul.affect.conflict_level,
            "unresolved": soul.affect.unresolved,
            "repairProgress": soul.affect.repair_progress,
        },
        "stage": { "id": stage.id, "label": stage.label, "trust": emotions.trust, "hint": stage.hint },
        "emotions": {
            "confidence": emotions.confidence, "trust": emotions.trust, "attachment": emotions.attachment,
            "energy": emotions.energy, "stress": emotions.stress, "pride": emotions.pride,
        },
        "recent_emotion_events": recent_emotion_events,
        "recent_emotional_memories": recent_emotional_memories,
        "memory_count": soul.memory.records.len(),
        "last_intent": soul.last_intent.as_ref().map(|intent| serde_json::json!({
            "intent": intent.intent, "cause": intent.cause, "value": intent.value,
        })),
        "agent_status": { "state": *state.agent_state.lock().unwrap(), "action": null, "detail": null },
        "interaction_count": soul.relationship.interaction_count,
    }))
}

fn emotion_valence(emotions: &furina_soul::EmotionState) -> f64 {
    ((
        (emotions.confidence - 45.0)
            + (emotions.trust - 20.0)
            + (emotions.attachment - 10.0)
            + (emotions.pride - 40.0)
            - (emotions.stress - 20.0) * 1.5
    ) / 200.0)
        .clamp(-1.0, 1.0)
}

fn emotion_arousal(emotions: &furina_soul::EmotionState) -> f64 {
    ((emotions.energy + emotions.stress) / 200.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod emotion_profile_tests {
    use super::{emotion_arousal, emotion_valence};
    use furina_soul::EmotionState;

    #[test]
    fn valence_is_neutral_at_baseline_and_negative_under_stress() {
        let baseline = EmotionState {
            confidence: 45.0,
            trust: 20.0,
            attachment: 10.0,
            energy: 60.0,
            stress: 20.0,
            pride: 40.0,
            updated_ms: 0,
        };
        let mut stressed = baseline.clone();
        stressed.stress = 90.0;
        assert_eq!(emotion_valence(&baseline), 0.0);
        assert!(emotion_valence(&stressed) < 0.0);
    }

    #[test]
    fn arousal_stays_in_range() {
        let emotions = EmotionState {
            confidence: 0.0,
            trust: 0.0,
            attachment: 0.0,
            energy: 100.0,
            stress: 100.0,
            pride: 0.0,
            updated_ms: 0,
        };
        assert_eq!(emotion_arousal(&emotions), 1.0);
    }
}

#[tauri::command]
async fn get_memories(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let mut soul = state.soul.lock().unwrap();
    soul.advance_time(furina_soul::now_ms());
    let records: Vec<_> = soul.memory.records.iter().rev().take(30).map(|record| serde_json::json!({
        "id": record.id,
        "type": record.kind,
        "content": record.content,
        "importance": record.importance_score,
        "valence": record.valence,
        "tags": record.tags,
        "emotion": record.emotion,
    })).collect();
    Ok(serde_json::json!(records))
}
fn avatar_asset_path(data_root: &Path) -> PathBuf { data_root.join(".furina/avatar/Furina.vrm") }

fn validate_avatar_asset(data_root: &Path) -> Result<(PathBuf, fs::Metadata), String> {
    let path = avatar_asset_path(data_root);
    let metadata = migration::validate_vrm_file(&path).map_err(|error| error.to_string())?;
    let avatar_root = data_root.join(".furina/avatar").canonicalize().map_err(|error| error.to_string())?;
    let canonical = path.canonicalize().map_err(|error| error.to_string())?;
    if !canonical.starts_with(avatar_root) { return Err("Avatar 资产路径越过了受信任目录".into()); }
    Ok((canonical, metadata))
}

#[tauri::command]
async fn get_avatar_asset_info(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let data_root = state.paths.read().map_err(|error| error.to_string())?.data_root.clone();
    let path = avatar_asset_path(&data_root);
    if !path.exists() {
        return Ok(serde_json::json!({ "available": false, "fileName": "Furina.vrm", "maxBytes": MAX_AVATAR_BYTES }));
    }
    let (_, metadata) = validate_avatar_asset(&data_root)?;
    Ok(serde_json::json!({
        "available": true, "fileName": "Furina.vrm", "sizeBytes": metadata.len(), "maxBytes": MAX_AVATAR_BYTES,
    }))
}

#[tauri::command]
async fn load_avatar_asset(state: State<'_, AppState>) -> Result<Response, String> {
    let data_root = state.paths.read().map_err(|error| error.to_string())?.data_root.clone();
    let (path, _) = validate_avatar_asset(&data_root)?;
    let bytes = tokio::fs::read(path).await.map_err(|error| format!("无法载入 Avatar 资产: {error}"))?;
    Ok(Response::new(bytes))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupPayload {
    llm_base_url: String,
    llm_model: String,
    #[serde(default)]
    llm_api_key: String,
    voice_enabled: bool,
    voice_provider: String,
    #[serde(default)]
    voice_protocol: String,
    #[serde(default)]
    voice_endpoint: String,
    #[serde(default)]
    voice_model: String,
    #[serde(default)]
    voice_voice: String,
    #[serde(default)]
    voice_format: String,
    #[serde(default)]
    voice_api_key_env: String,
    #[serde(default)]
    voice_api_key: String,
    #[serde(default)]
    voice_reference_id: String,
    asr_enabled: bool,
    asr_provider: String,
    #[serde(default)]
    asr_protocol: String,
    #[serde(default)]
    asr_endpoint: String,
    #[serde(default)]
    asr_model: String,
    #[serde(default)]
    asr_prompt: String,
    #[serde(default)]
    asr_language: String,
    #[serde(default)]
    asr_api_key_env: String,
    #[serde(default)]
    asr_api_key: String,
    #[serde(default)]
    qwen_api_key: String,
    #[serde(default)]
    emotion_classifier_enabled: bool,
    #[serde(default)]
    emotion_classifier_provider_id: String,
    #[serde(default)]
    emotion_classifier_model: String,
    #[serde(default = "default_emotion_classifier_timeout_ms")]
    emotion_classifier_timeout_ms: u64,
    #[serde(default = "default_emotion_classifier_max_tokens")]
    emotion_classifier_max_tokens: usize,
    workspace: String,
}

fn default_emotion_classifier_timeout_ms() -> u64 { 1500 }

fn default_emotion_classifier_max_tokens() -> usize { 256 }


fn setup_status_value(state: &AppState) -> Result<serde_json::Value, String> {
    let paths = state.paths.read().map_err(|error| error.to_string())?.clone();
    let services = state.services.lock().map_err(|error| error.to_string())?;
    let preferences = runtime::load_preferences(&paths.data_root);
    let provider = services.config.active_provider().ok();
    let llm_ready = app::build_llm(&services.config).is_ok();
    let voice_protocol = display_voice_protocol(&services.config);
    let voice_endpoint = if voice_protocol == "qwen_omni" && services.config.voice.protocol.trim().is_empty() {
        services.config.qwen.base_url.clone()
    } else { services.config.voice.endpoint.clone() };
    let voice_model = if voice_protocol == "qwen_omni" && services.config.voice.protocol.trim().is_empty() {
        services.config.qwen.tts_model.clone()
    } else { services.config.voice.model.clone() };
    let voice_name = if voice_protocol == "qwen_omni" && services.config.voice.protocol.trim().is_empty() {
        services.config.qwen.tts_voice.clone()
    } else { services.config.voice.voice.clone() };
    let voice_key_env = if voice_protocol == "qwen_omni" && services.config.voice.protocol.trim().is_empty() {
        services.config.qwen.api_key_env.clone()
    } else { services.config.voice.api_key_env.clone() };
    let asr_protocol = display_asr_protocol(&services.config);
    let asr_endpoint = if asr_protocol == "qwen_omni" && services.config.asr.protocol.trim().is_empty() {
        services.config.qwen.base_url.clone()
    } else { services.config.asr.endpoint.clone() };
    let asr_model = if asr_protocol == "qwen_omni" && services.config.asr.protocol.trim().is_empty() {
        services.config.qwen.asr_model.clone()
    } else { services.config.asr.model.clone() };
    let asr_prompt = if asr_protocol == "qwen_omni" && services.config.asr.prompt.trim().is_empty() {
        services.config.qwen.asr_prompt.clone()
    } else { services.config.asr.prompt.clone() };
    let asr_key_env = if asr_protocol == "qwen_omni" && services.config.asr.protocol.trim().is_empty() {
        services.config.qwen.api_key_env.clone()
    } else { services.config.asr.api_key_env.clone() };
    let emotion_classifier_provider = services.config.emotion_classifier.provider_id.trim();
    let emotion_classifier_model = services.config.emotion_classifier.model.trim();
    let emotion_classifier_provider_config = services.config.provider(emotion_classifier_provider);
    let emotion_classifier_ready = !services.config.emotion_classifier.enabled
        || (!emotion_classifier_provider.is_empty()
            && !emotion_classifier_model.is_empty()
            && emotion_classifier_provider_config
                .as_ref()
                .map(|provider| std::env::var_os(&provider.api_key_env).is_some())
                .unwrap_or(false));
    Ok(serde_json::json!({
        "setupCompleted": preferences.setup_completed,
        "needsSetup": !preferences.setup_completed || !llm_ready,
        "runtime": *state.runtime_info.lock().map_err(|error| error.to_string())?,
        "llm": {
            "ready": llm_ready,
            "baseUrl": provider.as_ref().map(|value| value.base_url.clone()).unwrap_or_default(),
            "model": provider.as_ref().map(|value| value.model.clone()).unwrap_or_default(),
            "apiKeyConfigured": provider.as_ref().map(|value| std::env::var_os(&value.api_key_env).is_some()).unwrap_or(false),
        },
        "voice": {
            "enabled": services.config.voice.enabled,
            "ready": services.voice.is_some(),
            "provider": services.config.voice.provider,
            "protocol": voice_protocol,
            "endpoint": voice_endpoint,
            "model": voice_model,
            "voice": voice_name,
            "referenceId": services.config.voice.reference_id,
            "format": services.config.voice.format,
            "apiKeyEnv": voice_key_env,
            "apiKeyConfigured": std::env::var_os(&voice_key_env).is_some(),
        },
        "asr": {
            "enabled": services.config.asr.enabled,
            "ready": services.asr.is_some(),
            "provider": services.config.asr.provider,
            "protocol": asr_protocol,
            "endpoint": asr_endpoint,
            "model": asr_model,
            "prompt": asr_prompt,
            "language": services.config.asr.language,
            "apiKeyEnv": asr_key_env,
            "apiKeyConfigured": std::env::var_os(&asr_key_env).is_some(),
        },
        "emotionClassifier": {
            "enabled": services.config.emotion_classifier.enabled,
            "providerId": services.config.emotion_classifier.provider_id,
            "model": services.config.emotion_classifier.model,
            "timeoutMs": services.config.emotion_classifier.timeout_ms,
            "maxTokens": services.config.emotion_classifier.max_tokens,
            "ready": emotion_classifier_ready,
        },
        "avatar": { "available": avatar_asset_path(&paths.data_root).is_file() },
        "sidecar": { "available": paths.sidecar.available(), "description": paths.sidecar.description() },
        "migrationCompleted": paths.data_root.join(".furina/migration.json").is_file(),
        "errors": services.errors,
    }))
}

#[tauri::command]
async fn get_setup_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> { setup_status_value(&state) }

fn ensure_reload_allowed(state: &AppState) -> Result<(), String> {
    if *state.agent_state.lock().map_err(|error| error.to_string())? != "idle" {
        return Err("Agent 正在工作，完成当前任务后再重新加载设置".into());
    }
    if !state.approvals.lock().map_err(|error| error.to_string())?.is_empty() {
        return Err("仍有工具审批等待处理，暂时不能重新加载设置".into());
    }
    Ok(())
}

fn reload_runtime_inner(state: &AppState) -> Result<(), String> {
    ensure_reload_allowed(state)?;
    let paths = state.paths.read().map_err(|error| error.to_string())?.clone();
    app::reload_secrets_env(&paths.data_root);
    let config = Config::load(&paths.config_path()).map_err(|error| error.to_string())?;
    let services = create_services(&state.app, &paths, config, true).map_err(|error| error.to_string())?;
    *state.agent.lock().map_err(|error| error.to_string())? = None;
    *state.services.lock().map_err(|error| error.to_string())? = services;
    let _ = state.app.emit("furina-runtime-reloaded", serde_json::json!({}));
    Ok(())
}

#[tauri::command]
async fn reload_runtime(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    reload_runtime_inner(&state)?;
    setup_status_value(&state)
}

#[tauri::command]
async fn save_setup(state: State<'_, AppState>, setup: SetupPayload) -> Result<serde_json::Value, String> {
    ensure_reload_allowed(&state)?;
    if setup.llm_base_url.trim().is_empty() || setup.llm_model.trim().is_empty() {
        return Err("LLM 地址和模型不能为空".into());
    }
    let workspace = PathBuf::from(setup.workspace.trim());
    if !workspace.is_absolute() { return Err("工作区必须使用绝对路径".into()); }
    fs::create_dir_all(&workspace).map_err(|error| format!("无法创建工作区: {error}"))?;
    let old_paths = state.paths.read().map_err(|error| error.to_string())?.clone();
    let config_path = old_paths.config_path();
    let secrets_path = old_paths.secrets_path();
    let old_config = fs::read(&config_path).ok();
    let old_secrets = fs::read(&secrets_path).ok();
    let old_preferences = runtime::load_preferences(&old_paths.data_root);
    let mut config = Config::load(&config_path).unwrap_or_default();
    config.model = setup.llm_model.trim().to_string();
    config.llm.active_provider = Some("desktop".into());
    config.llm.providers = vec![ProviderConfig {
        id: "desktop".into(), label: "Desktop Provider".into(), base_url: setup.llm_base_url.trim().to_string(),
        api_key_env: "FURINA_API_KEY".into(), model: setup.llm_model.trim().to_string(), vision: false,
    }];
    config.voice.enabled = setup.voice_enabled;
    config.voice.provider = setup.voice_provider.trim().to_string();
    config.voice.protocol = normalize_voice_protocol(&setup.voice_protocol, &setup.voice_provider)?;
    config.voice.endpoint = setup.voice_endpoint.trim().to_string();
    config.voice.model = setup.voice_model.trim().to_string();
    config.voice.voice = setup.voice_voice.trim().to_string();
    config.voice.format = default_if_empty(&setup.voice_format, "mp3");
    config.voice.api_key_env = validate_key_env(&setup.voice_api_key_env, "FURINA_TTS_API_KEY")?;
    config.voice.reference_id = setup.voice_reference_id.trim().to_string();
    validate_voice_setup(&config)?;
    config.asr.enabled = setup.asr_enabled;
    config.asr.provider = setup.asr_provider.trim().to_string();
    config.asr.protocol = normalize_asr_protocol(&setup.asr_protocol, &setup.asr_provider)?;
    config.asr.endpoint = setup.asr_endpoint.trim().to_string();
    config.asr.model = setup.asr_model.trim().to_string();
    config.asr.prompt = setup.asr_prompt.trim().to_string();
    config.asr.language = default_if_empty(&setup.asr_language, "zh");
    config.asr.api_key_env = validate_key_env(&setup.asr_api_key_env, "FURINA_ASR_API_KEY")?;
    validate_asr_setup(&config)?;
    let classifier_timeout_ms = setup.emotion_classifier_timeout_ms.clamp(500, 1500);
    let classifier_max_tokens = setup.emotion_classifier_max_tokens.clamp(32, 512);
    if setup.emotion_classifier_enabled {
        if setup.emotion_classifier_provider_id.trim().is_empty() {
            return Err("启用情绪分类器时必须选择 LLM 提供方".into());
        }
        if setup.emotion_classifier_model.trim().is_empty() {
            return Err("启用情绪分类器时必须填写模型名称".into());
        }
        if config.provider(setup.emotion_classifier_provider_id.trim()).is_none() {
            return Err(format!("找不到情绪分类器提供方: {}", setup.emotion_classifier_provider_id.trim()));
        }
    }
    config.emotion_classifier = EmotionClassifierConfig {
        enabled: setup.emotion_classifier_enabled,
        provider_id: setup.emotion_classifier_provider_id.trim().to_string(),
        model: setup.emotion_classifier_model.trim().to_string(),
        timeout_ms: classifier_timeout_ms,
        max_tokens: classifier_max_tokens,
    };
    runtime::atomic_write(&config_path, serde_yaml::to_string(&config).map_err(|error| error.to_string())?.as_bytes())
        .map_err(|error| error.to_string())?;
    let mut secrets = read_secrets_map(&secrets_path);
    update_secret(&mut secrets, "FURINA_API_KEY", &setup.llm_api_key)?;
    update_secret(&mut secrets, &config.voice.api_key_env, &setup.voice_api_key)?;
    update_secret(&mut secrets, &config.asr.api_key_env, &setup.asr_api_key)?;
    if !setup.qwen_api_key.trim().is_empty() {
        update_secret(&mut secrets, "QWEN_API_KEY", &setup.qwen_api_key)?;
    }
    write_secrets_map(&secrets_path, &secrets).map_err(|error| error.to_string())?;
    let preferences = DesktopPreferences { workspace: workspace.display().to_string(), setup_completed: true };
    runtime::save_preferences(&old_paths.data_root, &preferences).map_err(|error| error.to_string())?;
    {
        let mut paths = state.paths.write().map_err(|error| error.to_string())?;
        paths.workspace_root = workspace.clone();
    }
    state.runtime_info.lock().map_err(|error| error.to_string())?.workspace_root = workspace.display().to_string();
    if let Err(error) = reload_runtime_inner(&state) {
        restore_file(&config_path, old_config.as_deref());
        restore_file(&secrets_path, old_secrets.as_deref());
        let _ = runtime::save_preferences(&old_paths.data_root, &old_preferences);
        *state.paths.write().unwrap() = old_paths.clone();
        state.runtime_info.lock().unwrap().workspace_root = old_paths.workspace_root.display().to_string();
        app::reload_secrets_env(&old_paths.data_root);
        return Err(error);
    }
    setup_status_value(&state)
}

fn default_if_empty(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() { fallback.into() } else { value.into() }
}

fn validate_key_env(value: &str, fallback: &str) -> Result<String, String> {
    let value = default_if_empty(value, fallback);
    let mut chars = value.chars();
    let valid_first = chars.next().map(|ch| ch == '_' || ch.is_ascii_alphabetic()).unwrap_or(false);
    if !valid_first || !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
        return Err("API Key 环境变量名只能包含英文字母、数字和下划线，且不能以数字开头".into());
    }
    Ok(value)
}

fn normalize_voice_protocol(protocol: &str, provider: &str) -> Result<String, String> {
    let value = if protocol.trim().is_empty() {
        match provider.trim().to_lowercase().as_str() {
            "fish" => "fish",
            "qwen" => "qwen_omni",
            _ => "openai",
        }
    } else { protocol.trim() };
    match value.to_lowercase().as_str() {
        "fish" | "fish_tts" => Ok("fish".into()),
        "qwen" | "qwen_omni" | "openai_chat_audio" => Ok("qwen_omni".into()),
        "openai" | "openai_speech" => Ok("openai".into()),
        _ => Err("TTS 协议必须是 fish、qwen_omni 或 openai".into()),
    }
}

fn normalize_asr_protocol(protocol: &str, provider: &str) -> Result<String, String> {
    let value = if protocol.trim().is_empty() {
        match provider.trim().to_lowercase().as_str() {
            "fish" => "fish",
            "qwen" => "qwen_omni",
            _ => "openai",
        }
    } else { protocol.trim() };
    match value.to_lowercase().as_str() {
        "fish" | "fish_asr" => Ok("fish".into()),
        "qwen" | "qwen_omni" | "openai_chat_audio" => Ok("qwen_omni".into()),
        "openai" | "openai_transcriptions" => Ok("openai".into()),
        _ => Err("ASR 协议必须是 fish、qwen_omni 或 openai".into()),
    }
}

fn validate_http_endpoint(value: &str, label: &str) -> Result<(), String> {
    let lower = value.trim().to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err(format!("{label}必须以 http:// 或 https:// 开头"));
    }
    Ok(())
}

fn validate_voice_setup(config: &Config) -> Result<(), String> {
    if !config.voice.enabled { return Ok(()); }
    validate_http_endpoint(&config.voice.endpoint, "TTS API 地址")?;
    if config.voice.model.trim().is_empty() { return Err("启用 TTS 时模型不能为空".into()); }
    if config.voice.protocol == "openai" && config.voice.voice.trim().is_empty() {
        return Err("OpenAI TTS 启用时音色不能为空".into());
    }
    Ok(())
}

fn validate_asr_setup(config: &Config) -> Result<(), String> {
    if !config.asr.enabled { return Ok(()); }
    validate_http_endpoint(&config.asr.endpoint, "ASR API 地址")?;
    if config.asr.protocol != "fish" && config.asr.model.trim().is_empty() {
        return Err("启用 ASR 时模型不能为空".into());
    }
    Ok(())
}

fn display_voice_protocol(config: &Config) -> String {
    normalize_voice_protocol(&config.voice.protocol, &config.voice.provider).unwrap_or_else(|_| config.voice.protocol.clone())
}

fn display_asr_protocol(config: &Config) -> String {
    normalize_asr_protocol(&config.asr.protocol, &config.asr.provider).unwrap_or_else(|_| config.asr.protocol.clone())
}

fn read_secrets_map(path: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(path).ok().map(|text| app::parse_secrets_text(&text).into_iter().collect()).unwrap_or_default()
}

fn update_secret(secrets: &mut BTreeMap<String, String>, key: &str, value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.contains(['\r', '\n']) { return Err(format!("{key} 包含非法换行符")); }
    if !value.is_empty() { secrets.insert(key.into(), value.into()); }
    Ok(())
}

fn write_secrets_map(path: &Path, secrets: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let text = secrets.iter().map(|(key, value)| format!("{key}={value}\n")).collect::<String>();
    runtime::atomic_write(path, text.as_bytes())
}

fn restore_file(path: &Path, bytes: Option<&[u8]>) {
    match bytes {
        Some(bytes) => { let _ = runtime::atomic_write(path, bytes); }
        None => { let _ = fs::remove_file(path); }
    }
}

#[tauri::command]
async fn validate_provider(state: State<'_, AppState>, kind: String) -> Result<String, String> {
    let paths = state.paths.read().map_err(|error| error.to_string())?.clone();
    app::reload_secrets_env(&paths.data_root);
    let config = Config::load(&paths.config_path()).map_err(|error| error.to_string())?;
    match kind.as_str() {
        "llm" => app::build_llm(&config).map_err(|error| error.to_string())?.ping().await.map_err(|error| error.to_string()),
        "tts" => { VoiceClient::from_config(&config, &paths.voice_dir()).map_err(|error| error.to_string())?; Ok("TTS 配置有效".into()) },
        "asr" => { AsrClient::from_config(&config).map_err(|error| error.to_string())?; Ok("ASR 配置有效".into()) },
        "emotion_classifier" => {
            let classifier = &config.emotion_classifier;
            if !classifier.enabled {
                return Ok("情绪分类器未启用".into());
            }
            if classifier.provider_id.trim().is_empty() || classifier.model.trim().is_empty() {
                return Err("情绪分类器需要提供方和模型".into());
            }
            let mut provider = config
                .provider(classifier.provider_id.trim())
                .ok_or_else(|| format!("找不到情绪分类器提供方: {}", classifier.provider_id))?;
            provider.model = classifier.model.clone();
            let mut probe_config = config.clone();
            probe_config.llm.active_provider = Some(provider.id.clone());
            probe_config.llm.providers = vec![provider];
            app::build_llm(&probe_config)
                .map_err(|error| error.to_string())?
                .ping()
                .await
                .map(|_| format!("情绪分类器连接正常（{}）", classifier.model))
                .map_err(|error| error.to_string())
        }
        _ => Err("未知的 provider 类型".into()),
    }
}

#[tauri::command]
async fn import_avatar(state: State<'_, AppState>, source_path: String) -> Result<serde_json::Value, String> {
    let data_root = state.paths.read().map_err(|error| error.to_string())?.data_root.clone();
    let size = migration::import_avatar(Path::new(&source_path), &data_root).map_err(|error| error.to_string())?;
    let _ = state.app.emit("furina-avatar-changed", serde_json::json!({ "sizeBytes": size }));
    Ok(serde_json::json!({ "available": true, "fileName": "Furina.vrm", "sizeBytes": size }))
}

#[tauri::command]
async fn detect_legacy_data(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let paths = state.paths.read().map_err(|error| error.to_string())?.clone();
    let candidates = tokio::task::spawn_blocking(move || migration::detect_legacy_roots(&paths.resource_root, &paths.data_root))
        .await.map_err(|error| error.to_string())?;
    Ok(serde_json::json!(candidates))
}

#[tauri::command]
async fn migrate_legacy_data(state: State<'_, AppState>, source_root: String) -> Result<serde_json::Value, String> {
    ensure_reload_allowed(&state)?;
    let data_root = state.paths.read().map_err(|error| error.to_string())?.data_root.clone();
    let source = PathBuf::from(source_root);
    let result = tokio::task::spawn_blocking({
        let data_root = data_root.clone();
        move || migration::migrate_legacy_data(&source, &data_root)
    }).await.map_err(|error| error.to_string())?.map_err(|error| error.to_string())?;
    *state.soul.lock().unwrap() = Soul::load(app::soul_dir(&data_root));
    reload_runtime_inner(&state)?;
    let _ = state.app.emit("furina-avatar-changed", serde_json::json!({}));
    let _ = state.app.emit("furina-soul", serde_json::json!({}));
    Ok(result)
}

#[tauri::command]
async fn doctor(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let paths = state.paths.read().map_err(|error| error.to_string())?.clone();
    let (persona, voice_ready, asr_ready, asr_provider, service_errors) = {
        let services = state.services.lock().map_err(|error| error.to_string())?;
        (
            services.persona.clone(),
            services.voice.is_some(),
            services.asr.is_some(),
            services.asr.as_ref().map(|client| client.provider().to_string()),
            services.errors.clone(),
        )
    };
    let git_available = std::process::Command::new("git").arg("--version").output().map(|output| output.status.success()).unwrap_or(false);
    let sidecar_available = paths.sidecar.available();
    let (sidecar_running, sidecar_version, sidecar_error) = if sidecar_available {
        let probe = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            Sidecar::spawn(&paths.sidecar, &paths.workspace_root.display().to_string(), Arc::new(DoctorSink)),
        ).await;
        match probe {
            Ok(Ok(sidecar)) => (true, sidecar.version().to_string(), None),
            Ok(Err(error)) => (false, String::new(), Some(error.to_string())),
            Err(_) => (false, String::new(), Some("sidecar 启动探测超时".to_string())),
        }
    } else {
        (false, String::new(), Some("sidecar 文件不存在，工具能力已降级".to_string()))
    };
    Ok(serde_json::json!({
        "root": paths.data_root.display().to_string(),
        "resource_root": paths.resource_root.display().to_string(),
        "workspace": paths.workspace_root.display().to_string(),
        "persona": persona,
        "voice": voice_ready,
        "asr": asr_ready,
        "asr_provider": asr_provider,
        "sidecar": {
            "available": sidecar_available,
            "running": sidecar_running,
            "version": sidecar_version,
            "error": sidecar_error,
            "launch": paths.sidecar.description(),
            "protocol": "json-rpc-stdio"
        },
        "git": git_available,
        "runtime": *state.runtime_info.lock().map_err(|error| error.to_string())?,
        "errors": service_errors,
    }))
}

struct DoctorSink;

impl EventSink for DoctorSink {
    fn emit(&self, _event: Event) {}
}

fn run_disabled_sidecar() {
    use std::io::{BufRead, Write};
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) else { continue; };
        let id = request.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = request.get("method").and_then(|value| value.as_str()).unwrap_or("");
        let response = match method {
            "initialize" => serde_json::json!({ "id": id, "result": { "ok": true, "version": "disabled", "workspace": request["params"]["workspace_root"] } }),
            "tools.list" => serde_json::json!({ "id": id, "result": { "tools": [] } }),
            _ => serde_json::json!({ "id": id, "error": { "code": -32020, "message": "工具 sidecar 不可用，当前仅支持对话" } }),
        };
        let _ = writeln!(stdout, "{}", response);
        let _ = stdout.flush();
    }
}

fn main() {
    if std::env::args().any(|argument| argument == "--furina-disabled-sidecar") {
        run_disabled_sidecar();
        return;
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app_instance| {
            let resolved = runtime::resolve(app_instance.handle())?;
            app::load_secrets_env(&resolved.paths.data_root);
            let config = Config::load(&resolved.paths.config_path()).unwrap_or_default();
            let services = create_services(app_instance.handle(), &resolved.paths, config, false)?;
            let soul = Arc::new(Mutex::new(Soul::load(resolved.paths.soul_dir())));
            let lock = InstanceLock::acquire(&resolved.paths.soul_dir())?;
            app_instance.manage(AppState {
                _lock: lock,
                app: app_instance.handle().clone(),
                paths: Arc::new(RwLock::new(resolved.paths)),
                runtime_info: Mutex::new(resolved.info),
                soul,
                agent: Mutex::new(None),
                services: Mutex::new(services),
                agent_state: Arc::new(Mutex::new("idle".into())),
                approvals: Arc::new(Mutex::new(HashMap::new())),
                next_approval_id: Arc::new(Mutex::new(0)),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            chat_send, transcribe, tts_synthesize, stop_speaking, approval_respond, get_soul_state,
            get_memories, get_avatar_asset_info, load_avatar_asset, doctor, get_setup_status, save_setup,
            validate_provider, reload_runtime, import_avatar, detect_legacy_data, migrate_legacy_data,
        ])
        .run(tauri::generate_context!())
        .expect("Furina Desktop 启动失败");
}
