import React, { useEffect, useReducer, useRef, useState } from "react";
import { invoke, listen } from "./lib/tauri.js";
import { StreamBuffer } from "./lib/streaming.js";
import { projectEmotions } from "./lib/projection.js";
import { PcmRecorder } from "./lib/asr.js";
import { TtsPipeline } from "./lib/tts.js";
import AvatarStage from "./avatar/AvatarStage.jsx";

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

function moodToEmotionTag(mood) {
  switch (mood) {
    case "happy":
    case "proud":
      return "[happy]";
    case "hurt":
    case "sad":
      return "[sad]";
    case "annoyed":
      return "[angry]";
    default:
      return "";
  }
}

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
  const [toolLog, setToolLog] = useState([]);
  const [memories, setMemories] = useState([]);

  const speedRef = useRef(1.0);
  const soulRef = useRef(null);
  useEffect(() => {
    speedRef.current = speed;
  }, [speed]);
  useEffect(() => {
    soulRef.current = soul;
  }, [soul]);

  const streamRef = useRef(new StreamBuffer());
  const currentAssistantRef = useRef(null);
  const chatRef = useRef(null);

  // ---------- TTS 管线 ----------
  const ttsRef = useRef(null);
  useEffect(() => {
    ttsRef.current = new TtsPipeline({
      invoke,
      emotionFor: () => moodToEmotionTag(soulRef.current?.mood || "calm"),
      speedFor: () => speedRef.current,
      onStatus: (s) => pushMsg({ kind: "status", text: s }),
      onSpeaking: setSpeaking,
    });
    return () => ttsRef.current?.stop();
  }, []);
  useEffect(() => {
    if (ttsRef.current) ttsRef.current.enabled = voiceOn;
  }, [voiceOn]);

  // ---------- 录音 ----------
  const recorderRef = useRef(
    new PcmRecorder({ onStatus: (s) => pushMsg({ kind: "status", text: s }) })
  );

  // ---------- 流式渲染 ----------
  function appendAssistant(s) {
    const arr = msgsRef.current;
    const last = arr[arr.length - 1];
    if (!currentAssistantRef.current || !last || last.kind !== "assistant") {
      const m = { kind: "assistant", text: "" };
      arr.push(m);
      currentAssistantRef.current = m;
    }
    currentAssistantRef.current.text +=
      (currentAssistantRef.current.text ? "\n" : "") + s;
    tick();
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
          break;
        case "message_delta":
          for (const s of streamRef.current.feed(ev.content || ""))
            appendAssistant(s);
          break;
        case "message":
          if (ev.role === "assistant") {
            for (const s of streamRef.current.flush()) appendAssistant(s);
          }
          break;
        case "scan":
          pushMsg({
            kind: "status",
            text:
              "📋 项目勘察：" +
              (ev.project_type || "") +
              "，测试：" +
              (ev.test_command || ""),
          });
          break;
        case "tool_call":
          pushMsg({ kind: "status", text: "⚙️ " + (ev.name || "") });
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
          pushMsg({
            kind: "status",
            text: ev.passed ? "🧪 测试通过" : "🧪 测试未通过，正在修复…",
          });
          break;
        case "verify":
          pushMsg({ kind: "status", text: ev.passed ? "✅ 验证通过" : "❌ 验证未通过" });
          break;
        case "approval_required":
          pushMsg({ kind: "status", text: "⏳ 等待你的审批…" });
          break;
        case "approval_granted":
          // 人格化插话以 interjection 气泡到达，不显示固定状态行
          break;
        case "approval_denied":
          pushMsg({ kind: "status", text: "🚫 已拒绝执行" });
          break;
        case "interjection":
          pushMsg({ kind: "interjection", text: ev.text || "" });
          if (ev.text) ttsRef.current?.speak(ev.text);
          break;
        case "done":
          setThinking(false);
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
    if (!text.trim()) return;
    ttsRef.current?.stop();
    pushMsg({ kind: "user", text: text.trim() });
    try {
      await invoke("chat_send", { text: text.trim() });
    } catch (e) {
      pushMsg({
        kind: "status",
        text: "⚠️ 发送失败：" + (e && e.message ? e.message : e),
      });
      setThinking(false);
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

  async function startRecord() {
    ttsRef.current?.stop();
    const ok = await recorderRef.current.start();
    if (ok) setRecording(true);
  }

  async function stopRecord() {
    const wav = recorderRef.current.stop();
    setRecording(false);
    if (!wav) return;
    pushMsg({ kind: "status", text: "🎙️ 正在识别…" });
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
    }
  }

  async function loadMemories() {
    try {
      const list = await invoke("get_memories");
      setMemories(list || []);
    } catch (_) {
      setMemories([]);
    }
  }

  // 情绪投影（spec §6，前端 Desktop Layer 计算）
  const expr = projectEmotions(soul?.emotions || {}, soul?.mood || "calm");

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
              min="0.5"
              max="2"
              step="0.05"
              value={speed}
              onChange={(e) => setSpeed(parseFloat(e.target.value))}
            />
          </label>
        </div>
      </header>

      <AvatarStage
        mood={soul?.mood || "calm"}
        moodLabel={MOOD_LABELS[soul?.mood || "calm"]}
        intensity={expr.intensity}
        valence={expr.valence}
        arousal={expr.arousal}
        speaking={speaking}
      />

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
    </div>
  );
}

// ---------- 子组件 ----------

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

function Composer({ onSend, recording, onStartRecord, onStopRecord }) {
  const [input, setInput] = useState("");
  const send = () => {
    const t = input;
    setInput("");
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
        onChange={(e) => setInput(e.target.value)}
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
            <em>★{m.importance ?? 0}</em>
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
