const BRACKET_PAIRS = new Map([
  ["（", "）"],
  ["(", ")"],
  ["【", "】"],
  ["[", "]"],
  ["「", "」"],
  ["『", "』"],
]);

const CLOSE_TO_OPEN = new Map(
  [...BRACKET_PAIRS.entries()].map(([opening, closing]) => [closing, opening])
);

const ASIDE_CUE_RE = /(?:动作|旁白|内心|心想|心理活动|画外音|os|微笑|轻笑|笑了|笑着|叹气|点头|摇头|眨眼|挑眉|挥手|看向|靠近|转身|沉默|停顿|愣住|脸红|耳朵发红|紧张|困惑|疑惑|惊讶|吸气|呼气|哭声|笑声|脚步声|风声|音乐声)/iu;
const NARRATION_PREFIX_RE = /^(?:动作|旁白|内心|心想|心理活动|画外音|os)\s*[:：]/iu;
const EMOJI_RE = /(?:\p{Extended_Pictographic}|\p{Emoji_Presentation}|\p{Emoji_Modifier}|\uFE0F|\u200D)+/gu;
const EMOTICON_RE = /(?:[:;=8][\-^']?[)(/\\DPp]|[Tt]_[Tt]|[xX]D)/gu;
const LINK_RE = /\[([^\]\n]+)\]\((?:[^()\n]|\([^()\n]*\))*\)/gu;
const LINK_AT_START_RE = /^\[([^\]\n]+)\]\((?:[^()\n]|\([^()\n]*\))*\)/u;

function removeEmoji(text) {
  return text.replace(EMOJI_RE, "").replace(EMOTICON_RE, "");
}

function hasSpokenCharacters(text) {
  return /[\p{L}\p{N}]/u.test(text);
}

function isAsideContent(content) {
  const raw = String(content);
  const normalized = removeEmoji(raw).replace(/[\s\p{P}\p{S}]+/gu, "").trim();
  if (!normalized) return true;
  if (NARRATION_PREFIX_RE.test(raw.trim())) return true;
  return ASIDE_CUE_RE.test(raw) && !/[。！？!?；;，,].{12,}/u.test(raw);
}

function normalizePlainText(text) {
  let normalized = String(text)
    .replace(/\r\n?/gu, "\n")
    .replace(LINK_RE, "$1")
    .replace(/`([^`\n]+)`/gu, "$1")
    .replace(/^\s*(?:#{1,6}\s+|[-*+]\s+|>\s*|\|\s*)/gmu, "")
    .replace(/\*\*([^*\n]+)\*\*/gu, "$1");
  normalized = removeEmoji(normalized).replace(/\*([^*\n]{1,80})\*/gu, (match, content) => (
    isAsideContent(content) ? "" : content
  ));
  return normalized
    .replace(/[ \t]+/gu, " ")
    .replace(/[ \t]*([，。！？!?；;])\s*/gu, "$1")
    .replace(/\n{2,}/gu, "\n")
    .trim();
}

function firstOpeningIndex(text) {
  let earliest = -1;
  for (const opening of BRACKET_PAIRS.keys()) {
    const position = text.indexOf(opening);
    if (position >= 0 && (earliest < 0 || position < earliest)) earliest = position;
  }
  return earliest;
}

function matchingBracketIndex(text, start) {
  const stack = [text[start]];
  for (let position = start + 1; position < text.length; position += 1) {
    const character = text[position];
    if (BRACKET_PAIRS.has(character)) {
      stack.push(character);
      continue;
    }
    if (CLOSE_TO_OPEN.has(character) && stack[stack.length - 1] === CLOSE_TO_OPEN.get(character)) {
      stack.pop();
      if (stack.length === 0) return position;
    }
  }
  return -1;
}

function pushText(output, text) {
  const normalized = normalizePlainText(text);
  if (!normalized) return;
  if (!hasSpokenCharacters(normalized) && output.length === 0) return;
  if (output.length > 0 && !/\s$/u.test(output[output.length - 1]) && !/^\s/u.test(normalized)) {
    output[output.length - 1] += normalized;
  } else {
    output.push(normalized);
  }
}

export class SpeechTextFilter {
  constructor() {
    this.reset();
  }

  reset() {
    this.pending = "";
    this.inCodeFence = false;
    this.suppressLeadingPunctuation = false;
  }

  push(fragment) {
    if (fragment) this.pending += String(fragment);
    return this.consume(false);
  }

  flush() {
    const output = this.consume(true);
    if (this.pending) {
      const unmatched = this.pending;
      this.pending = "";
      if (!this.inCodeFence) {
        const opening = unmatched[0];
        const content = unmatched.slice(1);
        if (!BRACKET_PAIRS.has(opening)) {
          this.appendText(output, unmatched);
        } else if (!isAsideContent(content)) {
          this.appendText(output, content);
        }
      }
    }
    this.pending = "";
    this.inCodeFence = false;
    this.suppressLeadingPunctuation = false;
    return output;
  }

  appendText(output, text) {
    let normalized = normalizePlainText(text);
    if (!normalized) return;
    if (this.suppressLeadingPunctuation) {
      normalized = normalized.replace(/^[，。！？!?；;、]+/u, "");
      if (!normalized) return;
      this.suppressLeadingPunctuation = false;
    }
    pushText(output, normalized);
  }

  consume(force) {
    const output = [];
    while (this.pending) {
      if (this.inCodeFence) {
        const closingFence = this.pending.indexOf("```");
        if (closingFence < 0) {
          if (force) this.pending = "";
          break;
        }
        this.pending = this.pending.slice(closingFence + 3);
        this.inCodeFence = false;
        continue;
      }

      const fenceIndex = this.pending.indexOf("```");
      const openingIndex = firstOpeningIndex(this.pending);
      if (fenceIndex >= 0 && (openingIndex < 0 || fenceIndex < openingIndex)) {
        this.appendText(output, this.pending.slice(0, fenceIndex));
        this.pending = this.pending.slice(fenceIndex + 3);
        this.inCodeFence = true;
        continue;
      }

      if (openingIndex < 0) {
        this.appendText(output, this.pending);
        this.pending = "";
        break;
      }

      if (openingIndex > 0) {
        this.appendText(output, this.pending.slice(0, openingIndex));
        this.pending = this.pending.slice(openingIndex);
      }

      const linkMatch = this.pending.match(LINK_AT_START_RE);
      if (linkMatch) {
        this.appendText(output, linkMatch[1]);
        this.pending = this.pending.slice(linkMatch[0].length);
        continue;
      }

      const closingIndex = matchingBracketIndex(this.pending, 0);
      if (closingIndex < 0) {
        if (force) {
          const unmatched = this.pending;
          this.pending = "";
          const content = unmatched.slice(1);
          if (!isAsideContent(content)) this.appendText(output, content);
        }
        break;
      }

      const content = this.pending.slice(1, closingIndex);
      if (!isAsideContent(content)) {
        this.appendText(output, content);
      } else {
        this.suppressLeadingPunctuation = true;
      }
      this.pending = this.pending.slice(closingIndex + 1);
    }
    return output;
  }
}

export function filterSpeechText(text) {
  const filter = new SpeechTextFilter();
  return filter.push(text).concat(filter.flush()).join("");
}
