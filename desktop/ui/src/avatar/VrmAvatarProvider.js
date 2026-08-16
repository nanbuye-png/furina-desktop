import * as THREE from "three";
import { GLTFLoader } from "three/examples/jsm/loaders/GLTFLoader.js";
import { VRMLoaderPlugin } from "@pixiv/three-vrm";
import { fixHairHighlightOverlay } from "./hairMaterialFix.js";
import {
  AvatarActionScheduler,
  ACTION_PRIORITY,
  actionBlinkWeight,
  actionExpressionTargets,
  clampPoseRotation,
  greetingPose,
  interactionZoneForHit,
  smoothFactor,
} from "./avatarInteraction.js";

const MOOD_EXPRESSIONS = {
  calm: { relaxed: 0.18 },
  happy: { happy: 0.85 },
  proud: { happy: 0.42, relaxed: 0.3 },
  hurt: { sad: 0.68 },
  sad: { sad: 0.85 },
  annoyed: { angry: 0.78 },
};

function disposeMaterial(material) {
  for (const value of Object.values(material)) {
    if (value?.isTexture) value.dispose();
  }
  material.dispose();
}

export class VrmAvatarProvider {
  constructor(host) {
    this.host = host;
    this.scene = new THREE.Scene();
    this.camera = new THREE.PerspectiveCamera(32, 1, 0.01, 100);
    this.clock = new THREE.Clock();
    this.vrm = null;
    this.frameId = null;
    this.disposed = false;
    this.state = { mood: "calm", intensity: 0, speaking: false };
    this.context = {
      conversationState: "idle",
      mood: "calm",
      intensity: 0,
      valence: 0.5,
      arousal: 0.5,
      speaking: false,
      audioLevel: 0,
      recording: false,
      thinking: false,
      blocked: false,
    };
    this.behaviorMode = "idle";
    this.finishedRemaining = 0;
    this.scheduler = new AvatarActionScheduler();
    this.actionFrame = null;
    this.expressionWeights = new Map();
    this.blinkElapsed = 0;
    this.nextBlinkAt = 2.4;
    this.mouthElapsed = 0;
    this.idleElapsed = 0;
    this.avatarFrame = null;
    this.lookTarget = new THREE.Object3D();
    this.raycaster = new THREE.Raycaster();
    this.rayPointer = new THREE.Vector2();
    this.interaction = {
      pointer: new THREE.Vector2(),
      targetPointer: new THREE.Vector2(),
      pointerActive: false,
      focused: true,
    };
    this.poseBones = new Map();
    this.scene.add(this.lookTarget);

    this.renderer = new THREE.WebGLRenderer({ alpha: true, antialias: true });
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.renderer.setClearColor(0x000000, 0);
    this.renderer.domElement.className = "avatar-canvas";
    host.appendChild(this.renderer.domElement);

    const ambient = new THREE.AmbientLight(0xffffff, 1.7);
    const key = new THREE.DirectionalLight(0xffffff, 2.2);
    key.position.set(1.2, 2.4, 2.8);
    const fill = new THREE.DirectionalLight(0xa8e6ff, 0.8);
    fill.position.set(-2, 1.2, 1.4);
    this.scene.add(ambient, key, fill);

    this.resizeObserver = new ResizeObserver(() => this.resize());
    this.resizeObserver.observe(host);
    this.resize();
    this.animate();
  }

  async load(arrayBuffer) {
    const loader = new GLTFLoader();
    loader.register((parser) => new VRMLoaderPlugin(parser));
    const gltf = await new Promise((resolve, reject) => {
      loader.parse(arrayBuffer, "", resolve, reject);
    });
    if (this.disposed) {
      gltf.scene?.traverse((object) => {
        object.geometry?.dispose();
        if (Array.isArray(object.material)) object.material.forEach(disposeMaterial);
        else if (object.material) disposeMaterial(object.material);
      });
      return;
    }

    const vrm = gltf.userData.vrm;
    if (!vrm) throw new Error("文件中没有可用的 VRM 数据");
    if (this.vrm) this.removeCurrentVrm();
    this.vrm = vrm;
    fixHairHighlightOverlay(vrm.scene);
    this.scene.add(vrm.scene);
    this.configureLookAt();
    this.frameAvatar();
    this.applyState(0, true);
  }

  setState(nextState) {
    this.setInteractionContext(nextState);
  }

