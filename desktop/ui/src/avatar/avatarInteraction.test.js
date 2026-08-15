import { Euler, Quaternion, Vector3 } from "three";
import { describe, expect, it } from "vitest";
import {
  AvatarActionScheduler,
  actionBlinkWeight,
  actionPhase,
  actionWeight,
  clickZoneForPointer,
  clampPoseRotation,
  conversationStateFor,
  greetingPose,
  interactionExpressionTargets,
  interactionZoneForHit,
  normalizePointer,
  smoothFactor,
} from "./avatarInteraction.js";

describe("avatar interaction helpers", () => {
  it("normalizes pointer coordinates and clamps outside values", () => {
    const rect = { left: 10, top: 20, width: 200, height: 100 };

    expect(normalizePointer(rect, 110, 70)).toEqual({ x: 0, y: 0 });
    expect(normalizePointer(rect, -100, 300)).toEqual({ x: -1, y: -1 });
    expect(normalizePointer(rect, 400, -100)).toEqual({ x: 1, y: 1 });
  });

  it("maps pointer fallback regions to body parts", () => {
    expect(clickZoneForPointer({ x: 0, y: 0.5 })).toBe("head");
    expect(clickZoneForPointer({ x: 0.7, y: 0 })).toBe("leftArm");
    expect(clickZoneForPointer({ x: 0, y: -0.3 })).toBe("waist");
    expect(clickZoneForPointer({ x: -0.2, y: -0.8 })).toBe("rightLeg");
  });

  it("maps model hits using avatar-relative coordinates", () => {
    const frame = { centerX: 0, minY: 0, sizeX: 2, sizeY: 2 };
    expect(interactionZoneForHit({ x: 0, y: 1.8 }, frame)).toBe("head");
    expect(interactionZoneForHit({ x: 0.7, y: 1.1 }, frame)).toBe("leftArm");
    expect(interactionZoneForHit({ x: 0, y: 0.7 }, frame)).toBe("waist");
    expect(interactionZoneForHit({ x: -0.3, y: 0.2 }, frame)).toBe("rightLeg");
  });

  it("keeps click feedback small and bounded", () => {
    expect(interactionExpressionTargets("head", 2)).toEqual({
      happy: 0.32,
      relaxed: 0.12,
      surprised: 0.12,
    });
    expect(interactionExpressionTargets("body", 0)).toEqual({
      happy: 0,
      angry: 0,
      surprised: 0,
    });
    expect(interactionExpressionTargets("waist", 1)).toEqual({ angry: 0.16, surprised: 0.12 });
  });

  it("returns a stable smoothing factor", () => {
    expect(smoothFactor(0, 8)).toBe(0);
    expect(smoothFactor(1, 8)).toBeGreaterThan(0.99);
    expect(smoothFactor(0.016, 8)).toBeGreaterThan(0);
  });

  it("derives conversation state by active priority", () => {
    expect(conversationStateFor({ recording: true, speaking: true, thinking: true })).toBe("listening");
    expect(conversationStateFor({ speaking: true, thinking: true })).toBe("talking");
    expect(conversationStateFor({ thinking: true })).toBe("thinking");
    expect(conversationStateFor({ finished: true })).toBe("finished");
    expect(conversationStateFor({})).toBe("idle");
  });

  it("uses phased action envelopes and optional blink pulses", () => {
    expect(actionPhase(0.1)).toBe("enter");
    expect(actionPhase(0.4)).toBe("active");
    expect(actionPhase(0.75)).toBe("exit");
    expect(actionPhase(0.95)).toBe("recover");
    expect(actionWeight(0)).toBe(0);
    expect(actionWeight(0.4)).toBe(1);
    expect(actionWeight(1)).toBe(0);
    expect(actionBlinkWeight("idle_double_blink", 0.3)).toBe(1);
  });

  it("raises, waves, and lowers greeting motion smoothly", () => {
    const start = greetingPose(0);
    const active = greetingPose(0.4);
    const end = greetingPose(1);

    expect(start.raise).toBe(0);
    expect(start.shoulderZ).toBe(0);
    expect(start.lowerArmZ).toBe(0);
    expect(start.wristZ).toBe(0);
    expect(active.raise).toBe(1);
    expect(active.shoulderZ).toBeCloseTo(0.05);
    expect(active.upperArmX).toBe(0);
    expect(active.upperArmY).toBe(0);
    expect(active.upperArmZ).toBeCloseTo(0.2);
    expect(active.lowerArmX).toBe(0);
    expect(active.lowerArmZ).toBeCloseTo(-1.9);
    expect(active.wristX).toBeCloseTo(-Math.PI / 2);
    expect(active.wristY).toBe(0);
    expect(active.wristZ).toBeGreaterThan(-0.1);
    expect(active.wristZ).toBeLessThan(0.25);
    expect(Math.abs(greetingPose(0.45).wave)).toBeGreaterThan(0.1);
    expect(end.raise).toBe(0);
    expect(end.lowerArmZ).toBe(0);
    expect(Math.abs(end.wave)).toBeLessThan(0.000001);
  });

  it("keeps the normalized right-hand greeting facing the viewer", () => {
    const pose = greetingPose(0.4);
    const rotation = (x, y, z) => new Quaternion().setFromEuler(new Euler(x, y, z, "YXZ"));
    const shoulder = rotation(0, 0, pose.shoulderZ);
    const upperArm = shoulder.clone().multiply(rotation(pose.upperArmX, pose.upperArmY, pose.upperArmZ));
    const lowerArm = upperArm.clone().multiply(rotation(pose.lowerArmX, 0, pose.lowerArmZ));
    const hand = lowerArm.clone().multiply(rotation(pose.wristX, pose.wristY, pose.wristZ));
    const upperDirection = new Vector3(-1, 0, 0).applyQuaternion(upperArm);
    const forearmDirection = new Vector3(-1, 0, 0).applyQuaternion(lowerArm);
    const fingerDirection = new Vector3(-1, 0, 0).applyQuaternion(hand);
    const palmNormal = new Vector3(0, -1, 0).applyQuaternion(hand);

    expect(Math.abs(upperDirection.z)).toBeLessThan(0.05);
    expect(forearmDirection.y).toBeGreaterThan(0.95);
    expect(fingerDirection.y).toBeGreaterThan(0.95);
    expect(palmNormal.z).toBeGreaterThan(0.99);
    expect(Math.abs(palmNormal.x)).toBeLessThan(0.05);
    expect(Math.abs(palmNormal.y)).toBeLessThan(0.2);
  });

  it("clamps every pose layer to safe bone rotations", () => {
    expect(clampPoseRotation("head", 1, -1, 1)).toEqual({ x: 0.35, y: -0.45, z: 0.3 });
    expect(clampPoseRotation("leftUpperArm", 0, 0, -2)).toEqual({ x: 0, y: 0, z: -1.7 });
    expect(clampPoseRotation("leftLowerArm", -2, 0, 3)).toEqual({ x: -1.4, y: 0, z: 2 });
    expect(clampPoseRotation("rightHand", Math.PI, 0, 0)).toEqual({ x: 1.7, y: 0, z: 0 });
    expect(clampPoseRotation("missing", 1, 2, 3)).toEqual({ x: 1, y: 2, z: 3 });
  });

  it("lets pointer interaction interrupt idle and keeps one pending action", () => {
    const scheduler = new AvatarActionScheduler({ random: () => 0 });
    expect(scheduler.tick(5, { allowIdle: true }).action).toBe("idle_look");

    scheduler.request({ type: "pointer_click", zone: "head" });
    expect(scheduler.current.action).toBe("head_nod");
    scheduler.request({ type: "pointer_click", zone: "leftArm" });
    scheduler.request({ type: "pointer_click", zone: "waist" });
    expect(scheduler.pending.action).toBe("left_arm_lift");
  });

  it("avoids repeating the same click action and respects cooldowns", () => {
    const scheduler = new AvatarActionScheduler({ random: () => 0 });
    expect(scheduler.request({ type: "pointer_click", zone: "head" }).action).toBe("head_nod");
    scheduler.tick(1, { allowIdle: false });
    expect(scheduler.request({ type: "pointer_click", zone: "head" }).action).toBe("head_tilt");

    const greeting = new AvatarActionScheduler({ random: () => 0 });
    expect(greeting.request({ type: "greeting" }).action).toBe("greeting_wave");
    expect(greeting.request({ type: "focus_return" })).toBeNull();
    expect(greeting.pending).toBeNull();
    greeting.tick(2.5, { allowIdle: false });
    expect(greeting.request({ type: "greeting" })).toBeNull();
  });

  it("clears active, pending, cooldown, and history state on reset", () => {
    const scheduler = new AvatarActionScheduler({ random: () => 0 });
    scheduler.request({ type: "pointer_click", zone: "head" });
    scheduler.request({ type: "pointer_click", zone: "leftArm" });
    scheduler.reset();

    expect(scheduler.current).toBeNull();
    expect(scheduler.pending).toBeNull();
    expect(scheduler.cooldowns.size).toBe(0);
    expect(scheduler.lastByIntent.size).toBe(0);
  });
});
