import { describe, expect, it, vi } from "vitest";
import { VrmAvatarProvider } from "./VrmAvatarProvider.js";

function providerHarness(overrides = {}) {
  return Object.assign(Object.create(VrmAvatarProvider.prototype), {
    context: {
      conversationState: "idle",
      mood: "calm",
      intensity: 0,
      speaking: false,
      recording: false,
      thinking: false,
      blocked: false,
    },
    state: { mood: "calm", intensity: 0, speaking: false },
    interaction: { focused: true },
    finishedRemaining: 0,
    poseBones: new Map(),
    scheduler: { request: vi.fn((intent) => intent) },
    ...overrides,
  });
}

describe("VrmAvatarProvider interaction context", () => {
  it("enters a short finished transition after active conversation returns idle", () => {
    const provider = providerHarness({
      context: {
        conversationState: "talking",
        mood: "happy",
        intensity: 0.6,
        speaking: true,
        recording: false,
        thinking: false,
        blocked: false,
      },
    });

    provider.setInteractionContext({ conversationState: "idle", speaking: false });

    expect(provider.finishedRemaining).toBe(0.9);
    expect(provider.state).toEqual({ mood: "happy", intensity: 0.6, speaking: false });
  });

  it("does not restart the finished transition on repeated idle updates", () => {
    const provider = providerHarness({ finishedRemaining: 0.4 });

    provider.setInteractionContext({ conversationState: "idle", speaking: false });

    expect(provider.finishedRemaining).toBe(0.4);
  });

  it.each([
    ["recording", { recording: true }],
    ["thinking", { thinking: true }],
    ["speaking", { speaking: true }],
    ["approval", { blocked: true }],
  ])("blocks greeting while %s is active", (_label, flags) => {
    const provider = providerHarness();
    provider.context = { ...provider.context, ...flags };

    expect(provider.handleIntent({ type: "greeting" })).toBeNull();
    expect(provider.scheduler.request).not.toHaveBeenCalled();
  });

  it("defers controller greetings while the provider is busy", () => {
    const provider = providerHarness();
    provider.context = { ...provider.context, thinking: true };

    expect(provider.handleIntent({
      type: "behavior_action",
      action: "greeting_wave",
    })).toEqual({ deferred: true });
    expect(provider.scheduler.request).not.toHaveBeenCalled();
  });

  it("blocks focus greeting while the window is unfocused", () => {
    const provider = providerHarness({ interaction: { focused: false } });

    expect(provider.handleIntent({ type: "focus_return" })).toBeNull();
    expect(provider.scheduler.request).not.toHaveBeenCalled();
  });

  it("does not queue greeting during an active or pending interaction", () => {
    const provider = providerHarness({
      scheduler: {
        current: { priority: 40 },
        pending: null,
        request: vi.fn(),
      },
    });

    expect(provider.handleIntent({ type: "focus_return" })).toBeNull();
    expect(provider.scheduler.request).not.toHaveBeenCalled();
  });

  it("lets a new active conversation cancel a finished transition", () => {
    const provider = providerHarness({ finishedRemaining: 0.7 });

    provider.setInteractionContext({ conversationState: "thinking", thinking: true });

    expect(provider.finishedRemaining).toBe(0);
  });

  it("forwards allowed visual interaction without touching speech state", () => {
    const provider = providerHarness();

    expect(provider.handleIntent({ type: "pointer_click", zone: "head" })).toEqual({
      type: "pointer_click",
      zone: "head",
    });
    expect(provider.state.speaking).toBe(false);
  });

  it("safely ignores pose updates for missing bones", () => {
    const provider = providerHarness();

    expect(() => provider.applyPoseBone("leftUpperArm", 0, 0, -1.2, 1)).not.toThrow();
  });
});

describe("VrmAvatarProvider audio state", () => {
  it("clamps live audio levels before the renderer consumes them", () => {
    const provider = providerHarness();

    provider.setAudioLevel(2);
    expect(provider.context.audioLevel).toBe(1);
    provider.setAudioLevel(-1);
    expect(provider.context.audioLevel).toBe(0);
  });
});