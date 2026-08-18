import React, { lazy, Suspense, useEffect, useReducer, useRef, useState } from "react";
import { invoke, listen } from "./lib/tauri.js";
import { StreamBuffer } from "./lib/streaming.js";
import { projectEmotions } from "./lib/projection.js";
import { PcmRecorder } from "./lib/asr.js";
import { TtsPipeline } from "./lib/tts.js";
import { VoiceEmotionController } from "./lib/voiceEmotion.js";
import { moodReactionFor } from "./avatar/avatarBehavior.js";
import { conversationStateFor } from "./avatar/avatarInteraction.js";

const AvatarStage = lazy(() => import("./avatar/AvatarStage.jsx"));
const SetupWizard = lazy(() => import("./SetupWizard.jsx"));

const MOOD_LABELS = {
  calm: "淡定",
  happy: "开心",
  proud: "得意",
  hurt: "委屈",
  sad: "难过",
  annoyed: "恼火",
};
const EMOTION_DIMS = [
  ["confidence", "自信"],
  ["trust", "信任"],
  ["attachment", "依恋"],
  ["energy", "精力"],
  ["stress", "压力"],
  ["pride", "骄傲"],
];

export default function App() {
  // ---------- 消息列表（ref 可变 + tick 触发渲染） ----------
  const msgsRef = useRef([]);
  const [, tick] = useReducer((x) => x + 1, 0);
  const pushMsg = (m) => {
    msgsRef.current.push(m);
    tick();
  };

  // ---------- 状态 ----------
  const [soul, setSoul] = useState(null);
  const [thinking, setThinking] = useState(false);
  const [tab, setTab] = useState("chat");
  const [voiceOn, setVoiceOn] = useState(true);
  const [speed, setSpeed] = useState(1.0);
  const [approval, setApproval] = useState(null);
  const [recording, setRecording] = useState(false);
  const [speaking, setSpeaking] = useState(false);
  const [silentTalking, setSilentTalking] = useState(false);
  const [toolLog, setToolLog] = useState([]);
  const [memories, setMemories] = useState([]);
  const [setupStatus, setSetupStatus] = useState(null);
  const [showSetup, setShowSetup] = useState(false);

  const speedRef = useRef(1.0);
  const soulRef = useRef(null);
  const avatarRef = useRef(null);
  const voiceOnRef = useRef(voiceOn);
  const speakingRef = useRef(false);
  const silentTalkingRef = useRef(false);
  const voiceEmotionRef = useRef(new VoiceEmotionController());
  const inputActivityTimerRef = useRef(null);
  const previousMoodRef = useRef(null);
  useEffect(() => {
    speedRef.current = speed;
  }, [speed]);
  useEffect(() => {
    voiceOnRef.current = voiceOn;
  }, [voiceOn]);
  useEffect(() => {
    soulRef.current = soul;
  }, [soul]);

  useEffect(() => {
    let cancelled = false;
    invoke("get_setup_status")
      .then((status) => {
        if (cancelled) return;
        setSetupStatus(status);
        if (status?.needsSetup) setShowSetup(true);
      })
      .catch((error) => pushMsg({ kind: "status", text: "⚠️ 设置状态读取失败：" + error }));
    return () => { cancelled = true; };
  }, []);

  const streamRef = useRef(new StreamBuffer());
  const currentAssistantRef = useRef(null);
  const chatRef = useRef(null);

  // ---------- TTS 管线 ----------
  const ttsRef = useRef(null);
  useEffect(() => {
    ttsRef.current = new TtsPipeline({
      invoke,
      voiceProfileFor: () => voiceEmotionRef.current.next(soulRef.current, speedRef.current),
      onStatus: (s) => pushMsg({ kind: "status", text: s }),
      onSpeaking: setTtsSpeaking,
      onAudioLevel: (level) => avatarRef.current?.setAudioLevel(level),
    });
    return () => ttsRef.current?.dispose();
  }, []);
  useEffect(() => {
    if (ttsRef.current) ttsRef.current.enabled = voiceOn;
    if (!voiceOn) voiceEmotionRef.current.reset();
    if (voiceOn && silentTalkingRef.current) {
      silentTalkingRef.current = false;
      setSilentTalking(false);
      avatarRef.current?.talk(speakingRef.current);
    }
  }, [voiceOn]);

  function setTtsSpeaking(active) {
    speakingRef.current = active;
    setSpeaking(active);
    avatarRef.current?.talk(active || silentTalkingRef.current);
  }

  function setSilentTalkingActive(active) {
    silentTalkingRef.current = active;
    setSilentTalking(active);
    avatarRef.current?.talk(active || speakingRef.current);
  }

  // ---------- 录音 ----------
  const recorderRef = useRef(
    new PcmRecorder({ onStatus: (s) => pushMsg({ kind: "status", text: s }) })
  );

  // ---------- 流式渲染 ----------
  function appendAssistant(s) {
    const arr = msgsRef.current;
    const last = arr[arr.length - 1];
    const startsNewAssistant = !currentAssistantRef.current || !last || last.kind !== "assistant";
    const wasEmpty = startsNewAssistant || !currentAssistantRef.current.text;
    if (startsNewAssistant) {
      const m = { kind: "assistant", text: "" };
      arr.push(m);
      currentAssistantRef.current = m;
    }
    currentAssistantRef.current.text +=
      (currentAssistantRef.current.text ? "\n" : "") + s;
    tick();
    if (wasEmpty) {
      setThinking(false);
      avatarRef.current?.think(false);
      if (!voiceOnRef.current) {
        setSilentTalkingActive(true);
      }
    }
    ttsRef.current?.speak(s);
  }

  // ---------- 事件流 ----------
  const refreshSoul = async () => {
    try {
      setSoul(await invoke("get_soul_state"));
    } catch (_) {}
  };

  useEffect(() => {
    const handleEvent = (ev) => {
      switch (ev.type) {
        case "session_started":
          streamRef.current.reset();
          currentAssistantRef.current = null;
          setThinking(true);
          ttsRef.current?.stop();
          ttsRef.current?.beginResponse();
          voiceEmotionRef.current.reset();
          setSilentTalkingActive(false);
          avatarRef.current?.listen(false);
          avatarRef.current?.talk(false);
          avatarRef.current?.think(true);
          break;
        case "message_delta":
          for (const s of streamRef.current.feed(ev.content || ""))
            appendAssistant(s);
          break;
        case "message":
          if (ev.role === "assistant") {
            for (const s of streamRef.current.flush()) appendAssistant(s);
            ttsRef.current?.finishResponse();
          }
          break;
        case "scan":
          setToolLog((log) =>
            [
              ...log,
              "[勘察] " +
                (ev.project_type || "未知项目") +
                (ev.test_command ? "，测试：" + ev.test_command : ""),
            ].slice(-200)
          );
          break;
        case "tool_call":
          setToolLog((l) =>
            [...l, "[工具] " + (ev.name || "") + " " + (ev.summary || "")].slice(-200)
          );
          break;
        case "tool_result":
          if (!ev.ok)
            pushMsg({
              kind: "status",
              text: "⚠️ 执行结果：" + (ev.summary || "").slice(0, 200),
            });
          setToolLog((l) =>
            [
              ...l,
              "[结果] " +
                (ev.name || "") +
                (ev.ok ? " ✓" : " ✗") +
                " " +
                (ev.summary || "").slice(0, 200),
            ].slice(-200)
          );
          break;
        case "test_report":
          setToolLog((log) =>
            [...log, ev.passed ? "[测试] 通过" : "[测试] 未通过"].slice(-200)
          );
          if (!ev.passed)
            pushMsg({ kind: "status", text: "⚠️ 测试未通过，正在修复…" });
          break;
        case "verify":
          setToolLog((log) =>
            [...log, ev.passed ? "[验证] 通过" : "[验证] 未通过"].slice(-200)
          );
          if (!ev.passed)
            pushMsg({ kind: "status", text: "⚠️ 验证未通过" });
          break;
        case "checkpoint":
          setToolLog((log) =>
            [...log, `[检查点 #${ev.sequence || "?"}] ${ev.reason || "progress"} · ${ev.steps || 0} 步`].slice(-200)
          );
          pushMsg({
            kind: "status",
            text: `⏳ 长任务检查点 #${ev.sequence || "?"}：已执行 ${ev.steps || 0} 步，正在继续…`,
          });
          break;
        case "experience_learned":
          setToolLog((log) =>
            [...log, `[经验] ${ev.summary || ev.id || "已记录"}`].slice(-200)
          );
          break;
        case "self_change_proposed":
          pushMsg({
            kind: "status",
            text: `🧭 已生成自身改进提案：${ev.summary || ev.id || "待审阅"}`,
          });
          break;
        case "self_change_applied":
          pushMsg({
            kind: "status",
            text: ev.success
              ? `✅ 自身改进提案已应用：${ev.summary || ev.id || "完成"}`
              : `⚠️ 自身改进提案未应用：${ev.summary || ev.id || "验证失败并已回滚"}`,
          });
          break;
        case "task_recovery_available":
          pushMsg({
            kind: "status",
            text: `⏯️ 检测到未完成任务（${ev.steps || 0} 步，${ev.status || "可恢复"}）。发送“继续上次任务”即可恢复。`,
          });
          setToolLog((log) =>
            [...log, `[恢复] ${ev.goal || ev.task_id || "发现可恢复任务"}`].slice(-200)
          );
          break;
        case "task_recovery_resumed":
          pushMsg({
            kind: "status",
            text: `▶️ 已恢复上次任务，从检查点继续（${ev.steps || 0} 步）。`,
          });
          break;
        case "task_recovery_discarded":
          pushMsg({ kind: "status", text: "🧹 已放弃上次未完成任务，开始处理当前请求。" });
          break;
        case "diagnostic_exported":
          pushMsg({
            kind: "status",
            text: `🧾 已导出脱敏诊断：${ev.path || "完成"}`,
          });
          break;
        case "approval_required":
          // 审批弹窗已经提供明确反馈，不在聊天区重复显示流程胶囊。
          break;
        case "approval_granted":
          // 人格化插话以 interjection 气泡到达，不显示固定状态行
          break;
        case "approval_denied":
          pushMsg({ kind: "status", text: "🚫 已拒绝执行" });
          break;
        case "interjection":
          pushMsg({ kind: "interjection", text: ev.text || "" });
          if (ev.text) ttsRef.current?.speakImmediate(ev.text);
          break;
        case "done":
          ttsRef.current?.finishResponse();
          setThinking(false);
          avatarRef.current?.think(false);
          setSilentTalkingActive(false);
          if (!ev.success)
            pushMsg({ kind: "status", text: "⚠️ 任务未能完成：" + (ev.summary || "") });
          refreshSoul();
          break;
        default:
          break;
      }
    };
    let disposed = false;
    const unlisten = [];
    Promise.all([
      listen("furina-event", (e) => handleEvent(e.payload)),
      listen("furina-approval", (e) =>
        setApproval({ id: e.payload.id, prompt: e.payload.prompt })
      ),
      listen("furina-soul", () => refreshSoul()),
    ]).then((cleanups) => {
      if (disposed) cleanups.forEach((cleanup) => cleanup());
      else unlisten.push(...cleanups);
    });
    refreshSoul();
    return () => {
      disposed = true;
      unlisten.splice(0).forEach((cleanup) => cleanup());
    };
  }, []);

  // ---------- 聊天 ----------
  async function chatSend(text) {
    const trimmed = text.trim();
    if (!trimmed) return;
    clearInputActivity();
    ttsRef.current?.stop();
    voiceEmotionRef.current.reset();
    setThinking(false);
    setSilentTalkingActive(false);
    avatarRef.current?.listen(false);
    avatarRef.current?.think(false);
    avatarRef.current?.talk(false);
    avatarRef.current?.acknowledge();
    pushMsg({ kind: "user", text: trimmed });
    try {
      await invoke("chat_send", { text: trimmed });
    } catch (e) {
      pushMsg({
        kind: "status",
        text: "⚠️ 发送失败：" + (e && e.message ? e.message : e),
      });
      setThinking(false);
      setSilentTalking(false);
      voiceEmotionRef.current.reset();
      avatarRef.current?.reset();
    }
  }

  async function respondApproval(ok) {
    if (!approval) return;
    const id = approval.id;
    setApproval(null);
    try {
      await invoke("approval_respond", { id, ok });
    } catch (e) {
      pushMsg({ kind: "status", text: "⚠️ 审批响应失败：" + e });
    }
  }

  function openSettings() {
    if (recording) { pushMsg({ kind: "status", text: "⚠️ 录音期间不能重新加载设置。" }); return; }
    if (thinking || approval) { pushMsg({ kind: "status", text: "⚠️ Furina 正在处理任务或等待审批，请稍后再打开设置。" }); return; }
    ttsRef.current?.stop();
    voiceEmotionRef.current.reset();
    setShowSetup(true);
  }

  async function startRecord() {
    clearInputActivity();
    ttsRef.current?.stop();
    voiceEmotionRef.current.reset();
    const ok = await recorderRef.current.start();
    if (ok) {
      setRecording(true);
      avatarRef.current?.listen(true);
    }
  }

  async function stopRecord() {
    const wav = recorderRef.current.stop();
    setRecording(false);
    avatarRef.current?.listen(false);
    if (!wav) return;
    try {
      const text = await invoke("transcribe", {
        audio: Array.from(wav),
        mime: "audio/wav",
      });
      if (text && text.trim()) await chatSend(text.trim());
      else pushMsg({ kind: "status", text: "没有听清，再试一次？" });
    } catch (e) {
      pushMsg({
        kind: "status",
        text: "⚠️ 语音识别失败：" + (e && e.message ? e.message : e),
      });
      setSilentTalkingActive(false);
      avatarRef.current?.reset();
    }
  }

  function clearInputActivity() {
    if (inputActivityTimerRef.current) {
      clearTimeout(inputActivityTimerRef.current);
      inputActivityTimerRef.current = null;
    }
  }

  function handleInputActivity(active) {
    clearInputActivity();
    if (!active) {
      avatarRef.current?.listen(false);
      return;
    }
    avatarRef.current?.listen(true);
    inputActivityTimerRef.current = setTimeout(() => {
      inputActivityTimerRef.current = null;
      avatarRef.current?.listen(false);
    }, 900);
  }

  async function loadMemories() {
    try {
      const list = await invoke("get_memories");
      setMemories(list || []);
    } catch (_) {
      setMemories([]);
    }
  }

  async function exportDiagnostics() {
    try {
      const path = await invoke("export_diagnostics");
      pushMsg({ kind: "status", text: `🧾 诊断已导出：${path}` });
    } catch (error) {
      pushMsg({ kind: "status", text: "⚠️ 诊断导出失败：" + error });
    }
  }

  // 情绪投影（spec §6，前端 Desktop Layer 计算）
  const expr = projectEmotions(soul?.emotions || {}, soul?.mood || "calm");
  const conversationState = conversationStateFor({
    recording,
    speaking: speaking || silentTalking,
    thinking,
  });

  useEffect(() => {
    const mood = soul?.mood;
    if (!mood) return;
    const previous = previousMoodRef.current;
    previousMoodRef.current = mood;
    if (!previous || previous === mood || recording || thinking || speaking || silentTalking) return;
    const reaction = moodReactionFor(mood);
    if (reaction) avatarRef.current?.react(reaction);
  }, [soul?.mood, recording, thinking, speaking, silentTalking]);

  useEffect(() => () => clearInputActivity(), []);

  // 自动滚动到底（贴底才跟随）
  const nearBottom = () => {
    const el = chatRef.current;
    return (
      !el ||
      el.scrollHeight - el.scrollTop - el.clientHeight < 120
    );
  };
  useEffect(() => {
    const el = chatRef.current;
    if (el && nearBottom()) el.scrollTop = el.scrollHeight;
  }, [msgsRef.current.length, tab]);

  return (
    <div className="desktop">
      <div id="bg-glow" />
      <header className="topbar">
        <div className="brand">
          <span className="brand-mark" />
          Furina
        </div>
        <div className="soul-mini">
          {soul
            ? `${MOOD_LABELS[soul.mood] || soul.mood} · ${
                soul.stage?.label || "—"
              } · 记忆 ${soul.memory_count ?? 0}`
            : "正在加载灵魂状态…"}
        </div>
        <div className="controls">
          <button className="settings-button" onClick={openSettings} disabled={recording || thinking || Boolean(approval)}>设置</button>
          <button className="settings-button" onClick={exportDiagnostics} disabled={recording || Boolean(approval)}>诊断</button>
          <label className="toggle">
            <input
              type="checkbox"
              checked={voiceOn}
              onChange={(e) => setVoiceOn(e.target.checked)}
            />
            语音
          </label>
          <label className="speed">
            语速
            <input
              type="range"
              min="0.8"
              max="1.2"
              step="0.05"
              value={speed}
              onChange={(e) => setSpeed(parseFloat(e.target.value))}
            />
          </label>
        </div>
      </header>

      <Suspense fallback={<AvatarLoading />}>
        <AvatarStage
          ref={avatarRef}
          mood={soul?.mood || "calm"}
          moodLabel={MOOD_LABELS[soul?.mood || "calm"]}
          intensity={expr.intensity}
          valence={expr.valence}
          arousal={expr.arousal}
          speaking={speaking || silentTalking}
          recording={recording}
          thinking={thinking}
          conversationState={conversationState}
          interactionBlocked={Boolean(approval)}
        />
      </Suspense>

      <div className="content">
        <section className="conversation">
          <nav className="tabs">
            {[
              ["chat", "聊天"],
              ["memory", "记忆"],
              ["tool", "工具输出"],
            ].map(([id, label]) => (
              <button
                key={id}
                className={tab === id ? "active" : ""}
                onClick={() => {
                  setTab(id);
                  if (id === "memory") loadMemories();
                }}
              >
                {label}
              </button>
            ))}
          </nav>

          {tab === "chat" && (
            <>
              <div className="chat" ref={chatRef}>
                {msgsRef.current.map((m, i) => (
                  <Message key={i} m={m} />
                ))}
                {msgsRef.current.length === 0 && (
                  <div className="empty-hint">
                    和 Furina 说点什么…（文本或按住 🎤 说话）
                  </div>
                )}
              </div>
              <Composer
                onSend={chatSend}
                onActivity={handleInputActivity}
                recording={recording}
                onStartRecord={startRecord}
                onStopRecord={stopRecord}
              />
            </>
          )}
          {tab === "memory" && <MemoryPanel memories={memories} soul={soul} />}
          {tab === "tool" && <ToolPanel log={toolLog} />}
        </section>

        <SoulPanel soul={soul} expr={expr} speaking={speaking} />
      </div>

      <RuntimeStatus soul={soul} thinking={thinking} toolCount={toolLog.length} />

      <ApprovalOverlay approval={approval} onRespond={respondApproval} />

      {thinking && <div className="thinking">💭 Furina 正在思考中…</div>}
      {recording && <div className="thinking rec">🎙️ 正在听…松开发送</div>}
      {showSetup && setupStatus && (
        <Suspense fallback={<SetupLoading />}>
          <SetupWizard
            initialStatus={setupStatus}
            onUpdated={setSetupStatus}
            onClose={() => setShowSetup(false)}
          />
        </Suspense>
      )}
    </div>
  );
}

