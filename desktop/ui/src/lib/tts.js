// TTS 预合成管线：合成端边收边合成、播放端顺序播放（句间 280ms 自然停顿）。
// stop() 递增令牌，使在途合成结果作废，避免旧语音重新入队。

export class TtsPipeline {
  constructor({ invoke, emotionFor, speedFor, voiceProfileFor, onStatus, onSpeaking }) {
    this.invoke = invoke;
    this.emotionFor = emotionFor;
    this.speedFor = speedFor;
    this.voiceProfileFor = voiceProfileFor || (() => ({
      cue: this.emotionFor ? this.emotionFor() : "",
      speed: this.speedFor ? this.speedFor() : 1,
    }));
    this.onStatus = onStatus || (() => {});
    this.onSpeaking = onSpeaking || (() => {});
    this.ttsQueue = [];
    this.playQueue = [];
    this.playing = false;
    this.synthesizing = false;
    this.synthToken = 0;
    this.currentAudio = null;
    this.enabled = true;
  }

  speak(text) {
    if (!this.enabled) return;
    this.ttsQueue.push(text);
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
      } catch (e) {
        if (!this.enabled || token !== this.synthToken) break;
        this.onStatus("⚠️ 语音合成失败：" + (e && e.message ? e.message : e));
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
        await new Promise((r) => setTimeout(r, 280));
      } catch (e) {
        if (!this.enabled) break;
        this.onStatus("⚠️ 语音播放失败：" + (e && e.message ? e.message : e));
      } finally {
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

  stop() {
    if (this.currentPlaybackResolve) this.currentPlaybackResolve();
    if (this.currentAudio) {
      this.currentAudio.pause();
      this.currentAudio = null;
    }
    this.ttsQueue = [];
    this.playQueue = [];
    this.playing = false;
    this.synthToken++;
    this.onSpeaking(false);
  }
}
