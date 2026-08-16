export function clamp(value, min = -1, max = 1) {
  return Math.min(max, Math.max(min, value));
}

export function normalizePointer(rect, clientX, clientY) {
  if (!rect || rect.width <= 0 || rect.height <= 0) {
    return { x: 0, y: 0 };
  }

  return {
    x: clamp(((clientX - rect.left) / rect.width) * 2 - 1),
    y: clamp(1 - ((clientY - rect.top) / rect.height) * 2),
  };
}

export function clickZoneForPointer({ x = 0, y = 0 } = {}) {
  if (y >= 0.35) return "head";
  if (y >= -0.18 && Math.abs(x) >= 0.42) return x > 0 ? "leftArm" : "rightArm";
  if (y >= -0.18) return "body";
  if (y >= -0.48) return "waist";
  return x > 0 ? "leftLeg" : "rightLeg";
}

export function interactionZoneForHit(point, frame) {
  if (!point || !frame || frame.sizeY <= 0 || frame.sizeX <= 0) return null;
  const heightRatio = clamp((point.y - frame.minY) / frame.sizeY, 0, 1);
  const horizontal = (point.x - frame.centerX) / frame.sizeX;
  if (heightRatio >= 0.68) return "head";
  if (heightRatio >= 0.42 && Math.abs(horizontal) >= 0.2) {
    return horizontal > 0 ? "leftArm" : "rightArm";
  }
  if (heightRatio >= 0.46) return "body";
  if (heightRatio >= 0.28) return "waist";
  return horizontal > 0 ? "leftLeg" : "rightLeg";
}

export function interactionExpressionTargets(zone, strength) {
  const amount = clamp(strength, 0, 1);
  if (zone === "head") {
    return { happy: amount * 0.32, relaxed: amount * 0.12, surprised: amount * 0.12 };
  }
  if (zone === "body") {
    return { happy: amount * 0.16, angry: amount * 0.08, surprised: amount * 0.16 };
  }
  if (zone === "waist") {
    return { angry: amount * 0.16, surprised: amount * 0.12 };
  }
  return { angry: amount * 0.1, surprised: amount * 0.1 };
}

export function smoothFactor(delta, speed) {
  return 1 - Math.exp(-Math.max(delta, 0) * speed);
}

export const ACTION_PRIORITY = Object.freeze({
  idle: 10,
  emotion: 20,
  conversation: 30,
  interaction: 40,
});

const POSE_LIMITS = Object.freeze({
  head: { x: 0.35, y: 0.45, z: 0.3 },
  body: { x: 0.25, y: 0.25, z: 0.25 },
  hips: { x: 0.22, y: 0.25, z: 0.2 },
  leftShoulder: { x: 0.3, y: 0.3, z: 0.35 },
  rightShoulder: { x: 0.3, y: 0.3, z: 0.35 },
  leftUpperArm: { x: 0.55, y: 0.65, z: 1.7 },
  rightUpperArm: { x: 0.55, y: 0.65, z: 1.7 },
  leftLowerArm: { x: 1.4, y: 0.8, z: 2.0 },
  rightLowerArm: { x: 1.4, y: 0.8, z: 2.0 },
  leftHand: { x: 0.7, y: 0.65, z: 1.8 },
  rightHand: { x: 1.7, y: 0.65, z: 1.8 },
  leftUpperLeg: { x: 0.45, y: 0.25, z: 0.25 },
  rightUpperLeg: { x: 0.45, y: 0.25, z: 0.25 },
  leftLowerLeg: { x: 0.55, y: 0.2, z: 0.2 },
  rightLowerLeg: { x: 0.55, y: 0.2, z: 0.2 },
});

export function clampPoseRotation(key, x = 0, y = 0, z = 0) {
  const limits = POSE_LIMITS[key] || { x: Math.PI, y: Math.PI, z: Math.PI };
  return {
    x: clamp(x, -limits.x, limits.x),
    y: clamp(y, -limits.y, limits.y),
    z: clamp(z, -limits.z, limits.z),
  };
}