// ---------- 子组件 ----------

function AvatarLoading() {
  return (
    <main className="avatar-stage avatar-stage-loading" aria-busy="true">
      <div className="avatar-placeholder">正在加载 Avatar…</div>
    </main>
  );
}

function SetupLoading() {
  return (
    <div className="setup-overlay" aria-busy="true">
      <div className="setup-shell">
        <div className="setup-card">
          <div className="setup-empty">正在加载设置…</div>
        </div>
      </div>
    </div>
  );
}

function Message({ m }) {
  if (m.kind === "status") return <div className="status">{m.text}</div>;
  const cls = m.kind === "interjection" ? "msg assistant interjection" : "msg " + m.kind;
  return (
    <div className={cls}>
      <span className="who">{m.kind === "user" ? "你" : "Furina"}</span>
      <span className="body">{m.text}</span>
    </div>
  );
}

function Composer({ onSend, onActivity, recording, onStartRecord, onStopRecord }) {
  const [input, setInput] = useState("");
  const send = () => {
    const t = input;
    setInput("");
    onActivity(false);
    onSend(t);
  };
  return (
    <footer className="composer">
      <button
        className={"ptt" + (recording ? " recording" : "")}
        title="按住说话（松开即识别并发送）"
        onMouseDown={(e) => {
          e.preventDefault();
          onStartRecord();
        }}
        onMouseUp={onStopRecord}
        onMouseLeave={() => recording && onStopRecord()}
        onTouchStart={(e) => {
          e.preventDefault();
          onStartRecord();
        }}
        onTouchEnd={(e) => {
          e.preventDefault();
          onStopRecord();
        }}
      >
        🎤 按住说话
      </button>
      <input
        type="text"
        value={input}
        placeholder="和 Furina 说点什么…（Enter 发送）"
        autoComplete="off"
         onChange={(e) => {
           const next = e.target.value;
           setInput(next);
           onActivity(Boolean(next.trim()));
         }}
         onBlur={() => onActivity(false)}
        onKeyDown={(e) => {
          if (e.key === "Enter") send();
        }}
      />
      <button className="send" onClick={send}>
        发送
      </button>
    </footer>
  );
}

