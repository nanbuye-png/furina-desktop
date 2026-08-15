const STATE_PRIORITY = {
  calm: 0,
  happy: 1,
  proud: 2,
  sad: 3,
  hurt: 4,
  angry: 5,
};

const clamp = (value, min, max) => Math.min(max, Math.max(min, value));
const mix = (value, target, weight) => value * (1 - weight) + target * weight;

function numberOr(value, fallback) {
  const number = Number(value);
  return Number.isFinite(number) ? number : fallback;
}

function intensityFor(soul) {
  const affectIntensity = numberOr(soul?.affect?.intensity, NaN);
  if (Number.isFinite(affectIntensity)) return clamp(affectIntensity / 100, 0, 1);
  return clamp(numberOr(soul?.emotion_profile?.intensity, 0), 0, 1);
}

function moodFor(soul) {
  return soul?.affect?.primary || soul?.mood || "calm";
}

function stateFor(soul, intensity) {
  const mood = moodFor(soul);
  const stress = numberOr(soul?.emotions?.stress, 0);
  if (mood === "annoyed" || (stress >= 70 && intensity >= 0.35)) return "angry";
  if (mood === "hurt") return "hurt";
  if (mood === "sad") return "sad";
  if (mood === "proud") return "proud";
  if (mood === "happy") return "happy";
  return "calm";
}

function cueFor(state, intensity, trend, stress) {
  switch (state) {
    case "angry":
      if (intensity >= 0.85 && trend === "rising" && stress >= 75) {
        return "[shouting][extremely angry]";
      }
      if (intensity >= 0.72) return "[extremely angry]";
      if (intensity >= 0.42) return "[angry]";
      return "[frustrated]";
    case "happy":
      return intensity >= 0.72 ? "[delighted]" : "[happy]";
    case "proud":
      return intensity >= 0.72 ? "[confident]" : "[proud]";
    case "hurt":
      return intensity >= 0.68 ? "[sad]" : "[disappointed]";
    case "sad":
      return intensity >= 0.78 ? "[sighing][sad]" : "[sad]";
    default:
      return "";
  }
}

function rangeValue([min, max], intensity) {
  return min + (max - min) * intensity;
}

function acousticProfileFor(state, intensity) {
  const profiles = {
    calm: { speed: [0.92, 0.97], volume: [0, 0], temperature: [0.55, 0.55], topP: 0.75 },
    happy: { speed: [0.98, 1.06], volume: [1, 2], temperature: [0.62, 0.68], topP: 0.82 },
    proud: { speed: [0.97, 1.04], volume: [1, 2], temperature: [0.58, 0.64], topP: 0.78 },
    hurt: { speed: [0.87, 0.94], volume: [-1, -3], temperature: [0.5, 0.46], topP: 0.7 },
    sad: { speed: [0.85, 0.93], volume: [-2, -4], temperature: [0.48, 0.42], topP: 0.68 },
    angry: { speed: [1.03, 1.12], volume: [2, 5], temperature: [0.72, 0.85], topP: 0.85 },
  };
  const profile = profiles[state] || profiles.calm;
  return {
    speed: rangeValue(profile.speed, intensity),
    volume: rangeValue(profile.volume, intensity),
    temperature: rangeValue(profile.temperature, intensity),
    topP: profile.topP,
  };
}

export function voiceProfileFor(soul, manualSpeed = 1) {
  const intensity = intensityFor(soul);
  const state = stateFor(soul, intensity);
  const emotionProfile = soul?.emotion_profile || {};
  const trend = ["rising", "stable", "recovering"].includes(emotionProfile.trend)
    ? emotionProfile.trend
    : soul?.affect?.trend || "stable";
  const valence = clamp(numberOr(emotionProfile.valence, 0), -1, 1);
  const arousal = clamp(numberOr(emotionProfile.arousal, 0), 0, 1);
  const stress = clamp(numberOr(soul?.emotions?.stress, 0), 0, 100);
  const userSpeed = clamp(numberOr(manualSpeed, 1), 0.8, 1.2);
  const acoustic = acousticProfileFor(state, intensity);

  if (trend === "recovering") {
    acoustic.speed = mix(acoustic.speed, 0.95, 0.4);
    acoustic.volume = mix(acoustic.volume, 0, 0.45);
    acoustic.temperature = mix(acoustic.temperature, 0.55, 0.4);
  }

  return {
    state,
    cue: cueFor(state, intensity, trend, stress),
    speed: clamp(acoustic.speed + (userSpeed - 1) * 0.25, 0.8, 1.2),
    volume: clamp(acoustic.volume, -6, 6),
    normalizeLoudness: false,
    temperature: clamp(acoustic.temperature, 0, 1),
    topP: clamp(acoustic.topP, 0, 1),
    intensity,
    valence,
    arousal,
    trend,
  };
}

export class VoiceEmotionController {
  constructor() {
    this.current = null;
  }

  next(soul, manualSpeed = 1) {
    const candidate = voiceProfileFor(soul, manualSpeed);
    if (!this.current) {
      this.current = candidate;
      return candidate;
    }

    const stateChanged = candidate.state !== this.current.state;
    const intensityChanged = Math.abs(candidate.intensity - this.current.intensity) >= 0.08;
    const trendReversed =
      (this.current.trend === "recovering" && candidate.trend === "rising") ||
      (this.current.trend === "rising" && candidate.trend === "recovering");

    if (stateChanged || intensityChanged || trendReversed) {
      this.current = candidate;
      return candidate;
    }

    this.current = {
      ...candidate,
      state: this.current.state,
      cue: this.current.cue,
    };
    return this.current;
  }

  reset() {
    this.current = null;
  }
}

export { STATE_PRIORITY };
