import { describe, expect, it, vi } from "vitest";
import { TtsPipeline } from "./tts.js";

function audioResponse() {
  return { format: "wav", data: [0, 1, 2, 3] };
}

describe("TtsPipeline emotion profile", () => {
  it("passes the current cue and speed to synthesis", async () => {
    const invoke = vi.fn().mockResolvedValue(audioResponse());
    const audio = { play: vi.fn().mockResolvedValue(undefined), pause: vi.fn() };
    vi.stubGlobal("Audio", vi.fn(() => audio));
    vi.stubGlobal("URL", {
      createObjectURL: vi.fn(() => "blob:voice"),
      revokeObjectURL: vi.fn(),
    });
    const pipeline = new TtsPipeline({
      invoke,
      voiceProfileFor: () => ({
        cue: "[angry]",
        speed: 1.12,
        volume: 4,
        normalizeLoudness: false,
        temperature: 0.8,
        topP: 0.85,
      }),
    });

    pipeline.speak("别再这样了");
    await vi.waitFor(() => expect(invoke).toHaveBeenCalled());

    expect(invoke).toHaveBeenCalledWith("tts_synthesize", {
      text: "别再这样了",
      emotion: "[angry]",
      speed: 1.12,
      profile: {
        emotion: "[angry]",
        speed: 1.12,
        volume: 4,
        normalizeLoudness: false,
        temperature: 0.8,
        topP: 0.85,
      },
    });
    pipeline.stop();
    vi.unstubAllGlobals();
  });

  it("invalidates queued synthesis after stop", async () => {
    let resolve;
    const invoke = vi.fn(() => new Promise((r) => { resolve = r; }));
    const pipeline = new TtsPipeline({
      invoke,
      voiceProfileFor: () => ({ cue: "[happy]", speed: 1.04 }),
    });

    pipeline.speak("旧句子");
    await vi.waitFor(() => expect(invoke).toHaveBeenCalled());
    pipeline.stop();
    resolve(audioResponse());
    await Promise.resolve();
    expect(pipeline.playQueue).toHaveLength(0);
  });
});
