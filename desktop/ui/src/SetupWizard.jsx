import React, { useEffect, useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "./lib/tauri.js";
import { applyAsrPreset, applyVoicePreset, createInitialSetupForm, normalizeMigrationCandidates, projectDiagnosticRows, validateSetupForm } from "./setupModel.js";

const STEPS = ["迁移", "模型", "语音", "形象", "诊断"];

export default function SetupWizard({ initialStatus, onClose, onUpdated }) {
  const [step, setStep] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [candidates, setCandidates] = useState([]);
  const [diagnostics, setDiagnostics] = useState(null);
  const [avatarAvailable, setAvatarAvailable] = useState(Boolean(initialStatus?.avatar?.available));
  const [form, setForm] = useState(() => createInitialSetupForm(initialStatus));

  useEffect(() => {
    let cancelled = false;
    if (step !== 0) return undefined;
    setBusy(true);
    invoke("detect_legacy_data")
      .then((items) => { if (!cancelled) setCandidates(normalizeMigrationCandidates(items)); })
      .catch((reason) => { if (!cancelled) setError(messageOf(reason)); })
      .finally(() => { if (!cancelled) setBusy(false); });
    return () => { cancelled = true; };
  }, [step]);

  const canClose = !initialStatus?.needsSetup;
  const progress = useMemo(() => ((step + 1) / STEPS.length) * 100, [step]);

  function update(key, value) { setForm((current) => ({ ...current, [key]: value })); }
  function clearFeedback() { setError(""); setNotice(""); }

  async function chooseWorkspace() {
    const selected = await open({ directory: true, multiple: false, title: "选择 Furina 的工作区" });
    if (selected) update("workspace", selected);
  }

  async function importAvatar() {
    clearFeedback();
    const selected = await open({
      multiple: false,
      title: "导入 VRM Avatar",
      filters: [{ name: "VRM Avatar", extensions: ["vrm"] }],
    });
    if (!selected) return;
    setBusy(true);
    try {
      const result = await invoke("import_avatar", { sourcePath: selected });
      setAvatarAvailable(true);
      setNotice("已导入 " + result.fileName + " · " + (result.sizeBytes / 1024 / 1024).toFixed(1) + " MiB");
    } catch (reason) { setError(messageOf(reason)); }
    finally { setBusy(false); }
  }

  async function migrate(root) {
    clearFeedback();
    setBusy(true);
    try {
      await invoke("migrate_legacy_data", { sourceRoot: root });
      const status = await invoke("get_setup_status");
      onUpdated?.(status);
      setNotice("旧 Desktop 配置、记忆、密钥和 VRM 已复制，源目录保持不变。");
      setCandidates([]);
    } catch (reason) { setError(messageOf(reason)); }
    finally { setBusy(false); }
  }

  async function saveAndDiagnose() {
    clearFeedback();
    const validation = validateSetupForm(form, initialStatus);
    if (validation) { setError(validation.message); setStep(validation.step); return; }
    setBusy(true);
    try {
      const status = await invoke("save_setup", { setup: form });
      onUpdated?.(status);
      let llmResult = "配置已保存";
      let llmOk = true;
      try { llmResult = await invoke("validate_provider", { kind: "llm" }); }
      catch (reason) { llmOk = false; llmResult = "连接测试失败：" + messageOf(reason); }
      let emotionClassifierResult = form.emotionClassifierEnabled ? "未测试" : "未启用";
      let emotionClassifierOk = !form.emotionClassifierEnabled;
      if (form.emotionClassifierEnabled) {
        try {
          emotionClassifierResult = await invoke("validate_provider", { kind: "emotion_classifier" });
          emotionClassifierOk = true;
        } catch (reason) {
          emotionClassifierResult = "连接测试失败：" + messageOf(reason);
          emotionClassifierOk = false;
        }
      }
      let doctor = {};
      try { doctor = await invoke("doctor"); }
      catch (reason) { doctor = { service_errors: [messageOf(reason)] }; }
      setDiagnostics({ ...doctor, llmResult, llmOk, emotionClassifierResult, emotionClassifierOk });
      setNotice("设置已保存，运行时已重新加载。");
      setStep(4);
    } catch (reason) { setError(messageOf(reason)); }
    finally { setBusy(false); }
  }

  return (
    <div className="setup-overlay" role="dialog" aria-modal="true" aria-label="Furina 首次设置">
      <div className="setup-shell">
        <aside className="setup-rail">
          <div className="setup-crest"><span />Furina Desktop</div>
          <p>让声音、记忆与形象在这台电脑上安定下来。</p>
          <ol>{STEPS.map((label, index) => (
            <li key={label} className={index === step ? "active" : index < step ? "done" : ""}>
              <span>{index < step ? "✓" : index + 1}</span>{label}
            </li>
          ))}</ol>
          <div className="setup-progress"><i style={{ width: progress + "%" }} /></div>
        </aside>

        <section className="setup-card">
          {canClose && <button className="setup-close" onClick={onClose} aria-label="关闭设置">×</button>}
          <div className="setup-scroll" tabIndex={0}>
            {step === 0 && <MigrationStep candidates={candidates} busy={busy} onMigrate={migrate} />}
            {step === 1 && <ModelStep form={form} update={update} chooseWorkspace={chooseWorkspace} keyConfigured={initialStatus?.llm?.apiKeyConfigured} />}
            {step === 2 && <VoiceStep form={form} setForm={setForm} update={update} />}
            {step === 3 && <AvatarStep available={avatarAvailable} busy={busy} onImport={importAvatar} notice={notice} />}
            {step === 4 && <DiagnosticsStep diagnostics={diagnostics} status={initialStatus} />}
            {(error || (notice && step !== 3)) && <div className={error ? "setup-message error" : "setup-message"}>{error || notice}</div>}
          </div>
          <footer className="setup-actions">
            <button className="quiet" disabled={busy || step === 0} onClick={() => { clearFeedback(); setStep((value) => Math.max(0, value - 1)); }}>上一步</button>
            {step < 3 && <button disabled={busy} onClick={() => { clearFeedback(); setStep((value) => value + 1); }}>下一步</button>}
            {step === 3 && <button disabled={busy} onClick={saveAndDiagnose}>保存并检查</button>}
            {step === 4 && <button disabled={busy} onClick={onClose}>进入 Furina</button>}
          </footer>
        </section>
      </div>
    </div>
  );
}

function MigrationStep({ candidates, busy, onMigrate }) {
  return <div className="setup-step">
    <span className="setup-eyebrow">01 · Continuity</span><h1>把旧时光带过来</h1>
    <p>只检测 Desktop 项目，不读取历史 CLI。迁移采用复制，原目录不会被改动。</p>
    {busy && <div className="setup-empty">正在检查可信项目目录…</div>}
    {!busy && candidates.length === 0 && <div className="setup-empty">没有发现可迁移的旧 Desktop 数据，可以直接开始全新设置。</div>}
    <div className="migration-list">{candidates.map((candidate) => <article key={candidate.root}>
      <b>{candidate.root}</b><small>{[candidate.hasMemory && "记忆", candidate.hasSecrets && "密钥", candidate.hasAvatar && "VRM"].filter(Boolean).join(" · ") || "配置"}</small>
      <button onClick={() => onMigrate(candidate.root)}>确认并复制</button>
    </article>)}</div>
  </div>;
}

function ModelStep({ form, update, chooseWorkspace, keyConfigured }) {
  return <div className="setup-step"><span className="setup-eyebrow">02 · Intelligence</span><h1>连接她的思考</h1>
    <p>支持 OpenAI 兼容接口。密钥仅写入本机私有文件，界面不会再次显示完整内容。</p>
    <Field label="接口地址"><input value={form.llmBaseUrl} onChange={(event) => update("llmBaseUrl", event.target.value)} /></Field>
    <Field label="模型"><input value={form.llmModel} onChange={(event) => update("llmModel", event.target.value)} /></Field>
    <Field label={"API Key" + (keyConfigured ? "（已配置，留空保持不变）" : "")}><input type="password" value={form.llmApiKey} onChange={(event) => update("llmApiKey", event.target.value)} /></Field>
    <div className="voice-config-block">
      <ServiceToggle title="情绪分类器（高级，可选）" enabled={form.emotionClassifierEnabled} onToggle={(value) => update("emotionClassifierEnabled", value)}>
        <small className="setup-help">只分析当前输入与最近两轮对话；1500ms 内失败会自动回退关键词规则，不阻断聊天。</small>
      </ServiceToggle>
      {form.emotionClassifierEnabled && <>
        <Field label="提供方 ID"><input value={form.emotionClassifierProviderId} onChange={(event) => update("emotionClassifierProviderId", event.target.value)} /></Field>
        <Field label="轻量模型"><input value={form.emotionClassifierModel} onChange={(event) => update("emotionClassifierModel", event.target.value)} /></Field>
        <Field label="超时（500–1500ms）"><input type="number" min="500" max="1500" step="100" value={form.emotionClassifierTimeoutMs} onChange={(event) => update("emotionClassifierTimeoutMs", Number(event.target.value))} /></Field>
        <Field label="Token 上限（32–512）"><input type="number" min="32" max="512" step="32" value={form.emotionClassifierMaxTokens} onChange={(event) => update("emotionClassifierMaxTokens", Number(event.target.value))} /></Field>
      </>}
    </div>
    <Field label="Agent 工作区"><div className="path-control"><input value={form.workspace} onChange={(event) => update("workspace", event.target.value)} /><button className="quiet" onClick={chooseWorkspace}>选择</button></div></Field>
  </div>;
}

function VoiceStep({ form, setForm, update }) {
  return <div className="setup-step"><span className="setup-eyebrow">03 · Voice</span><h1>选择她如何听与说</h1>
    <p>语音是可选能力。Fish、Qwen 与 OpenAI 兼容服务只是预设，服务名称、接口、模型和密钥均可独立修改。</p>
    <div className="service-grid">
      <ServiceToggle title="TTS 朗读" enabled={form.voiceEnabled} onToggle={(value) => update("voiceEnabled", value)}>
        <PresetButtons onSelect={(preset) => setForm((current) => applyVoicePreset(current, preset))} />
      </ServiceToggle>
      <ServiceToggle title="ASR 识别" enabled={form.asrEnabled} onToggle={(value) => update("asrEnabled", value)}>
        <PresetButtons onSelect={(preset) => setForm((current) => applyAsrPreset(current, preset))} />
      </ServiceToggle>
    </div>
    {form.voiceEnabled && <div className="voice-config-block">
      <h3>TTS 配置</h3>
      <Field label="服务名称"><input value={form.voiceProvider} onChange={(event) => update("voiceProvider", event.target.value)} /></Field>
      <Field label="请求协议"><select value={form.voiceProtocol} onChange={(event) => update("voiceProtocol", event.target.value)}><option value="fish">Fish TTS</option><option value="qwen_omni">Qwen Omni Chat Audio</option><option value="openai">OpenAI Audio Speech</option></select></Field>
      <Field label="完整接口地址"><input value={form.voiceEndpoint} onChange={(event) => update("voiceEndpoint", event.target.value)} /></Field>
      <Field label="模型"><input value={form.voiceModel} onChange={(event) => update("voiceModel", event.target.value)} /></Field>
      {form.voiceProtocol === "fish" && <Field label="Reference ID（可选）"><input value={form.voiceReferenceId} onChange={(event) => update("voiceReferenceId", event.target.value)} /></Field>}
      {form.voiceProtocol !== "fish" && <Field label="音色"><input value={form.voiceVoice} onChange={(event) => update("voiceVoice", event.target.value)} /></Field>}
      <Field label="输出格式"><select value={form.voiceFormat} onChange={(event) => update("voiceFormat", event.target.value)}><option value="mp3">MP3</option><option value="wav">WAV</option><option value="flac">FLAC</option></select></Field>
      <Field label="API Key 环境变量"><input value={form.voiceApiKeyEnv} onChange={(event) => update("voiceApiKeyEnv", event.target.value)} /></Field>
      <Field label="TTS API Key（留空保持不变）"><input type="password" value={form.voiceApiKey} onChange={(event) => update("voiceApiKey", event.target.value)} /></Field>
    </div>}
    {form.asrEnabled && <div className="voice-config-block">
      <h3>ASR 配置</h3>
      <Field label="服务名称"><input value={form.asrProvider} onChange={(event) => update("asrProvider", event.target.value)} /></Field>
      <Field label="请求协议"><select value={form.asrProtocol} onChange={(event) => update("asrProtocol", event.target.value)}><option value="fish">Fish ASR</option><option value="qwen_omni">Qwen Omni Chat Audio</option><option value="openai">OpenAI Audio Transcriptions</option></select></Field>
      <Field label="完整接口地址"><input value={form.asrEndpoint} onChange={(event) => update("asrEndpoint", event.target.value)} /></Field>
      {form.asrProtocol !== "fish" && <Field label="模型"><input value={form.asrModel} onChange={(event) => update("asrModel", event.target.value)} /></Field>}
      <Field label="语言"><input value={form.asrLanguage} onChange={(event) => update("asrLanguage", event.target.value)} /></Field>
      {form.asrProtocol !== "fish" && <Field label="转写提示词（可选）"><input value={form.asrPrompt} onChange={(event) => update("asrPrompt", event.target.value)} /></Field>}
      <Field label="API Key 环境变量"><input value={form.asrApiKeyEnv} onChange={(event) => update("asrApiKeyEnv", event.target.value)} /></Field>
      <Field label="ASR API Key（留空保持不变）"><input type="password" value={form.asrApiKey} onChange={(event) => update("asrApiKey", event.target.value)} /></Field>
    </div>}
  </div>;
}

function PresetButtons({ onSelect }) {
  return <div className="preset-buttons"><button className="quiet" onClick={() => onSelect("fish")}>Fish</button><button className="quiet" onClick={() => onSelect("qwen")}>Qwen</button><button className="quiet" onClick={() => onSelect("openai")}>OpenAI 兼容</button></div>;
}

function AvatarStep({ available, busy, onImport, notice }) {
  return <div className="setup-step avatar-import-step"><span className="setup-eyebrow">04 · Presence</span><h1>让她在桌面上出现</h1>
    <p>项目不会分发角色模型。请选择你有权使用的 VRM 文件；导入失败时仍会保留旧模型或占位形象，也可以暂不导入并直接继续。</p>
    <div className={"avatar-drop " + (available ? "ready" : "")}><span className="avatar-orbit" /><b>{available ? "本地 VRM 已就绪" : "尚未导入 VRM"}</b><small>最大 256 MiB · glTF 2.0 / VRM</small><button disabled={busy} onClick={onImport}>{busy ? "正在导入…" : available ? "更换 VRM" : "选择 VRM 文件"}</button></div>
    {notice && <div className="setup-message">{notice}</div>}
  </div>;
}

function DiagnosticsStep({ diagnostics, status }) {
  const rows = projectDiagnosticRows(diagnostics, status);
  return <div className="setup-step"><span className="setup-eyebrow">05 · Ready</span><h1>水面已经平静</h1><p>配置已写入 Desktop 独立数据目录，运行时已原子重载。</p>
    <div className="diagnostic-list">{rows.map(([label, detail, ok]) => <div key={label}><span className={ok ? "ok" : "warn"}>{ok ? "✓" : "!"}</span><b>{label}</b><small>{detail}</small></div>)}</div>
  </div>;
}

function Field({ label, children }) { return <label className="setup-field"><span>{label}</span>{children}</label>; }
function ServiceToggle({ title, enabled, onToggle, children }) { return <div className={"service-card " + (enabled ? "enabled" : "")}><label><input type="checkbox" checked={enabled} onChange={(event) => onToggle(event.target.checked)} /><b>{title}</b></label>{enabled && children}</div>; }
function messageOf(reason) { return reason instanceof Error ? reason.message : String(reason); }
