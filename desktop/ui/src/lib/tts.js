import { audioLevelFromTimeDomain, smoothAudioLevel } from "./audioLevel.js";
import { SpeechTextFilter, filterSpeechText } from "./speechText.js";

// TTS 预合成管线：合成端边收边合成、播放端顺序播放（句间 280ms 自然停顿）。
// stop() 递增令牌，使在途合成结果作废，避免旧语音重新入队。

export class TtsPipeline {
  constructor({
    invoke,
    emotionFor,
    speedFor,
    voiceProfileFor,
    onStatus,
    onSpeaking,
    onAudioLevel,
  }) {
    this.invoke = invoke;
    this.emotionFor = emotionFor;
    this.speedFor = speedFor;
    this.voiceProfileFor = voiceProfileFor || (() => ({
      cue: this.emotionFor ? this.emotionFor() : "",
      speed: this.speedFor ? this.speedFor() : 1,
    }));
    this.onStatus = onStatus || (() => {});
    this.onSpeaking = onSpeaking || (() => {});
    this.onAudioLevel = onAudioLevel || (() => {});
    this.speechFilter = new SpeechTextFilter();
    this.ttsQueue = [];
    this.playQueue = [];
    this.playing = false;
    this.synthesizing = false;
    this.synthToken = 0;
    this.currentAudio = null;
    this.currentPlaybackResolve = null;
    this.enabled = true;
    this.audioContext = null;
    this.audioSource = null;
    this.audioAnalyser = null;
    this.audioLevelBuffer = null;
    this.audioLevel = 0;
    this.audioLevelFrame = null;
    this.audioAnalysisActive = false;
    this.lastAudioSampleAt = 0;
  }

  beginResponse() {
    this.speechFilter.reset();
  }

  speak(text) {
    if (!this.enabled) return;
    for (const speechText of this.speechFilter.push(text)) this.enqueue(speechText);
  }

  finishResponse() {
    if (!this.enabled) {
      this.speechFilter.reset();
      return;
    }
    for (const speechText of this.speechFilter.flush()) this.enqueue(speechText);
  }

  speakImmediate(text) {
    if (!this.enabled) return;
    const speechText = filterSpeechText(text);
    if (speechText) this.enqueue(speechText);
  }

  enqueue(text) {
    const speechText = String(text || '').trim();
    if (!speechText) return;
    this.ttsQueue.push(speechText);
    this.kickSynth();
    this.kickPlay();
  }

  async kickSynth() {
    if (this.synthesizing || this.ttsQueue.length === 0) return;
    this.synthesizing = true;
    const token = ++this.synthToken;
    while (this.ttsQueue.length > 0) {
      const text = this.ttsQueue.shift();
      try {
        const profile = this.voiceProfileFor() || {};
        const emotion = profile.cue || profile.emotion || "";
        const speed = Number.isFinite(profile.speed) ? profile.speed : 1;
        const res = await this.invoke("tts_synthesize", {
          text,
          emotion,
          speed,
          profile: {
            emotion,
            speed,
            volume: Number.isFinite(profile.volume) ? profile.volume : null,
            normalizeLoudness: profile.normalizeLoudness ?? null,
            temperature: Number.isFinite(profile.temperature) ? profile.temperature : null,
            topP: Number.isFinite(profile.topP) ? profile.topP : null,
          },
        });
        if (!this.enabled || token !== this.synthToken) break;
        this.playQueue.push({ text, res });
        this.kickPlay();
      } catch (error) {
        if (!this.enabled || token !== this.synthToken) break;
        this.onStatus("⚠️ 语音合成失败：" + (error && error.message ? error.message : error));
      }
    }
    this.synthesizing = false;
    if (token === this.synthToken && this.ttsQueue.length > 0) this.kickSynth();
  }

