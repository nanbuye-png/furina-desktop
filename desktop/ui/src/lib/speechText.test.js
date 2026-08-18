import { describe, expect, it } from "vitest";
import { SpeechTextFilter, filterSpeechText } from "./speechText.js";

describe("SpeechTextFilter", () => {
  it("removes action asides while preserving the displayed wording elsewhere", () => {
    expect(filterSpeechText("（微笑）你好，今天过得怎么样？")).toBe("你好，今天过得怎么样？");
    expect(filterSpeechText("我当然会帮你（轻轻叹气）。")).toBe("我当然会帮你");
    expect(filterSpeechText("【疑惑】你确定吗？")).toBe("你确定吗？");
  });

  it("keeps ordinary parenthetical content but removes its delimiters", () => {
    expect(filterSpeechText("版本（推荐）")).toBe("版本推荐");
    expect(filterSpeechText("函数（x）返回结果。")).toBe("函数x返回结果。");
  });

  it("handles asides that span multiple streaming fragments", () => {
    const filter = new SpeechTextFilter();
    expect(filter.push("先说一句（轻轻")).toEqual(["先说一句"]);
    expect(filter.push("叹气）。再说一句。")).toEqual(["再说一句。"]);
    expect(filter.flush()).toEqual([]);
  });

  it("drops nested action asides and emoji without dropping speech", () => {
    expect(filterSpeechText("（看向窗外（沉默片刻））你好 😊")).toBe("你好");
    expect(filterSpeechText("你好，真的很棒！✨")).toBe("你好，真的很棒！");
  });

  it("removes code and markdown noise", () => {
    const text = "哼，易如反掌。\n\n```rust\nlet value = 1;\n```\n\n- 第一点\n- 第二点";
    expect(filterSpeechText(text)).toBe("哼，易如反掌。第一点\n第二点");
    expect(filterSpeechText("查看[项目文档](https://example.com)即可。")).toBe("查看项目文档即可。");
  });

  it("recovers unfinished brackets conservatively at flush", () => {
    const aside = new SpeechTextFilter();
    aside.push("（沉默片刻");
    expect(aside.flush()).toEqual([]);

    const ordinary = new SpeechTextFilter();
    expect(ordinary.push("版本（推荐")).toEqual(["版本"]);
    expect(ordinary.flush()).toEqual(["推荐"]);
  });

  it("resets old streaming state", () => {
    const filter = new SpeechTextFilter();
    filter.push("（微笑");
    filter.reset();
    expect(filter.push("新消息。")).toEqual(["新消息。"]);
  });
});