function SoulPanel({ soul, expr, speaking }) {
  const s = soul || {};
  const emotions = s.emotions || {};
  return (
    <aside className="soul-panel">
      <h3>Soul Status</h3>
      <div className="soul-row">
        <span>心情</span>
        <b className={"mood-badge emotion-" + (s.mood || "calm")}>
          {MOOD_LABELS[s.mood] || s.mood || "—"}
        </b>
      </div>
      <div className="soul-row">
        <span>关系</span>
        <b>{s.stage?.label || "—"}</b>
        <progress max="100" value={s.stage?.trust || 0} />
      </div>
      <div className="soul-section">六维情绪</div>
      {EMOTION_DIMS.map(([key, label]) => (
        <div className="emotion-bar" key={key}>
          <span>{label}</span>
          <progress max="100" value={emotions[key] || 0} />
          <em>{Math.round(emotions[key] || 0)}</em>
        </div>
      ))}
      <div className="soul-section">Workspace</div>
      <div className="workspace-box">
        <div>Current Workspace: {s.workspace?.current_ws || "—"}</div>
        <div>Memory Scope: {s.workspace?.memory_scope || "—"}</div>
        <div>Active Memories: {s.workspace?.active_memory_count ?? s.memory_count ?? 0}</div>
      </div>
      <div className="soul-section">Agent</div>
      <div className={"agent-status " + (s.agent_status?.state || "idle")}>
        {s.agent_status?.action || s.agent_status?.state || "idle"}
        {speaking ? " · 🔊 朗读中" : ""}
      </div>
      <div className="soul-foot">投影 v1：V {expr.valence.toFixed(2)} A{" "}
        {expr.arousal.toFixed(2)} I {expr.intensity.toFixed(2)}</div>
    </aside>
  );
}

