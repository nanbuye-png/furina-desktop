// Tauri 桥：invoke 命令 + 事件监听（withGlobalTauri）。
export const invoke = (...args) => window.__TAURI__.core.invoke(...args);
export const listen = (...args) => window.__TAURI__.event.listen(...args);

/** 全局唯一事件 id（Envelope 用，前端生成）。 */
export function newEventId() {
  return crypto.randomUUID
    ? crypto.randomUUID()
    : "evt-" + Date.now() + "-" + Math.random().toString(36).slice(2, 10);
}

/** 统一 Event Envelope（spec §5）：{ event_id, event_type, timestamp, payload } */
export function envelope(eventType, payload) {
  return {
    event_id: newEventId(),
    event_type: eventType,
    timestamp: Date.now(),
    payload,
  };
}