const ACTIONS = Object.freeze({
  idle_look: { priority: 10, duration: 1.8, cooldown: 4.5, interruptible: true },
  idle_tilt: { priority: 10, duration: 2.1, cooldown: 5.5, interruptible: true },
  idle_weight: { priority: 10, duration: 2.4, cooldown: 5.0, interruptible: true },
  idle_arm: { priority: 10, duration: 2.0, cooldown: 6.0, interruptible: true },
  idle_double_blink: { priority: 10, duration: 1.1, cooldown: 8.0, interruptible: true },
  head_nod: { priority: 40, duration: 1.2, cooldown: 1.4, interruptible: true },
  head_tilt: { priority: 40, duration: 1.5, cooldown: 1.8, interruptible: true },
  head_blink_smile: { priority: 40, duration: 1.4, cooldown: 2.0, interruptible: true },
  left_arm_lift: { priority: 40, duration: 1.55, cooldown: 1.8, interruptible: true },
  left_arm_dodge: { priority: 40, duration: 1.3, cooldown: 1.5, interruptible: true },
  right_arm_lift: { priority: 40, duration: 1.55, cooldown: 1.8, interruptible: true },
  right_arm_dodge: { priority: 40, duration: 1.3, cooldown: 1.5, interruptible: true },
  body_lean: { priority: 40, duration: 1.45, cooldown: 1.6, interruptible: true },
  body_sway: { priority: 40, duration: 1.55, cooldown: 1.8, interruptible: true },
  waist_twist: { priority: 40, duration: 1.5, cooldown: 1.7, interruptible: true },
  waist_recoil: { priority: 40, duration: 1.3, cooldown: 1.5, interruptible: true },
  left_leg_shift: { priority: 40, duration: 1.5, cooldown: 1.8, interruptible: true },
  left_leg_recoil: { priority: 40, duration: 1.25, cooldown: 1.5, interruptible: true },
  right_leg_shift: { priority: 40, duration: 1.5, cooldown: 1.8, interruptible: true },
  right_leg_recoil: { priority: 40, duration: 1.25, cooldown: 1.5, interruptible: true },
  greeting_wave: { priority: 40, duration: 3.2, cooldown: 20.0, interruptible: true },
});

const IDLE_ACTIONS = ["idle_look", "idle_tilt", "idle_weight", "idle_arm", "idle_double_blink"];
const CLICK_ACTIONS = Object.freeze({
  head: ["head_nod", "head_tilt", "head_blink_smile"],
  leftArm: ["left_arm_lift", "left_arm_dodge"],
  rightArm: ["right_arm_lift", "right_arm_dodge"],
  body: ["body_lean", "body_sway"],
  waist: ["waist_twist", "waist_recoil"],
  leftLeg: ["left_leg_shift", "left_leg_recoil"],
  rightLeg: ["right_leg_shift", "right_leg_recoil"],
});

export function conversationStateFor({ recording, speaking, thinking, finished = false } = {}) {
  if (recording) return "listening";
  if (speaking) return "talking";
  if (thinking) return "thinking";
  if (finished) return "finished";
  return "idle";
}

export function actionPhase(progress) {
  if (progress < 0.18) return "enter";
  if (progress < 0.62) return "active";
  if (progress < 0.84) return "exit";
  return "recover";
}

function easeInOut(value) {
  const t = clamp(value, 0, 1);
  return t * t * (3 - 2 * t);
}

export function actionWeight(progress) {
  const value = clamp(progress, 0, 1);
  if (value < 0.2) return easeInOut(value / 0.2);
  if (value < 0.62) return 1;
  return easeInOut((1 - value) / 0.38);
}

export function greetingPose(progress) {
  const value = clamp(progress, 0, 1);
  const raise = value < 0.24
    ? easeInOut(value / 0.24)
    : value > 0.8
      ? easeInOut((1 - value) / 0.2)
      : 1;
  const waveIn = easeInOut(clamp((value - 0.22) / 0.12, 0, 1));
  const waveOut = easeInOut(clamp((0.82 - value) / 0.12, 0, 1));
  const waveEnvelope = waveIn * waveOut;
  const wave = Math.sin((value - 0.22) * Math.PI * 5) * waveEnvelope;
  const nodProgress = clamp((value - 0.08) / 0.5, 0, 1);
  const nod = Math.sin(nodProgress * Math.PI) * raise;
  const poseValue = (amount) => (raise === 0 ? 0 : amount * raise);
  return {
    raise,
    wave,
    nod,
    shoulderZ: poseValue(0.05),
    upperArmX: -0.035 + poseValue(0.035),
    upperArmY: 0,
    upperArmZ: 1.18 + poseValue(-0.98),
    lowerArmX: -0.08 + poseValue(0.08),
    lowerArmZ: poseValue(-1.9),
    wristX: poseValue(-Math.PI / 2),
    wristY: 0,
    wristZ: poseValue(0.08) + 0.16 * wave,
  };
}

function blinkPulse(progress, center, width = 0.08) {
  const distance = Math.abs(progress - center);
  return distance >= width ? 0 : 1 - distance / width;
}

export function actionBlinkWeight(action, progress) {
  if (action === "idle_double_blink") {
    return Math.max(blinkPulse(progress, 0.3), blinkPulse(progress, 0.62));
  }
  if (action === "head_blink_smile") return blinkPulse(progress, 0.35, 0.1);
  return 0;
}

export function actionExpressionTargets(action, strength) {
  const amount = clamp(strength, 0, 1);
  if (action === "head_blink_smile" || action === "greeting_wave") {
    return { happy: amount * 0.34, relaxed: amount * 0.14 };
  }
  if (action?.includes("dodge") || action?.includes("recoil")) {
    return { angry: amount * 0.12 };
  }
  return {};
}

