import { describe, expect, it } from "vitest";
import {
  audioLevelFromTimeDomain,
  normalizeAudioLevel,
  rmsFromTimeDomain,
  smoothAudioLevel,
} from "./audioLevel.js";

describe("audio level analysis", () => {
  it("returns zero for silence and a positive level for speech-like samples", () => {
    expect(rmsFromTimeDomain(new Uint8Array([128, 128, 128]))).toBe(0);
    expect(audioLevelFromTimeDomain(new Uint8Array([128, 160, 96]))).toBeGreaterThan(0);
  });

  it("gates low-level noise and clamps loud input", () => {
    expect(normalizeAudioLevel(0.01)).toBe(0);
    expect(normalizeAudioLevel(2)).toBe(1);
  });

  it("uses faster attack and slower release for stable lip motion", () => {
    const attack = smoothAudioLevel(0, 1, 0.05);
    const release = smoothAudioLevel(1, 0, 0.05);
    expect(attack).toBeGreaterThan(1 - release);
    expect(smoothAudioLevel(0.4, 0.4, 0.05)).toBeCloseTo(0.4);
  });
});
