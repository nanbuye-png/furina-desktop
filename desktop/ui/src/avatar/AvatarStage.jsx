import React, { forwardRef, useEffect, useImperativeHandle, useRef, useState } from "react";
import { invoke, listen } from "../lib/tauri.js";
import { AvatarBehaviorController } from "./avatarBehavior.js";
import { VrmAvatarProvider } from "./VrmAvatarProvider.js";
import { clickZoneForPointer, normalizePointer } from "./avatarInteraction.js";

function normalizeArrayBuffer(payload) {
  if (payload instanceof ArrayBuffer) return payload;
  if (ArrayBuffer.isView(payload)) {
    return payload.buffer.slice(payload.byteOffset, payload.byteOffset + payload.byteLength);
  }
  if (Array.isArray(payload)) return Uint8Array.from(payload).buffer;
  throw new Error("Avatar IPC 返回了未知的二进制格式");
}

function PlaceholderAvatar({ mood, intensity, speaking }) {
  const glowClass = `emotion-${mood || "calm"}`;
  const scale = 1 + (intensity || 0) * 0.06;
  return (
    <div className="silhouette-wrap avatar-placeholder">
      <div className={`silhouette ${glowClass}`} style={{ transform: `scale(${scale})` }}>
        <div className="silhouette-head" />
        <div className="silhouette-body" />
        {speaking && <div className="mouth-dot" />}
      </div>
    </div>
  );
}

