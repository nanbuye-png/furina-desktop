import { describe, expect, it } from "vitest";
import { VoiceEmotionController, voiceProfileFor } from "./voiceEmotion.js";

const soul = (mood, intensity, trend = "stable", extras = {}) => ({
  mood,
  affect: { primary: mood, intensity: intensity * 100, trend },
  emotion_profile: {
    valence: mood === "annoyed" || mood === "hurt" || mood === "sad" ? -0.8 : 0.6,
    arousal: intensity,
    intensity,
    trend,
  },
  emotions: { stress: extras.stress ?? 20 },
});

describe("voice emotion profile", () => {
  it.each([
    ["calm", "", "calm"],
    ["happy", "[delighted]", "happy"],
    ["proud", "[confident]", "proud"],
    ["hurt", "[sad]", "hurt"],
    ["sad", "[sighing][sad]", "sad"],
    ["annoyed", "[extremely angry]", "angry"],
  ])("maps %s to an expressive Fish profile", (mood, cue, state) => {
    const profile = voiceProfileFor(soul(mood, 0.8));
    expect(profile.state).toBe(state);
    expect(profile.cue).toBe(cue);
    expect(profile.normalizeLoudness).toBe(false);
    expect(profile.temperature).toBeGreaterThanOrEqual(0);
    expect(profile.temperature).toBeLessThanOrEqual(1);
    expect(profile.topP).toBeGreaterThanOrEqual(0);
    expect(profile.topP).toBeLessThanOrEqual(1);
  });

  it("keeps all profile ranges bounded", () => {
    const profile = voiceProfileFor(soul("annoyed", 1), 2);
    expect(profile.valence).toBeGreaterThanOrEqual(-1);
    expect(profile.valence).toBeLessThanOrEqual(1);
    expect(profile.arousal).toBeGreaterThanOrEqual(0);
    expect(profile.arousal).toBeLessThanOrEqual(1);
    expect(profile.intensity).toBe(1);
    expect(profile.speed).toBeLessThanOrEqual(1.2);
    expect(profile.volume).toBeLessThanOrEqual(6);
  });

  it("uses shouting only for intense rising anger with high stress", () => {
    const ordinary = voiceProfileFor(soul("annoyed", 0.9, "stable", { stress: 90 }));
    const intense = voiceProfileFor(soul("annoyed", 0.9, "rising", { stress: 90 }));
    expect(ordinary.cue).toBe("[extremely angry]");
    expect(intense.cue).toBe("[shouting][extremely angry]");
  });

  it("applies manual speed as a small offset instead of a multiplier", () => {
    const normal = voiceProfileFor(soul("calm", 0.5), 1);
    const adjusted = voiceProfileFor(soul("calm", 0.5), 1.2);
    expect(adjusted.speed - normal.speed).toBeCloseTo(0.05, 5);
    expect(adjusted.speed).toBeLessThan(1.05);
  });

  it("makes stronger angry emotion faster and louder", () => {
    const mild = voiceProfileFor(soul("annoyed", 0.2));
    const strong = voiceProfileFor(soul("annoyed", 0.9));
    expect(strong.speed).toBeGreaterThan(mild.speed);
    expect(strong.volume).toBeGreaterThan(mild.volume);
  });

  it("pulls recovering speech toward calm speed, volume, and temperature", () => {
    const rising = voiceProfileFor(soul("annoyed", 0.8, "rising", { stress: 80 }));
    const recovering = voiceProfileFor(soul("annoyed", 0.8, "recovering", { stress: 80 }));
    expect(recovering.speed).toBeLessThan(rising.speed);
    expect(recovering.volume).toBeLessThan(rising.volume);
    expect(Math.abs(recovering.temperature - 0.55))
      .toBeLessThan(Math.abs(rising.temperature - 0.55));
  });
});

describe("VoiceEmotionController", () => {
  it("does not flap for a small intensity change", () => {
    const controller = new VoiceEmotionController();
    const first = controller.next(soul("happy", 0.5));
    const second = controller.next(soul("happy", 0.55));
    expect(second.cue).toBe(first.cue);
    expect(second.state).toBe(first.state);
  });

  it("switches state on a mood change", () => {
    const controller = new VoiceEmotionController();
    controller.next(soul("happy", 0.5));
    expect(controller.next(soul("annoyed", 0.5)).cue).toBe("[angry]");
  });

  it("resets the current state", () => {
    const controller = new VoiceEmotionController();
    controller.next(soul("happy", 0.5));
    controller.reset();
    expect(controller.current).toBeNull();
  });
});