  async kickPlay() {
    if (this.playing || this.playQueue.length === 0) return;
    this.playing = true;
    this.onSpeaking(true);
    while (this.playQueue.length > 0 && this.enabled) {
      const { res } = this.playQueue.shift();
      let url = null;
      try {
        const format = (res && res.format) || "mp3";
        const mime =
          format === "wav"
            ? "audio/wav"
            : format === "flac"
              ? "audio/flac"
              : "audio/mpeg";
        const blob = new Blob([new Uint8Array(res.data)], { type: mime });
        url = URL.createObjectURL(blob);
        this.currentAudio = new Audio(url);
        this.startAudioAnalysis(this.currentAudio);
        await new Promise((resolve, reject) => {
          let settled = false;
          const finish = (error) => {
            if (settled) return;
            settled = true;
            this.currentPlaybackResolve = null;
            if (error) reject(error);
            else resolve();
          };
          this.currentPlaybackResolve = () => finish();
          this.currentAudio.onended = () => finish();
          this.currentAudio.onerror = () => finish(new Error("浏览器音频播放失败"));
          this.currentAudio.play().catch((error) => finish(error));
        });
        await new Promise((resolve) => setTimeout(resolve, 280));
      } catch (error) {
        if (!this.enabled) break;
        this.onStatus("⚠️ 语音播放失败：" + (error && error.message ? error.message : error));
      } finally {
        this.stopAudioAnalysis();
        if (this.currentAudio) {
          this.currentAudio.pause();
          this.currentAudio = null;
        }
        if (url) URL.revokeObjectURL(url);
      }
    }
    this.playing = false;
    this.onSpeaking(false);
    if (this.playQueue.length > 0) this.kickPlay();
  }

  startAudioAnalysis(audio) {
    this.stopAudioAnalysis(false);
    const AudioContextClass = globalThis.AudioContext || globalThis.webkitAudioContext;
    if (!AudioContextClass) return false;
    try {
      if (!this.audioContext || this.audioContext.state === "closed") {
        this.audioContext = new AudioContextClass();
      }
      const analyser = this.audioContext.createAnalyser();
      analyser.fftSize = 256;
      analyser.smoothingTimeConstant = 0.55;
      const source = this.audioContext.createMediaElementSource(audio);
      source.connect(analyser);
      analyser.connect(this.audioContext.destination);
      this.audioSource = source;
      this.audioAnalyser = analyser;
      this.audioLevelBuffer = new Uint8Array(analyser.fftSize);
      this.audioLevel = 0;
      this.audioAnalysisActive = true;
      this.lastAudioSampleAt = performance.now();
      if (this.audioContext.state === "suspended") {
        this.audioContext.resume().catch(() => {});
      }
      this.sampleAudioLevel();
      return true;
    } catch (_) {
      this.stopAudioAnalysis();
      return false;
    }
  }

  sampleAudioLevel = () => {
    if (!this.audioAnalysisActive || !this.audioAnalyser || !this.audioLevelBuffer) return;
    this.audioAnalyser.getByteTimeDomainData(this.audioLevelBuffer);
    const currentTime = performance.now();
    const delta = Math.min(Math.max((currentTime - this.lastAudioSampleAt) / 1000, 1 / 240), 0.1);
    this.lastAudioSampleAt = currentTime;
    const rawLevel = audioLevelFromTimeDomain(this.audioLevelBuffer);
    this.audioLevel = smoothAudioLevel(this.audioLevel, rawLevel, delta);
    this.onAudioLevel(this.audioLevel);
    if (typeof globalThis.requestAnimationFrame === "function") {
      this.audioLevelFrame = globalThis.requestAnimationFrame(this.sampleAudioLevel);
    } else {
      this.audioLevelFrame = setTimeout(this.sampleAudioLevel, 16);
    }
  };

  stopAudioAnalysis(notify = true) {
    this.audioAnalysisActive = false;
    if (this.audioLevelFrame !== null) {
      if (typeof globalThis.cancelAnimationFrame === "function") {
        globalThis.cancelAnimationFrame(this.audioLevelFrame);
      } else {
        clearTimeout(this.audioLevelFrame);
      }
      this.audioLevelFrame = null;
    }
    try {
      this.audioSource?.disconnect();
    } catch (_) {}
    try {
      this.audioAnalyser?.disconnect();
    } catch (_) {}
    this.audioSource = null;
    this.audioAnalyser = null;
    this.audioLevelBuffer = null;
    this.audioLevel = 0;
    if (notify) this.onAudioLevel(0);
  }

  stop() {
    if (this.currentPlaybackResolve) this.currentPlaybackResolve();
    if (this.currentAudio) {
      this.currentAudio.pause();
      this.currentAudio = null;
    }
    this.stopAudioAnalysis();
    this.ttsQueue = [];
    this.playQueue = [];
    this.playing = false;
    this.synthToken++;
    this.speechFilter.reset();
    this.onSpeaking(false);
  }

  dispose() {
    this.stop();
    if (this.audioContext && this.audioContext.state !== "closed") {
      this.audioContext.close().catch(() => {});
    }
    this.audioContext = null;
  }
}
