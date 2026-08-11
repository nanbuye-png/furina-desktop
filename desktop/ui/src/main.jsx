import React from "react";
import { createRoot } from "react-dom/client";
import App from "./App.jsx";
import "./styles.css";

const rootEl = document.getElementById("root");

if (!window.__TAURI__ || !window.__TAURI__.core || !window.__TAURI__.event) {
  rootEl.innerHTML =
    '<div style="position:fixed;inset:0;display:flex;align-items:center;justify-content:center;background:#040a18;color:#eaf4ff;font:14px/1.6 system-ui;">' +
    "Tauri IPC 未注入（window.__TAURI__ 不可用）——请用 cargo run -p furina-desktop 启动，或检查 tauri.conf.json 的 withGlobalTauri。" +
    "</div>";
} else {
  createRoot(rootEl).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>
  );
}
