export function createInitialSetupForm(status = {}) {
  const runtime = status.runtime || {};
  return {
    llmBaseUrl: status.llm?.baseUrl || "https://api.deepseek.com",
    llmModel: status.llm?.model || "deepseek-chat",
    llmApiKey: "",
    emotionClassifierEnabled: status.emotionClassifier?.enabled ?? false,
    emotionClassifierProviderId: status.emotionClassifier?.providerId || "desktop",
    emotionClassifierModel: status.emotionClassifier?.model || status.llm?.model || "deepseek-chat",
    emotionClassifierTimeoutMs: status.emotionClassifier?.timeoutMs || 1500,
    emotionClassifierMaxTokens: status.emotionClassifier?.maxTokens || 256,
    workspace: runtime.workspace_root || runtime.workspaceRoot || "",
    voiceEnabled: status.voice?.enabled ?? false,
    voiceProvider: status.voice?.provider || "Fish Audio",
    voiceProtocol: status.voice?.protocol || "fish",
    voiceEndpoint: status.voice?.endpoint || "https://api.fish.audio/v1/tts",
    voiceModel: status.voice?.model || "s2.1-pro-free",
    voiceVoice: status.voice?.voice || "",
    voiceFormat: status.voice?.format || "mp3",
    voiceApiKeyEnv: status.voice?.apiKeyEnv || "FURINA_TTS_API_KEY",
    voiceApiKey: "",
    voiceReferenceId: status.voice?.referenceId || "",
    asrEnabled: status.asr?.enabled ?? false,
    asrProvider: status.asr?.provider || "Qwen Omni",
    asrProtocol: status.asr?.protocol || "qwen_omni",
    asrEndpoint: status.asr?.endpoint || "https://dashscope.aliyuncs.com/compatible-mode/v1",
    asrModel: status.asr?.model || "qwen3.5-omni-flash",
    asrPrompt: status.asr?.prompt || "请只输出转写结果。",
    asrLanguage: status.asr?.language || "zh",
    asrApiKeyEnv: status.asr?.apiKeyEnv || "FURINA_ASR_API_KEY",
    asrApiKey: "",
    qwenApiKey: "",
  };
}

export function validateSetupForm(form, status = {}) {
  if (!form.llmBaseUrl.trim() || !form.llmModel.trim()) return { step: 1, message: "填写模型地址和模型名称。" };
  if (!form.workspace.trim()) return { step: 1, message: "选择 Agent 工作区。" };
  if (!status.llm?.apiKeyConfigured && !form.llmApiKey.trim()) return { step: 1, message: "首次设置需要填写 LLM API Key。" };
  if (form.emotionClassifierEnabled) {
    if (!form.emotionClassifierProviderId.trim() || !form.emotionClassifierModel.trim()) return { step: 1, message: "启用情绪分类器时需要填写提供方 ID 和模型。" };
    const timeout = Number(form.emotionClassifierTimeoutMs);
    const maxTokens = Number(form.emotionClassifierMaxTokens);
    if (!Number.isFinite(timeout) || timeout < 500 || timeout > 1500) return { step: 1, message: "情绪分类器超时必须在 500–1500ms 之间。" };
    if (!Number.isFinite(maxTokens) || maxTokens < 32 || maxTokens > 512) return { step: 1, message: "情绪分类器 Token 上限必须在 32–512 之间。" };
  }
  if (form.voiceEnabled) {
    if (!form.voiceProvider.trim() || !form.voiceProtocol.trim()) return { step: 2, message: "填写 TTS 服务名称并选择协议。" };
    if (!form.voiceEndpoint.trim() || !form.voiceModel.trim()) return { step: 2, message: "启用 TTS 时需要填写接口地址和模型。" };
    if (form.voiceProtocol === "openai" && !form.voiceVoice.trim()) return { step: 2, message: "OpenAI TTS 需要填写音色。" };
  }
  if (form.asrEnabled) {
    if (!form.asrProvider.trim() || !form.asrProtocol.trim()) return { step: 2, message: "填写 ASR 服务名称并选择协议。" };
    if (!form.asrEndpoint.trim()) return { step: 2, message: "启用 ASR 时需要填写接口地址。" };
    if (form.asrProtocol !== "fish" && !form.asrModel.trim()) return { step: 2, message: "当前 ASR 协议需要填写模型。" };
  }
  return null;
}

