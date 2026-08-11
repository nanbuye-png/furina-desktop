// Emotion Projection Function v1（spec §6）：Soul State → Avatar 连续参数。
// 纯函数，输出 0–1；仅展示层使用，不参与任何判断。

const BASELINE = {
  confidence: 45,
  trust: 20,
  attachment: 10,
  energy: 60,
  stress: 20,
  pride: 40,
};

const MOOD_DIMS = {
  proud: ["pride", "confidence"],
  happy: ["energy", "confidence"],
  hurt: ["stress", "trust"],
  sad: ["energy"],
  annoyed: ["stress", "energy"],
  calm: [],
};

const clamp01 = (v) => Math.max(0, Math.min(1, v));

export function projectEmotions(emotions = {}, mood = "calm") {
  const e = { ...BASELINE, ...emotions };
  const valence = clamp01(
    (e.pride + e.confidence + e.trust + e.attachment - e.stress) / 400
  );
  const arousal = clamp01((e.energy + e.stress) / 200);
  const dims = MOOD_DIMS[mood] || [];
  let intensity;
  if (dims.length === 0) {
    intensity = clamp01(Math.abs(e.pride - BASELINE.pride) / 100);
  } else {
    let sum = 0;
    for (const d of dims) sum += Math.abs(e[d] - BASELINE[d]) / 100;
    intensity = clamp01(sum / dims.length);
  }
  return { valence, arousal, intensity };
}
