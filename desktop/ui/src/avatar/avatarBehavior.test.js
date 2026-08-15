import { describe, expect, it } from "vitest";
import { AvatarBehaviorController } from "./avatarBehavior.js";

describe("AvatarBehaviorController", () => {
  function createController() {
    let time = 0;
    const requests = [];
    const controller = new AvatarBehaviorController({
      now: () => time,
      requestAction: (intent) => {
        requests.push(intent);
        return { action: intent.action };
      },
    });
    return { controller, requests, advance: (delta) => { time += delta; } };
  }

  it("exposes sustained modes with priority and release", () => {
    const { controller } = createController();
    expect(controller.listen(true)).toBe(true);
    expect(controller.getSnapshot().mode).toBe("listening");
    expect(controller.think(true)).toBe(true);
    expect(controller.getSnapshot().mode).toBe("thinking");
  });

  it("supports mode transitions through the public controller methods", () => {
    const { controller } = createController();
    expect(controller.setMode("listening", true)).toBe(true);
    expect(controller.setMode("thinking", true)).toBe(true);
    expect(controller.setMode("listening", false)).toBe(true);
    expect(controller.setMode("thinking", false)).toBe(true);
    expect(controller.getSnapshot().mode).toBe("idle");
  });

  it("restores a lower-priority mode when the higher-priority mode ends", () => {
    const { controller } = createController();
    controller.listen(true);
    controller.think(true);
    controller.talk(true);

    expect(controller.getSnapshot().mode).toBe("talking");
    expect(controller.talk(false)).toBe(true);
    expect(controller.getSnapshot().mode).toBe("thinking");
    expect(controller.think(false)).toBe(true);
    expect(controller.getSnapshot().mode).toBe("listening");
    expect(controller.listen(false)).toBe(true);
    expect(controller.getSnapshot().mode).toBe("idle");
  });

  it("keeps a gated greeting pending until the provider becomes safe", () => {
    const { controller, requests } = createController();
    let deferred = true;
    controller.requestAction = (intent) => {
      if (deferred) return { deferred: true };
      requests.push(intent);
      return { action: intent.action };
    };

    expect(controller.greet()).toEqual({ accepted: true, queued: true, behavior: "greet" });
    expect(controller.getSnapshot().pending).toBe("greet");
    deferred = false;
    expect(controller.flushPending()).toEqual({
      accepted: true,
      queued: false,
      behavior: "greet",
      action: "greeting_wave",
    });
    expect(requests[0].action).toBe("greeting_wave");
  });

  it("executes acknowledge immediately and queues lifecycle-safe actions", () => {
    const { controller, requests } = createController();
    expect(controller.setMode("thinking", true)).toBe(true);
    expect(controller.acknowledge()).toEqual({ accepted: true, queued: false, behavior: "acknowledge", action: "head_nod" });
    expect(controller.greet()).toEqual({ accepted: true, queued: true, behavior: "greet" });
    expect(requests.map((request) => request.action)).toEqual(["head_nod"]);
    expect(controller.setMode("thinking", false)).toBe(true);
    expect(requests.map((request) => request.action)).toEqual(["head_nod", "greeting_wave"]);
  });

  it("maps reactions and ignores repeated mood values", () => {
    const { controller, requests } = createController();
    expect(controller.observeMood("calm")).toBeNull();
    expect(controller.observeMood("happy").action).toBe("head_blink_smile");
    expect(controller.observeMood("happy")).toBeNull();
    expect(requests[0].action).toBe("head_blink_smile");
  });

  it("resets mode, pending work, cooldowns, and mood history", () => {
    const { controller, advance } = createController();
    controller.setMode("talking", true);
    controller.greet();
    controller.observeMood("happy");
    controller.reset();
    advance(0.1);
    expect(controller.getSnapshot().mode).toBe("idle");
    expect(controller.getSnapshot().pending).toBeNull();
    expect(controller.observeMood("calm")).toBeNull();
  });
});