export function applyVoicePreset(form, preset) {
  const presets = {
    fish: { voiceProvider: "Fish Audio", voiceProtocol: "fish", voiceEndpoint: "https://api.fish.audio/v1/tts", voiceModel: "s2.1-pro-free", voiceApiKeyEnv: "FISH_AUDIO_API_KEY", voiceVoice: "", voiceFormat: "mp3" },
    qwen: { voiceProvider: "Qwen Omni", voiceProtocol: "qwen_omni", voiceEndpoint: "https://dashscope.aliyuncs.com/compatible-mode/v1", voiceModel: "qwen3.5-omni-flash", voiceApiKeyEnv: "QWEN_API_KEY", voiceVoice: "Tina", voiceFormat: "wav" },
    openai: { voiceProvider: "OpenAI Compatible", voiceProtocol: "openai", voiceEndpoint: "https://api.openai.com/v1/audio/speech", voiceModel: "gpt-4o-mini-tts", voiceApiKeyEnv: "FURINA_TTS_API_KEY", voiceVoice: "alloy", voiceFormat: "mp3" },
  };
  return { ...form, ...(presets[preset] || {}) };
}

export function applyAsrPreset(form, preset) {
  const presets = {
    fish: { asrProvider: "Fish Audio", asrProtocol: "fish", asrEndpoint: "https://api.fish.audio/v1/asr", asrModel: "", asrApiKeyEnv: "FISH_AUDIO_API_KEY" },
    qwen: { asrProvider: "Qwen Omni", asrProtocol: "qwen_omni", asrEndpoint: "https://dashscope.aliyuncs.com/compatible-mode/v1", asrModel: "qwen3.5-omni-flash", asrApiKeyEnv: "QWEN_API_KEY", asrPrompt: "请只输出转写结果。" },
    openai: { asrProvider: "OpenAI Compatible", asrProtocol: "openai", asrEndpoint: "https://api.openai.com/v1/audio/transcriptions", asrModel: "whisper-1", asrApiKeyEnv: "FURINA_ASR_API_KEY" },
  };
  return { ...form, ...(presets[preset] || {}) };
}

export function normalizeMigrationCandidates(items) {
  return (Array.isArray(items) ? items : []).filter((item) => item && typeof item.root === "string" && item.root.trim());
}

export function projectDiagnosticRows(diagnostics, status = {}) {
  if (!diagnostics) return [];
  const sidecarReady = diagnostics.sidecar?.running ?? diagnostics.sidecar?.available ?? false;
  const sidecarDetail = sidecarReady
    ? `独立工具进程可用${diagnostics.sidecar?.version ? ` · v${diagnostics.sidecar.version}` : ""}`
    : diagnostics.sidecar?.error || "不可用，工具能力将降级";
  const serviceErrors = diagnostics.errors || diagnostics.service_errors || [];
  const ttsError = serviceErrors.find((item) => String(item).startsWith("TTS:"));
  const asrError = serviceErrors.find((item) => String(item).startsWith("ASR:"));
  return [
    ["LLM", diagnostics.llmResult || "未测试", diagnostics.llmOk ?? Boolean(diagnostics.llmResult)],
    ["情绪分类器", diagnostics.emotionClassifierResult || (status.emotionClassifier?.enabled ? "未测试" : "未启用"), diagnostics.emotionClassifierOk ?? !status.emotionClassifier?.enabled],
    ["Sidecar", sidecarDetail, Boolean(sidecarReady)],
    ["TTS", diagnostics.voice ? "已就绪" : status.voice?.enabled ? (ttsError || "配置不可用，已降级") : "未启用", Boolean(diagnostics.voice) || !status.voice?.enabled],
    ["ASR", diagnostics.asr ? "已就绪" : status.asr?.enabled ? (asrError || "配置不可用，已降级") : "未启用", Boolean(diagnostics.asr) || !status.asr?.enabled],
    ["Git", diagnostics.git ? "可用" : "未检测到（仅影响 Git 工具）", true],
  ];
}
