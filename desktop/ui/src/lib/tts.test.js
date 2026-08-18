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

  it("uses filtered text and skips aside-only speech", async () => {
    const invoke = vi.fn().mockResolvedValue(audioResponse());
    const onSpeaking = vi.fn();
    const pipeline = new TtsPipeline({ invoke, onSpeaking });

    pipeline.beginResponse();
    pipeline.speak("（微笑）你好。");
    await vi.waitFor(() => expect(invoke).toHaveBeenCalled());
    expect(invoke.mock.calls[0][1].text).toBe("你好。");

    pipeline.stop();
    invoke.mockClear();
    onSpeaking.mockClear();
    pipeline.beginResponse();
    pipeline.speak("（沉默片刻）");
    pipeline.finishResponse();
    await Promise.resolve();
    expect(invoke).not.toHaveBeenCalled();
    expect(onSpeaking).not.toHaveBeenCalledWith(true);
  });

  it("flushes streaming asides and keeps immediate interjections independent", async () => {
    const invoke = vi.fn().mockResolvedValue(audioResponse());
    const pipeline = new TtsPipeline({ invoke });

    pipeline.beginResponse();
    pipeline.speak("你好（轻轻");
    pipeline.speak("叹气）。");
    pipeline.finishResponse();
    pipeline.speakImmediate("（点头）收到。");
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));

    expect(invoke.mock.calls.map((call) => call[1].text)).toEqual(["你好", "收到。"]);
    pipeline.stop();
  });
  it("starts synthesizing the first sentence during a long response", async () => {
    const invoke = vi.fn().mockResolvedValue(audioResponse());
    const pipeline = new TtsPipeline({ invoke });

    pipeline.beginResponse();
    pipeline.speak("第一句先开始朗读。");
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    for (let sentenceNumber = 2; sentenceNumber <= 40; sentenceNumber += 1) {
      pipeline.speak(`第${sentenceNumber}句继续输出。`);
    }
    expect(invoke).toHaveBeenCalled();
    pipeline.stop();
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
