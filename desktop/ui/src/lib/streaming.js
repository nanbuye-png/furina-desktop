// 流式文本状态机（与 CLI StreamText 同语义）：
// "---" 是思考/回复的权威分界线；分界线之前静默缓冲并丢弃，之后才是正式回复。

const PREAMBLE_MARKERS = [
  "不需要调用工具",
  "没有技术任务",
  "不需要扫描",
  "无需调用工具",
  "无需工具",
  "属于情感互动",
  "用户只是",
  "不涉及任何",
  "纯聊天",
  "仅聊天",
  "任务分析",
  "分析：",
  "判断：",
];

const MIN_SENTENCE = 6;
const BUFFER_CAP = 400;

export function takeSentences(bufObj) {
  const chars = Array.from(bufObj.text);
  const out = [];
  let start = 0;
  for (let i = 0; i < chars.length; i++) {
    const c = chars[i];
    const end =
      "。！？!?\n；;".includes(c) ||
      (c === "." && (i + 1 >= chars.length || /\s/.test(chars[i + 1])));
    if (!end) continue;
    const s = chars.slice(start, i + 1).join("").trim();
    const tooShort = Array.from(s).length < MIN_SENTENCE;
    const paraBreak = c === "\n" && i + 1 < chars.length && chars[i + 1] === "\n";
    if (s && (!tooShort || paraBreak)) {
      out.push(s);
      start = i + 1;
    }
  }
  bufObj.text = chars.slice(start).join("");
  return out;
}

export class StreamBuffer {
  constructor() {
    this.reset();
  }

  reset() {
    this.pending = "";
    this.flushed = false;
    this.buf = { text: "" };
  }

  /** 注入一个增量块，返回应展示/朗读的完整句子。 */
  feed(delta) {
    if (!this.flushed) {
      this.pending += delta;
      if (this.pending.includes("---")) {
        const idx = this.pending.indexOf("---");
        const rest = this.pending.slice(idx + 3);
        this.pending = "";
        this.flushed = true;
        this.buf.text = rest;
        return takeSentences(this.buf);
      }
      if (PREAMBLE_MARKERS.some((m) => this.pending.includes(m))) {
        return [];
      }
      if (Array.from(this.pending).length > BUFFER_CAP) {
        this.buf.text = this.pending;
        this.pending = "";
        this.flushed = true;
        return takeSentences(this.buf);
      }
      return [];
    }
    this.buf.text += delta;
    return takeSentences(this.buf);
  }

  /** 消息结束：剩余缓冲作为正文放行。 */
  flush() {
    if (!this.flushed) {
      this.buf.text = this.pending;
      this.pending = "";
      this.flushed = true;
    }
    const s = takeSentences(this.buf);
    const rest = this.buf.text.trim();
    if (rest) s.push(rest);
    this.buf.text = "";
    return s;
  }
}