  setAudioLevel(level) {
    this.context.audioLevel = THREE.MathUtils.clamp(Number(level) || 0, 0, 1);
  }

  setInteractionContext(nextContext) {
    const previousConversation = this.getBaseConversationState();
    this.context = { ...this.context, ...nextContext };
    this.state = {
      mood: this.context.mood,
      intensity: this.context.intensity,
      speaking: this.context.speaking,
    };
    const conversationState = this.getBaseConversationState();
    if (conversationState !== "idle" && conversationState !== "finished") {
      this.finishedRemaining = 0;
    }
    if (
      conversationState === "idle"
      && previousConversation !== "idle"
      && previousConversation !== "finished"
    ) {
      this.finishedRemaining = 0.9;
    }
  }

  setBehaviorMode(mode = "idle") {
    this.behaviorMode = mode || "idle";
    if (this.behaviorMode !== "idle") this.finishedRemaining = 0;
  }

  getConversationState() {
    if (this.finishedRemaining > 0) return "finished";
    return this.getBaseConversationState();
  }

  getBaseConversationState() {
    const behaviorMode = this.behaviorMode || "idle";
    return behaviorMode === "idle" ? this.context.conversationState : behaviorMode;
  }

  setPointer(x, y, active = true) {
    this.interaction.targetPointer.set(x, y);
    this.interaction.pointerActive = active;
  }

  clearPointer() {
    this.interaction.pointerActive = false;
    this.interaction.targetPointer.set(0, 0);
  }

  setFocused(focused) {
    this.interaction.focused = focused;
    if (!focused) this.clearPointer();
  }

  triggerClick(zone = "body") {
    return this.handleIntent({ type: "pointer_click", zone });
  }

  handleIntent(intent) {
    const isBehaviorGreeting = intent.type === "behavior_action" && intent.action === "greeting_wave";
    const isGreeting = intent.type === "greeting" || intent.type === "focus_return" || isBehaviorGreeting;
    if (
      isGreeting
      && (!this.interaction.focused
        || this.context.blocked
        || this.context.recording
        || this.context.thinking
        || this.context.speaking)
    ) {
      return isBehaviorGreeting ? { deferred: true } : null;
    }
    if (
      isGreeting
      && (this.scheduler.current?.priority >= ACTION_PRIORITY.interaction
        || this.scheduler.pending?.priority >= ACTION_PRIORITY.interaction)
    ) {
      return isBehaviorGreeting ? { deferred: true } : null;
    }
    return this.scheduler.request(intent);
  }

  triggerClickAt(x, y) {
    if (!this.vrm || !this.avatarFrame) return null;
    this.rayPointer.set(x, y);
    this.raycaster.setFromCamera(this.rayPointer, this.camera);
    const hit = this.raycaster.intersectObject(this.vrm.scene, true)[0];
    if (!hit) return null;
    const zone = interactionZoneForHit(hit.point, this.avatarFrame);
    if (!zone) return null;
    this.triggerClick(zone);
    return zone;
  }

  configureLookAt() {
    this.lookTarget.position.copy(this.vrm.scene.position);
    if (this.vrm.lookAt) {
      this.vrm.lookAt.autoUpdate = false;
      this.vrm.lookAt.target = null;
      this.vrm.lookAt.reset();
    }
    this.poseBones.clear();
    this.scheduler.reset();
    this.actionFrame = null;
    this.behaviorMode = "idle";
    this.registerPoseBone("head", ["head"]);
    this.registerPoseBone("body", ["upperChest", "chest", "spine"]);
    this.registerPoseBone("hips", ["hips"]);
    this.registerPoseBone("leftShoulder", ["leftShoulder"]);
    this.registerPoseBone("rightShoulder", ["rightShoulder"]);
    this.registerPoseBone("leftUpperArm", ["leftUpperArm"]);
    this.registerPoseBone("rightUpperArm", ["rightUpperArm"]);
    this.registerPoseBone("leftLowerArm", ["leftLowerArm"]);
    this.registerPoseBone("rightLowerArm", ["rightLowerArm"]);
    this.registerPoseBone("leftHand", ["leftHand"]);
    this.registerPoseBone("rightHand", ["rightHand"]);
    this.registerPoseBone("leftUpperLeg", ["leftUpperLeg"]);
    this.registerPoseBone("rightUpperLeg", ["rightUpperLeg"]);
    this.registerPoseBone("leftLowerLeg", ["leftLowerLeg"]);
    this.registerPoseBone("rightLowerLeg", ["rightLowerLeg"]);
  }