function chooseCandidate(candidates, lastAction, cooldowns, time, random) {
  const available = candidates.filter((action) => (cooldowns.get(action) || 0) <= time);
  const pool = available.filter((action) => action !== lastAction);
  const choices = pool.length > 0 ? pool : available;
  if (choices.length === 0) return null;
  return choices[Math.min(choices.length - 1, Math.floor(random() * choices.length))];
}

export class AvatarActionScheduler {
  constructor({ random = Math.random } = {}) {
    this.random = random;
    this.time = 0;
    this.current = null;
    this.pending = null;
    this.cooldowns = new Map();
    this.lastByIntent = new Map();
    this.nextIdleAt = 2.5 + this.random() * 2.5;
  }

  request(intent) {
    const action = this.selectAction(intent);
    if (!action) return null;
    if (this.current?.action === action || this.pending?.action === action) return null;
    const definition = ACTIONS[action];
    const requested = {
      action,
      priority: intent.priority ?? definition.priority,
      startedAt: null,
      duration: definition.duration,
      cooldown: definition.cooldown,
      interruptible: definition.interruptible,
      phase: "enter",
      progress: 0,
      intent: intent.type,
      zone: intent.zone || null,
      variant: { x: this.random() * 2 - 1, y: this.random() * 2 - 1 },
    };

    if (!this.current) return this.start(requested);
    if (requested.priority > this.current.priority && this.current.interruptible) {
      this.finishCurrent();
      return this.start(requested);
    }
    if (!this.pending || requested.priority > this.pending.priority) {
      this.pending = requested;
      return requested;
    }
    return null;
  }

  selectAction(intent) {
    let candidates;
    let key;
    if (intent.type === "idle") {
      candidates = IDLE_ACTIONS;
      key = "idle";
    } else if (intent.type === "greeting" || intent.type === "focus_return") {
      candidates = ["greeting_wave"];
      key = "greeting";
    } else if (intent.type === "behavior_action") {
      candidates = [intent.action];
      key = `behavior:${intent.action}`;
    } else if (intent.type === "motion") {
      candidates = [intent.action];
      key = `motion:${intent.motion || intent.action}`;
    } else if (intent.type === "pointer_click") {
      candidates = CLICK_ACTIONS[intent.zone] || CLICK_ACTIONS.body;
      key = `click:${intent.zone || "body"}`;
    } else {
      return null;
    }

    const action = chooseCandidate(
      candidates,
      this.lastByIntent.get(key),
      this.cooldowns,
      this.time,
      this.random,
    );
    if (action) this.lastByIntent.set(key, action);
    return action;
  }

  start(action) {
    action.startedAt = this.time;
    this.current = action;
    return action;
  }

  finishCurrent() {
    if (!this.current) return;
    this.cooldowns.set(this.current.action, this.time + this.current.cooldown);
    if (this.current.intent === "idle") this.scheduleNextIdle();
    this.current = null;
  }

  scheduleNextIdle() {
    this.nextIdleAt = this.time + 3.2 + this.random() * 4.3;
  }

  tick(delta, { allowIdle = true } = {}) {
    this.time += Math.max(0, delta);
    if (this.current) {
      const elapsed = this.time - this.current.startedAt;
      if (elapsed >= this.current.duration) this.finishCurrent();
    }
    if (!this.current && this.pending) {
      const pending = this.pending;
      this.pending = null;
      this.start(pending);
    }
    if (!this.current && allowIdle && this.time >= this.nextIdleAt) {
      const action = this.selectAction({ type: "idle" });
      if (action) {
        const definition = ACTIONS[action];
        this.start({
          action,
          priority: definition.priority,
          startedAt: this.time,
          duration: definition.duration,
          cooldown: definition.cooldown,
          interruptible: definition.interruptible,
          phase: "enter",
          progress: 0,
          intent: "idle",
          zone: null,
          variant: { x: this.random() * 2 - 1, y: this.random() * 2 - 1 },
        });
      } else {
        this.scheduleNextIdle();
      }
    }
    if (!this.current) return null;
    const progress = clamp((this.time - this.current.startedAt) / this.current.duration, 0, 1);
    this.current.progress = progress;
    this.current.phase = actionPhase(progress);
    return { ...this.current, weight: actionWeight(progress) };
  }

  reset() {
    this.current = null;
    this.pending = null;
    this.cooldowns.clear();
    this.lastByIntent.clear();
    this.time = 0;
    this.nextIdleAt = 2.5 + this.random() * 2.5;
  }

  cancelIdle() {
    if (this.current?.intent === "idle") this.finishCurrent();
    if (this.pending?.intent === "idle") this.pending = null;
  }
}
