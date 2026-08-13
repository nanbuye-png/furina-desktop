import { describe, expect, it } from "vitest";
import { applyAsrPreset, applyVoicePreset, createInitialSetupForm, normalizeMigrationCandidates, projectDiagnosticRows, validateSetupForm } from "./setupModel.js";

describe("setup model", () => {
  it("requires an LLM key on first setup", () => {
    const form = createInitialSetupForm({ runtime: { workspace_root: "D:/workspace" } });
    expect(validateSetupForm(form, { llm: { apiKeyConfigured: false } })).toEqual({
      step: 1,
      message: "首次设置需要填写 LLM API Key。",
    });
  });

  it("defaults optional voice services and classifier to disabled", () => {
    const form = createInitialSetupForm({ runtime: { workspace_root: "D:/workspace" } });
    expect(form.voiceEnabled).toBe(false);
    expect(form.asrEnabled).toBe(false);
    expect(form.emotionClassifierEnabled).toBe(false);
    expect(form.emotionClassifierTimeoutMs).toBe(1500);
  });

  it("restores and validates emotion classifier advanced settings", () => {
    const form = createInitialSetupForm({
      runtime: { workspace_root: "D:/workspace" },
      llm: { apiKeyConfigured: true, model: "main-model" },
      emotionClassifier: { enabled: true, providerId: "desktop", model: "small-model", timeoutMs: 1200, maxTokens: 128 },
    });
    expect(form.emotionClassifierModel).toBe("small-model");
    expect(validateSetupForm(form, { llm: { apiKeyConfigured: true } })).toBeNull();
    expect(validateSetupForm({ ...form, emotionClassifierTimeoutMs: 2000 }, { llm: { apiKeyConfigured: true } })).toEqual({
      step: 1,
      message: "情绪分类器超时必须在 500–1500ms 之间。",
    });
  });

  it("restores independent custom TTS and ASR settings", () => {
    const form = createInitialSetupForm({
      runtime: { workspace_root: "D:/workspace" },
      voice: { enabled: true, provider: "Local TTS", protocol: "openai", endpoint: "http://127.0.0.1:9001/v1/audio/speech", model: "tts-local", voice: "furina", apiKeyEnv: "LOCAL_TTS_KEY" },
      asr: { enabled: true, provider: "Local ASR", protocol: "openai", endpoint: "http://127.0.0.1:9002/v1/audio/transcriptions", model: "whisper-local", language: "zh", apiKeyEnv: "LOCAL_ASR_KEY" },
    });
    expect(form.voiceEndpoint).toContain("9001");
    expect(form.asrEndpoint).toContain("9002");
    expect(form.voiceApiKeyEnv).toBe("LOCAL_TTS_KEY");
    expect(form.asrApiKeyEnv).toBe("LOCAL_ASR_KEY");
  });

  it("presets fill fields without locking custom values", () => {
    const base = createInitialSetupForm({ runtime: { workspace_root: "D:/workspace" } });
    expect(applyVoicePreset(base, "openai").voiceProtocol).toBe("openai");
    expect(applyAsrPreset(base, "fish").asrProtocol).toBe("fish");
  });

  it("keeps valid migration candidates only", () => {
    expect(normalizeMigrationCandidates([{ root: "D:/old" }, null, { root: "" }])).toEqual([{ root: "D:/old" }]);
  });

  it("marks failed LLM and degraded voice services as warnings", () => {
    const rows = projectDiagnosticRows({ llmResult: "连接测试失败", llmOk: false, voice: false, asr: false, errors: ["TTS: key missing", "ASR: endpoint invalid"] }, { voice: { enabled: true }, asr: { enabled: true } });
    expect(rows.find(([name]) => name === "LLM")[2]).toBe(false);
    expect(rows.find(([name]) => name === "TTS")[1]).toContain("key missing");
    expect(rows.find(([name]) => name === "ASR")[2]).toBe(false);
  });

  it("projects disabled voice services as healthy", () => {
    const rows = projectDiagnosticRows({ sidecar: { available: false }, git: false }, { voice: { enabled: false }, asr: { enabled: false } });
    expect(rows.find(([name]) => name === "TTS")[2]).toBe(true);
    expect(rows.find(([name]) => name === "ASR")[2]).toBe(true);
  });
});