  registerPoseBone(key, names) {
    const node = names
      .map((name) => this.vrm.humanoid?.getNormalizedBoneNode(name))
      .find(Boolean);
    if (!node) return;
    this.poseBones.set(key, {
      node,
      rest: node.quaternion.clone(),
      target: new THREE.Quaternion(),
      offset: new THREE.Quaternion(),
      euler: new THREE.Euler(0, 0, 0, "YXZ"),
    });
  }

  applyPoseBone(key, x, y, z, blend, immediate = false) {
    const bone = this.poseBones.get(key);
    if (!bone) return;
    const rotation = clampPoseRotation(key, x, y, z);
    bone.euler.set(rotation.x, rotation.y, rotation.z, "YXZ");
    bone.offset.setFromEuler(bone.euler);
    bone.target.copy(bone.rest).multiply(bone.offset);
    bone.node.quaternion.slerp(bone.target, immediate ? 1 : blend);
  }

  frameAvatar() {
    if (!this.vrm) return;
    const bounds = new THREE.Box3().setFromObject(this.vrm.scene);
    const size = bounds.getSize(new THREE.Vector3());
    const center = bounds.getCenter(new THREE.Vector3());
    const visibleHeight = Math.max(size.y * 0.58, 0.8);
    const distance = visibleHeight / (2 * Math.tan(THREE.MathUtils.degToRad(this.camera.fov * 0.5)));
    const targetY = bounds.min.y + size.y * 0.7;
    this.camera.position.set(center.x, targetY + size.y * 0.015, center.z + distance * 1.08);
    this.camera.near = Math.max(distance / 100, 0.01);
    this.camera.far = distance * 10 + size.z;
    this.camera.lookAt(center.x, targetY, center.z);
    this.camera.updateProjectionMatrix();
    this.avatarFrame = {
      center,
      size,
      centerX: center.x,
      sizeX: size.x,
      sizeY: size.y,
      targetY,
      minY: bounds.min.y,
    };
    this.updateLookTarget(0, true);
  }

  resize() {
    const width = Math.max(this.host.clientWidth, 1);
    const height = Math.max(this.host.clientHeight, 1);
    this.renderer.setSize(width, height, false);
    this.camera.aspect = width / height;
    this.camera.updateProjectionMatrix();
  }

  applyExpression(name, target, delta, immediate = false) {
    const manager = this.vrm?.expressionManager;
    if (!manager || manager.getValue(name) === null) return;
    const previous = this.expressionWeights.get(name) || 0;
    const blend = immediate ? 1 : 1 - Math.exp(-delta * 7.5);
    const value = THREE.MathUtils.lerp(previous, target, blend);
    this.expressionWeights.set(name, value);
    manager.setValue(name, value);
  }

  applyState(delta, immediate = false) {
    if (!this.vrm) return;
    const moodTargets = MOOD_EXPRESSIONS[this.state.mood] || MOOD_EXPRESSIONS.calm;
    const intensity = THREE.MathUtils.clamp(this.state.intensity || 0, 0, 1);
    const actionTargets = this.actionFrame
      ? actionExpressionTargets(this.actionFrame.action, this.actionFrame.weight)
      : {};
    for (const name of ["happy", "angry", "sad", "relaxed", "surprised"]) {
      const moodTarget = (moodTargets[name] || 0) * (0.35 + intensity * 0.65);
      this.applyExpression(name, Math.max(moodTarget, actionTargets[name] || 0), delta, immediate);
    }

    this.blinkElapsed += delta;
    let blink = 0;
    if (this.blinkElapsed >= this.nextBlinkAt) {
      const phase = (this.blinkElapsed - this.nextBlinkAt) / 0.14;
      blink = phase < 0.5 ? phase * 2 : Math.max(0, (1 - phase) * 2);
      if (phase >= 1) {
        this.blinkElapsed = 0;
        this.nextBlinkAt = 2.2 + Math.random() * 3.2;
      }
    }
    if (this.actionFrame) {
      blink = Math.max(
        blink,
        actionBlinkWeight(this.actionFrame.action, this.actionFrame.progress)
          * this.actionFrame.weight,
      );
    }
    this.applyExpression("blink", blink, delta, immediate);

    const audioLevel = THREE.MathUtils.clamp(Number(this.context.audioLevel) || 0, 0, 1);
    const mouth = this.context.speaking
      ? THREE.MathUtils.clamp(0.04 + audioLevel * 1.05, 0, 0.95)
      : 0;
    this.applyExpression("aa", mouth, delta, immediate);
  }