const AvatarStage = forwardRef(function AvatarStage({
  mood,
  moodLabel,
  intensity,
  valence,
  arousal,
  speaking,
  recording,
  thinking,
  conversationState,
  interactionBlocked,
}, ref) {
  const viewportRef = useRef(null);
  const providerRef = useRef(null);
  const behaviorRef = useRef(null);
  const queuedBehaviorsRef = useRef([]);
  const [status, setStatus] = useState("loading");
  const [detail, setDetail] = useState("正在载入 Furina VRM");
  const [revision, setRevision] = useState(0);

  useImperativeHandle(ref, () => {
    const call = (name, ...args) => {
      const behavior = behaviorRef.current;
      if (behavior) return behavior[name](...args);
      if (["acknowledge", "greet", "farewell", "react", "motion"].includes(name)) {
        queuedBehaviorsRef.current.push({ name, args });
        return { accepted: true, queued: true, behavior: name };
      }
      return false;
    };
    return {
      acknowledge: () => call("acknowledge"),
      listen: (active = true) => call("listen", active),
      think: (active = true) => call("think", active),
      talk: (active = true) => call("talk", active),
      greet: () => call("greet"),
      farewell: () => call("farewell"),
      react: (kind) => call("react", kind),
      motion: (name) => call("motion", name),
      setAudioLevel: (level) => providerRef.current?.setAudioLevel(level),
      reset: () => {
        queuedBehaviorsRef.current = [];
        return behaviorRef.current?.reset() ?? false;
      },
    };
  }, []);

  useEffect(() => {
    let unlisten;
    listen("furina-avatar-changed", () => setRevision((value) => value + 1)).then((dispose) => { unlisten = dispose; });
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    let cancelled = false;
    let provider = null;
    try {
      provider = new VrmAvatarProvider(viewportRef.current);
      providerRef.current = provider;
      behaviorRef.current = new AvatarBehaviorController({
        requestAction: (intent) => provider.handleIntent(intent),
        onModeChange: (mode) => provider.setBehaviorMode(mode),
      });
    } catch (error) {
      console.error("Avatar Renderer 初始化失败", error);
      setStatus("error");
      setDetail(error instanceof Error ? error.message : String(error));
    }

    async function loadAvatar() {
      try {
        const info = await invoke("get_avatar_asset_info");
        if (!info?.available) {
          setStatus("missing");
          setDetail("未安装本地 VRM，已使用轻量占位形象");
          return;
        }
        if (!provider) return;
        const payload = await invoke("load_avatar_asset");
        await provider.load(normalizeArrayBuffer(payload));
        if (!cancelled) {
          setStatus("ready");
          setDetail(`${info.fileName} · ${(info.sizeBytes / 1024 / 1024).toFixed(1)} MiB`);
          const queued = queuedBehaviorsRef.current.splice(0);
          if (queued.length > 0) {
            queued.forEach(({ name, args }) => behaviorRef.current?.[name](...args));
          } else {
            behaviorRef.current?.greet();
          }
        }
      } catch (error) {
        if (!cancelled) {
          console.error("Avatar 加载失败", error);
          setStatus("error");
          setDetail(error instanceof Error ? error.message : String(error));
        }
      }
    }

    loadAvatar();
    return () => {
      cancelled = true;
      behaviorRef.current?.reset();
      behaviorRef.current = null;
      provider?.dispose();
      providerRef.current = null;
    };
  }, [revision]);

  useEffect(() => {
    providerRef.current?.setInteractionContext({
      mood,
      intensity,
      valence,
      arousal,
      speaking,
      recording,
      thinking,
      conversationState,
      blocked: interactionBlocked,
    });
    behaviorRef.current?.flushPending();
  }, [
    mood,
    intensity,
    valence,
    arousal,
    speaking,
    recording,
    thinking,
    conversationState,
    interactionBlocked,
  ]);

  useEffect(() => {
    const provider = providerRef.current;
    if (!provider) return undefined;
    let focused = document.visibilityState === "visible" && document.hasFocus();
    const handleFocus = () => {
      const returned = !focused;
      focused = true;
      provider.setFocused(true);
      behaviorRef.current?.flushPending();
      if (returned && status === "ready") behaviorRef.current?.greet();
    };
    const handleBlur = () => {
      focused = false;
      provider.setFocused(false);
    };
    const handleVisibility = () => {
      if (document.visibilityState === "visible") handleFocus();
      else handleBlur();
    };
    window.addEventListener("focus", handleFocus);
    window.addEventListener("blur", handleBlur);
    document.addEventListener("visibilitychange", handleVisibility);
    return () => {
      window.removeEventListener("focus", handleFocus);
      window.removeEventListener("blur", handleBlur);
      document.removeEventListener("visibilitychange", handleVisibility);
    };
  }, [revision, status]);

  function updatePointer(event, active = true) {
    const provider = providerRef.current;
    if (!provider) return;
    const point = normalizePointer(
      event.currentTarget.getBoundingClientRect(),
      event.clientX,
      event.clientY,
    );
    provider.setPointer(point.x, point.y, active);
  }

  function handlePointerDown(event) {
    updatePointer(event);
    const point = normalizePointer(
      event.currentTarget.getBoundingClientRect(),
      event.clientX,
      event.clientY,
    );
    const provider = providerRef.current;
    if (!provider?.triggerClickAt(point.x, point.y)) {
      provider?.triggerClick(clickZoneForPointer(point));
    }
  }

  const showPlaceholder = status !== "ready";
  return (
    <main className={`avatar-stage avatar-stage-${status}`}>
      <div
        className="avatar-viewport"
        ref={viewportRef}
        aria-label="Furina VRM Avatar"
        role="img"
        onPointerEnter={(event) => updatePointer(event)}
        onPointerMove={(event) => updatePointer(event)}
        onPointerLeave={() => providerRef.current?.clearPointer()}
        onPointerDown={status === "ready" ? handlePointerDown : undefined}
      />
      {showPlaceholder && (
        <PlaceholderAvatar mood={mood} intensity={intensity} speaking={speaking} />
      )}
      <div className={`avatar-runtime-status ${status}`} title={detail}>
        <span className="avatar-status-dot" />
        {status === "ready" ? "VRM LIVE" : status === "loading" ? "VRM LOADING" : "FALLBACK"}
      </div>
      <div className="stage-caption">
        {moodLabel || mood} · 效价 {valence.toFixed(2)} · 唤醒 {arousal.toFixed(2)}
      </div>
    </main>
  );
});

export default AvatarStage;
