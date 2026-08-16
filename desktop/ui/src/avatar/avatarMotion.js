export const MOTION_LIBRARY = Object.freeze({
  wave: Object.freeze({ action: "greeting_wave", cooldown: 20, immediate: false, priority: 50 }),
  nod: Object.freeze({ action: "head_nod", cooldown: 1.4, immediate: true, priority: 50 }),
  smile: Object.freeze({ action: "head_blink_smile", cooldown: 2, immediate: false, priority: 50 }),
  tilt: Object.freeze({ action: "head_tilt", cooldown: 1.8, immediate: false, priority: 50 }),
  sway: Object.freeze({ action: "body_sway", cooldown: 1.8, immediate: false, priority: 50 }),
  lean: Object.freeze({ action: "body_lean", cooldown: 1.6, immediate: false, priority: 50 }),
  leftArm: Object.freeze({ action: "left_arm_lift", cooldown: 1.8, immediate: false, priority: 50 }),
  rightArm: Object.freeze({ action: "right_arm_lift", cooldown: 1.8, immediate: false, priority: 50 }),
  twist: Object.freeze({ action: "waist_twist", cooldown: 1.7, immediate: false, priority: 50 }),
  recoil: Object.freeze({ action: "waist_recoil", cooldown: 1.5, immediate: false, priority: 50 }),
});

export function resolveMotion(name) {
  if (typeof name !== "string") return null;
  const normalized = name.trim();
  const definition = MOTION_LIBRARY[normalized];
  return definition ? { name: normalized, ...definition } : null;
}