  updateLookTarget(delta, immediate = false) {
    if (!this.vrm || !this.avatarFrame) return;
    this.idleElapsed += delta;
    if (this.finishedRemaining > 0) this.finishedRemaining = Math.max(0, this.finishedRemaining - delta);
    const conversationState = this.getConversationState();
    const allowIdle = this.interaction.focused
      && !this.interaction.pointerActive
      && !this.context.blocked
      && conversationState === "idle";
    if (!allowIdle) this.scheduler.cancelIdle();
    this.actionFrame = this.scheduler.tick(delta, { allowIdle });

    const active = this.interaction.pointerActive && this.interaction.focused;
    const targetX = active
      ? this.interaction.targetPointer.x
      : this.actionFrame?.action === "idle_look"
        ? (this.actionFrame.variant?.x || 0) * 0.35 * this.actionFrame.weight
        : conversationState === "thinking"
          ? 0.16
          : 0;
    const targetY = active
      ? this.interaction.targetPointer.y
      : this.actionFrame?.action === "idle_look"
        ? (this.actionFrame.variant?.y || 0) * 0.18 * this.actionFrame.weight
        : conversationState === "thinking"
          ? -0.04
          : 0;
    const blend = immediate ? 1 : smoothFactor(delta, 5.5);
    this.interaction.pointer.x = THREE.MathUtils.lerp(this.interaction.pointer.x, targetX, blend);
    this.interaction.pointer.y = THREE.MathUtils.lerp(this.interaction.pointer.y, targetY, blend);

    const { center, size, targetY: lookY } = this.avatarFrame;
    this.lookTarget.position.set(
      center.x + this.interaction.pointer.x * size.x * 0.42,
      lookY + this.interaction.pointer.y * size.y * 0.28,
      this.camera.position.z,
    );

    if (this.vrm.lookAt) {
      this.vrm.lookAt.yaw = this.interaction.pointer.x * 12;
      this.vrm.lookAt.pitch = this.interaction.pointer.y * 8;
    }

    const action = this.actionFrame?.action;
    const actionWeightValue = this.actionFrame?.weight || 0;
    const variantX = this.actionFrame?.variant?.x || 0;
    const valence = THREE.MathUtils.clamp(this.context.valence ?? 0.5, 0, 1);
    const arousal = THREE.MathUtils.clamp(this.context.arousal ?? 0.5, 0, 1);
    const emotionAmount = THREE.MathUtils.clamp(this.context.intensity || 0, 0, 1)
      * (0.3 + arousal * 0.7)
      * 0.6;
    const emotionScale = ["sad", "hurt"].includes(this.context.mood)
      ? 0.72 - arousal * 0.12
      : 0.82 + arousal * 0.18;
    const positiveMotion = (valence - 0.5) * 2;
    let headX = -this.interaction.pointer.y * 0.1;
    let headY = this.interaction.pointer.x * 0.16;
    let headZ = 0;
    let bodyX = Math.sin(this.idleElapsed * 1.35) * 0.006;
    let bodyY = 0;
    let bodyZ = 0;
    let hipsX = 0;
    let hipsY = 0;
    let hipsZ = 0;
    let leftShoulderX = 0;
    let leftShoulderY = 0;
    let leftShoulderZ = 0;
    let rightShoulderX = 0;
    let rightShoulderY = 0;
    let rightShoulderZ = 0;
    let leftUpperArmX = -0.035;
    let leftUpperArmY = 0;
    let leftUpperArmZ = -1.18;
    let rightUpperArmX = -0.035;
    let rightUpperArmY = 0;
    let rightUpperArmZ = 1.18;
    let leftLowerArmX = 0.08;
    let leftLowerArmY = 0;
    let leftLowerArmZ = 0;
    let rightLowerArmX = -0.08;
    let rightLowerArmY = 0;
    let rightLowerArmZ = 0;
    let leftHandX = 0;
    let leftHandY = 0;
    let leftHandZ = 0;
    let rightHandX = 0;
    let rightHandY = 0;
    let rightHandZ = 0;
    let leftUpperLegX = 0;
    let rightUpperLegX = 0;
    let leftLowerLegX = 0;
    let rightLowerLegX = 0;

    if (this.context.mood === "happy" || this.context.mood === "proud") {
      headX -= 0.025 * emotionAmount * (0.8 + Math.max(positiveMotion, 0) * 0.2);
      bodyX -= 0.018 * emotionAmount * (0.8 + Math.max(positiveMotion, 0) * 0.2);
      leftUpperArmX -= 0.02 * emotionAmount;
      rightUpperArmX -= 0.02 * emotionAmount;
    } else if (this.context.mood === "sad" || this.context.mood === "hurt") {
      headX += 0.07 * emotionAmount;
      bodyX += 0.02 * emotionAmount;
      headZ -= 0.025 * emotionAmount * (0.8 + Math.max(-positiveMotion, 0) * 0.2);
    } else if (this.context.mood === "annoyed") {
      headY += 0.055 * emotionAmount * (0.8 + arousal * 0.2);
      bodyY -= 0.025 * emotionAmount * (0.8 + arousal * 0.2);
      headZ += 0.02 * emotionAmount;
    }

    if (conversationState === "listening") {
      headZ += Math.sin(this.idleElapsed * 2.2) * (0.008 + arousal * 0.008);
    } else if (conversationState === "thinking") {
      headZ += 0.04 + arousal * 0.02;
      headY += 0.025 + arousal * 0.02;
    } else if (conversationState === "talking") {
      headX += Math.sin(this.idleElapsed * 2.1) * (0.008 + arousal * 0.008);
      bodyX += Math.sin(this.idleElapsed * 1.8) * (0.004 + arousal * 0.004);
    } else if (conversationState === "finished") {
      const finishedProgress = 1 - this.finishedRemaining / 0.9;
      headX -= Math.sin(finishedProgress * Math.PI) * 0.05;
    }

    if (action === "idle_tilt") headZ += 0.08 * variantX * actionWeightValue;
    if (action === "idle_weight") {
      hipsZ += 0.035 * variantX * actionWeightValue;
      bodyZ += 0.018 * variantX * actionWeightValue;
    }
    if (action === "idle_arm") {
      if (variantX >= 0) {
        leftUpperArmZ += 0.12 * actionWeightValue;
        leftLowerArmZ -= 0.04 * actionWeightValue;
      } else {
        rightUpperArmZ -= 0.12 * actionWeightValue;
        rightLowerArmZ += 0.04 * actionWeightValue;
      }
    }
    if (action === "head_nod") headX -= Math.sin(this.actionFrame.progress * Math.PI) * 0.1;
    if (action === "head_tilt") headZ += 0.085 * variantX * actionWeightValue;
    if (action === "left_arm_lift") {
      leftUpperArmZ += 0.26 * actionWeightValue;
      leftLowerArmZ -= 0.14 * actionWeightValue;
      headY += 0.018 * actionWeightValue;
    }
    if (action === "right_arm_lift") {
      rightUpperArmZ -= 0.26 * actionWeightValue;
      rightLowerArmZ += 0.14 * actionWeightValue;
      headY -= 0.018 * actionWeightValue;
    }
    if (action === "left_arm_dodge") {
      leftUpperArmY += 0.1 * actionWeightValue;
      bodyY -= 0.025 * actionWeightValue;
    }
    if (action === "right_arm_dodge") {
      rightUpperArmY -= 0.1 * actionWeightValue;
      bodyY += 0.025 * actionWeightValue;
    }
    if (action === "body_lean") bodyX -= 0.045 * actionWeightValue;
    if (action === "body_sway") bodyZ += 0.07 * variantX * actionWeightValue;
    if (action === "waist_twist") hipsY += 0.07 * variantX * actionWeightValue;
    if (action === "waist_recoil") hipsX += 0.055 * actionWeightValue;
    if (action === "left_leg_shift") leftUpperLegX += 0.065 * actionWeightValue;
    if (action === "right_leg_shift") rightUpperLegX += 0.065 * actionWeightValue;
    if (action === "left_leg_recoil") {
      leftUpperLegX += 0.06 * actionWeightValue;
      leftLowerLegX -= 0.075 * actionWeightValue;
    }
    if (action === "right_leg_recoil") {
      rightUpperLegX += 0.06 * actionWeightValue;
      rightLowerLegX -= 0.075 * actionWeightValue;
    }
    if (action === "greeting_wave") {
      const greeting = greetingPose(this.actionFrame.progress);
      rightShoulderZ = greeting.shoulderZ;
      rightUpperArmX = greeting.upperArmX;
      rightUpperArmY = greeting.upperArmY;
      rightUpperArmZ = greeting.upperArmZ;
      rightLowerArmX = greeting.lowerArmX;
      rightLowerArmZ = greeting.lowerArmZ;
      rightHandX = greeting.wristX;
      rightHandY = greeting.wristY;
      rightHandZ = greeting.wristZ;
      headX -= 0.035 * greeting.nod;
      headY -= 0.022 * greeting.raise;
      headZ += 0.014 * greeting.raise;
      bodyY += 0.022 * greeting.raise;
      bodyZ += 0.012 * greeting.raise;
      hipsZ -= 0.006 * greeting.raise;
    }

    this.applyPoseBone(
      "head",
      headX,
      headY,
      headZ,
      immediate ? 1 : blend * 0.86 * emotionScale,
      immediate,
    );
    this.applyPoseBone(
      "body",
      bodyX,
      bodyY,
      bodyZ,
      immediate ? 1 : blend * 0.78 * emotionScale,
      immediate,
    );
    this.applyPoseBone(
      "hips",
      hipsX,
      hipsY,
      hipsZ,
      immediate ? 1 : blend * 0.72 * emotionScale,
      immediate,
    );
    this.applyPoseBone("leftShoulder", leftShoulderX, leftShoulderY, leftShoulderZ, immediate ? 1 : blend * 0.74, immediate);
    this.applyPoseBone("rightShoulder", rightShoulderX, rightShoulderY, rightShoulderZ, immediate ? 1 : blend * 0.74, immediate);
    this.applyPoseBone(
      "leftUpperArm",
      leftUpperArmX,
      leftUpperArmY,
      leftUpperArmZ,
      immediate ? 1 : blend * 0.8 * emotionScale,
      immediate,
    );
    this.applyPoseBone(
      "rightUpperArm",
      rightUpperArmX,
      rightUpperArmY,
      rightUpperArmZ,
      immediate ? 1 : blend * 0.8 * emotionScale,
      immediate,
    );
    this.applyPoseBone("leftLowerArm", leftLowerArmX, leftLowerArmY, leftLowerArmZ, immediate ? 1 : blend * 0.76 * emotionScale, immediate);
    this.applyPoseBone("rightLowerArm", rightLowerArmX, rightLowerArmY, rightLowerArmZ, immediate ? 1 : blend * 0.76 * emotionScale, immediate);
    this.applyPoseBone("leftHand", leftHandX, leftHandY, leftHandZ, immediate ? 1 : blend * 0.72, immediate);
    this.applyPoseBone("rightHand", rightHandX, rightHandY, rightHandZ, immediate ? 1 : blend * 0.72, immediate);
    this.applyPoseBone("leftUpperLeg", leftUpperLegX, 0, 0, immediate ? 1 : blend * 0.62 * emotionScale, immediate);
    this.applyPoseBone("rightUpperLeg", rightUpperLegX, 0, 0, immediate ? 1 : blend * 0.62 * emotionScale, immediate);
    this.applyPoseBone("leftLowerLeg", leftLowerLegX, 0, 0, immediate ? 1 : blend * 0.58 * emotionScale, immediate);
    this.applyPoseBone("rightLowerLeg", rightLowerLegX, 0, 0, immediate ? 1 : blend * 0.58 * emotionScale, immediate);
  }

  animate = () => {
    if (this.disposed) return;
    const delta = Math.min(this.clock.getDelta(), 0.05);
    this.updateLookTarget(delta);
    this.applyState(delta);
    this.vrm?.update(delta);
    this.renderer.render(this.scene, this.camera);
    this.frameId = requestAnimationFrame(this.animate);
  };

  removeCurrentVrm() {
    if (!this.vrm) return;
    this.scene.remove(this.vrm.scene);
    this.vrm.scene.traverse((object) => {
      object.geometry?.dispose();
      if (Array.isArray(object.material)) object.material.forEach(disposeMaterial);
      else if (object.material) disposeMaterial(object.material);
    });
    this.vrm = null;
    this.avatarFrame = null;
    this.poseBones.clear();
    this.expressionWeights.clear();
    this.scheduler.reset();
    this.actionFrame = null;
  }

  dispose() {
    this.disposed = true;
    if (this.frameId) cancelAnimationFrame(this.frameId);
    this.resizeObserver.disconnect();
    this.removeCurrentVrm();
    this.renderer.dispose();
    this.renderer.domElement.remove();
  }
}
