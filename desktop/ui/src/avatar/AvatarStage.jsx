import React, { useEffect, useRef, useState } from "react";
import { invoke } from "../lib/tauri.js";
import { VrmAvatarProvider } from "./VrmAvatarProvider.js";

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

export default function AvatarStage({
  mood,
  moodLabel,
  intensity,
  valence,
  arousal,
  speaking,
}) {
  const viewportRef = useRef(null);
  const providerRef = useRef(null);
  const [status, setStatus] = useState("loading");
  const [detail, setDetail] = useState("正在载入 Furina VRM");

  useEffect(() => {
    let cancelled = false;
    let provider = null;
    try {
      provider = new VrmAvatarProvider(viewportRef.current);
      providerRef.current = provider;
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
      provider?.dispose();
      providerRef.current = null;
    };
  }, []);

  useEffect(() => {
    providerRef.current?.setState({ mood, intensity, speaking });
  }, [mood, intensity, speaking]);

  const showPlaceholder = status !== "ready";
  return (
    <main className={`avatar-stage avatar-stage-${status}`}>
      <div className="avatar-viewport" ref={viewportRef} aria-label="Furina VRM Avatar" />
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
}