function RuntimeStatus({ soul, thinking, toolCount }) {
  const engines = [
    ["Soul", soul ? "运行中" : "待机"],
    ["Memory", `${soul?.memory_count ?? 0} 条`],
    ["Agent", thinking ? "工作中" : "空闲"],
    ["Tool Manager", `${toolCount} 条记录`],
  ];
  return (
    <footer className="runtime">
      <span>Rust Core Runtime</span>
      {engines.map(([name, state]) => (
        <span className="engine" key={name}>
          <b>{name}</b> {state}
        </span>
      ))}
    </footer>
  );
}

function MemoryPanel({ memories, soul }) {
  return (
    <div className="panel-body">
      <div className="panel-note">
        Active Memories: {soul?.workspace?.active_memory_count ?? soul?.memory_count ?? 0}
      </div>
      {memories.length === 0 ? (
        <div className="empty-hint">暂无记忆摘要（切换本页会从灵魂引擎拉取）。</div>
      ) : (
        memories.map((m, i) => (
          <div className="memory-item" key={i}>
            <span className="memory-type">{m.type || "?"}</span>
            <span>{m.content}</span>
            <em>★{m.importance ?? 0}{m.valence ? ` · ${m.valence > 0 ? "+" : "−"}` : ""}</em>
          </div>
        ))
      )}
    </div>
  );
}

function ToolPanel({ log }) {
  return (
    <div className="panel-body tool-log">
      {log.length === 0 ? (
        <div className="empty-hint">暂无工具执行记录。</div>
      ) : (
        log.map((line, i) => <div key={i}>{line}</div>)
      )}
    </div>
  );
}

function ApprovalOverlay({ approval, onRespond }) {
  if (!approval) return null;
  return (
    <div className="approval-overlay">
      <div className="approval-box">
        <h3>Furina 需要你的许可</h3>
        <pre>{approval.prompt}</pre>
        <div className="approval-actions">
          <button className="primary" onClick={() => onRespond(true)}>
            同意
          </button>
          <button onClick={() => onRespond(false)}>拒绝</button>
        </div>
      </div>
    </div>
  );
}
