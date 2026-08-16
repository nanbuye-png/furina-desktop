import { resolveMotion, MOTION_LIBRARY } from "./avatarMotion.js";
export const AVATAR_MODES = Object.freeze({
  idle: "idle",
  listening: "listening",
  thinking: "thinking",
  talking: "talking",
});

const MODE_PRIORITY = Object.freeze({
  idle: 0,
  listening: 10,
  thinking: 20,
  talking: 30,
});

const TRANSIENT_BEHAVIORS = Object.freeze({
  acknowledge: { action: "head_nod", cooldown: 0.45, immediate: true },
  greet: { action: "greeting_wave", cooldown: 20, immediate: false },
  farewell: { action: "body_lean", cooldown: 1.6, immediate: false },
  happy: { action: "head_blink_smile", cooldown: 2, immediate: false },
  surprised: { action: "head_nod", cooldown: 1.2, immediate: false },
  annoyed: { action: "right_arm_dodge", cooldown: 1.5, immediate: false },
});

const MOOD_REACTIONS = Object.freeze({
  happy: "happy",
  proud: "happy",
  sad: "surprised",
  hurt: "surprised",
  annoyed: "annoyed",
});

function nowSeconds() {
  return Date.now() / 1000;
}

export class AvatarBehaviorController {
  constructor({ requestAction = () => null, onModeChange = () => {}, now = nowSeconds } = {}) {
    this.requestAction = requestAction;
    this.onModeChange = onModeChange;
    this.now = now;
    this.mode = AVATAR_MODES.idle;
    this.activeModes = new Set();
    this.pending = null;
    this.cooldowns = new Map();
    this.lastMood = null;
  }

  getSnapshot() {
    return {
      mode: this.mode,
      activeModes: new Set(this.activeModes),
      pending: this.pending?.motion || this.pending?.behavior || null,
      cooldowns: new Map(this.cooldowns),
    };
  }

  setMode(mode, active = true) {
    if (!Object.prototype.hasOwnProperty.call(MODE_PRIORITY, mode) || mode === AVATAR_MODES.idle) return false;
    const wasActive = this.activeModes.has(mode);
    if (active === wasActive) return false;
    if (active) this.activeModes.add(mode);
    else this.activeModes.delete(mode);

    const nextMode = [...this.activeModes].sort(
      (left, right) => MODE_PRIORITY[right] - MODE_PRIORITY[left],
    )[0] || AVATAR_MODES.idle;
    if (nextMode !== this.mode) {
      this.mode = nextMode;
      this.onModeChange(this.mode);
      if (this.mode === AVATAR_MODES.idle) this.flushPending();
    }
    return true;
  }

  listen(active = true) {
    return this.setMode(AVATAR_MODES.listening, active);
  }

  think(active = true) {
    return this.setMode(AVATAR_MODES.thinking, active);
  }

  talk(active = true) {
    return this.setMode(AVATAR_MODES.talking, active);
  }

  acknowledge() {
    return this.trigger("acknowledge");
  }

  greet() {
    return this.trigger("greet");
  }

  farewell() {
    return this.trigger("farewell");
  }

  react(kind) {
    if (!Object.prototype.hasOwnProperty.call(TRANSIENT_BEHAVIORS, kind)) return null;
    return this.trigger(kind);
  }

  motion(name) {
    const definition = resolveMotion(name);
    if (!definition) return null;
    const currentTime = this.now();
    const request = {
      behavior: `motion:${definition.name}`,
      motion: definition.name,
      action: definition.action,
      priority: definition.priority,
    };
    if ((this.cooldowns.get(request.behavior) || 0) > currentTime) return null;
    if (this.mode !== AVATAR_MODES.idle && !definition.immediate) {
      this.pending = request;
      return { accepted: true, queued: true, behavior: definition.name };
    }
    return this.execute(request, definition, currentTime);
  }

  observeMood(mood) {
    if (!mood || mood === this.lastMood) return null;
    const previous = this.lastMood;
    this.lastMood = mood;
    if (!previous || this.mode !== AVATAR_MODES.idle || this.pending) return null;
    const reaction = MOOD_REACTIONS[mood];
    return reaction ? this.react(reaction) : null;
  }

  trigger(behavior) {
    const definition = TRANSIENT_BEHAVIORS[behavior];
    if (!definition) return null;
    const currentTime = this.now();
    if ((this.cooldowns.get(behavior) || 0) > currentTime) return null;
    const request = { behavior, action: definition.action };
    if (this.mode !== AVATAR_MODES.idle && !definition.immediate) {
      this.pending = request;
      return { accepted: true, queued: true, behavior };
    }
    return this.execute(request, definition, currentTime);
  }

  execute(request, definition, currentTime = this.now()) {
    const result = this.requestAction({
      type: request.motion ? "motion" : "behavior_action",
      motion: request.motion || null,
      action: request.action,
      priority: request.priority || 50,
    });
    if (result?.deferred) {
      this.pending = request;
      return {
        accepted: true,
        queued: true,
        behavior: request.behavior,
      };
    }
    if (!result) return null;
    this.cooldowns.set(request.behavior, currentTime + definition.cooldown);
    return {
      accepted: true,
      queued: false,
      behavior: request.behavior,
      action: result.action || request.action,
    };
  }

  flushPending() {
    if (this.mode !== AVATAR_MODES.idle || !this.pending) return null;
    const request = this.pending;
    this.pending = null;
    const definition = TRANSIENT_BEHAVIORS[request.behavior] || (request.motion ? MOTION_LIBRARY[request.motion] : null);
    const result = this.execute(request, definition);
    if (!result) this.pending = request;
    return result;
  }

  reset() {
    this.activeModes.clear();
    this.pending = null;
    this.cooldowns.clear();
    this.lastMood = null;
    this.mode = AVATAR_MODES.idle;
    this.onModeChange(this.mode);
  }
}

export function moodReactionFor(mood) {
  return MOOD_REACTIONS[mood] || null;
}
