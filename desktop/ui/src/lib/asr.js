// PTT 录音：直接采集 PCM 并编码 16kHz 单声道 WAV（qwen ASR 只接受 wav）。
// 绕过 MediaRecorder 的 webm/opus（无法被 decodeAudioData 可靠解码）。

const ASR_SAMPLE_RATE = 16000;

export class PcmRecorder {
  constructor({ onStatus } = {}) {
    this.onStatus = onStatus || (() => {});
    this.stream = null;
    this.audioCtx = null;
    this.sourceNode = null;
    this.scriptNode = null;
    this.silentGain = null;
    this.pcmChunks = [];
    this.recording = false;
  }

  async start() {
    if (this.recording) return;
    try {
      this.stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    } catch (e) {
      this.onStatus("⚠️ 无法访问麦克风：" + e.message);
      return false;
    }
    try {
      const AC = window.AudioContext || window.webkitAudioContext;
      this.audioCtx = new AC({ sampleRate: ASR_SAMPLE_RATE });
    } catch (_) {
      const AC = window.AudioContext || window.webkitAudioContext;
      this.audioCtx = new AC();
    }
    this.pcmChunks = [];
    try {
      this.sourceNode = this.audioCtx.createMediaStreamSource(this.stream);
      this.scriptNode = this.audioCtx.createScriptProcessor(4096, 1, 1);
      this.scriptNode.onaudioprocess = (e) => {
        this.pcmChunks.push(new Float32Array(e.inputBuffer.getChannelData(0)));
      };
      this.silentGain = this.audioCtx.createGain();
      this.silentGain.gain.value = 0;
      this.sourceNode.connect(this.scriptNode);
      this.scriptNode.connect(this.silentGain);
      this.silentGain.connect(this.audioCtx.destination);
    } catch (e) {
      this.onStatus("⚠️ 录音初始化失败：" + e.message);
      this.cleanup();
      return false;
    }
    this.recording = true;
    return true;
  }

  /** 停止并返回 16kHz 单声道 WAV 字节（Uint8Array），无录音时返回 null。 */
  stop() {
    if (!this.recording) return null;
    this.recording = false;
    const srcRate = this.audioCtx ? this.audioCtx.sampleRate : ASR_SAMPLE_RATE;
    const wav = encodeWav16kMono(this.pcmChunks, srcRate);
    this.cleanup();
    return wav.length === 0 ? null : wav;
  }

  cleanup() {
    for (const n of [this.sourceNode, this.scriptNode, this.silentGain]) {
      if (n) {
        try {
          n.disconnect();
        } catch (_) {}
      }
    }
    if (this.stream) {
      this.stream.getTracks().forEach((t) => t.stop());
      this.stream = null;
    }
    if (this.audioCtx) {
      try {
        this.audioCtx.close();
      } catch (_) {}
      this.audioCtx = null;
    }
    this.sourceNode = this.scriptNode = this.silentGain = null;
    this.pcmChunks = [];
  }
}

function encodeWav16kMono(chunks, srcRate) {
  let total = 0;
  for (const c of chunks) total += c.length;
  if (total === 0) return new Uint8Array(0);
  const src = new Float32Array(total);
  let off = 0;
  for (const c of chunks) {
    src.set(c, off);
    off += c.length;
  }
  const ratio = (srcRate || ASR_SAMPLE_RATE) / ASR_SAMPLE_RATE;
  const outLen = Math.max(1, Math.round(total / ratio));
  const out = new Float32Array(outLen);
  for (let i = 0; i < outLen; i++) {
    const pos = i * ratio;
    const i0 = Math.floor(pos);
    const i1 = Math.min(src.length - 1, i0 + 1);
    const frac = pos - i0;
    out[i] = src[i0] * (1 - frac) + src[i1] * frac;
  }
  const buffer = new ArrayBuffer(44 + outLen * 2);
  const view = new DataView(buffer);
  const writeStr = (o, s) => {
    for (let i = 0; i < s.length; i++) view.setUint8(o + i, s.charCodeAt(i));
  };
  writeStr(0, "RIFF");
  view.setUint32(4, 36 + outLen * 2, true);
  writeStr(8, "WAVE");
  writeStr(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, ASR_SAMPLE_RATE, true);
  view.setUint32(28, ASR_SAMPLE_RATE * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  writeStr(36, "data");
  view.setUint32(40, outLen * 2, true);
  let o = 44;
  for (let i = 0; i < outLen; i++) {
    const s = Math.max(-1, Math.min(1, out[i]));
    view.setInt16(o, s < 0 ? s * 0x8000 : s * 0x7fff, true);
    o += 2;
  }
  return new Uint8Array(buffer);
}
