export function rmsFromTimeDomain(samples) {
  if (!samples?.length) return 0;
  let sum = 0;
  for (const sample of samples) {
    const normalized = (sample - 128) / 128;
    sum += normalized * normalized;
  }
  return Math.sqrt(sum / samples.length);
}

export function normalizeAudioLevel(rms, { noiseFloor = 0.018, speechCeiling = 0.22 } = {}) {
  if (!Number.isFinite(rms) || rms <= noiseFloor) return 0;
  const range = Math.max(speechCeiling - noiseFloor, 0.001);
  const normalized = Math.min(1, Math.max(0, (rms - noiseFloor) / range));
  return Math.pow(normalized, 0.72);
}

export function audioLevelFromTimeDomain(samples, options) {
  return normalizeAudioLevel(rmsFromTimeDomain(samples), options);
}

export function smoothAudioLevel(
  previous,
  next,
  delta,
  { attack = 20, release = 8 } = {},
) {
  const safePrevious = Number.isFinite(previous) ? Math.min(1, Math.max(0, previous)) : 0;
  const safeNext = Number.isFinite(next) ? Math.min(1, Math.max(0, next)) : 0;
  const speed = safeNext > safePrevious ? attack : release;
  const blend = 1 - Math.exp(-Math.max(delta, 0) * speed);
  return safePrevious + (safeNext - safePrevious) * blend;
}
