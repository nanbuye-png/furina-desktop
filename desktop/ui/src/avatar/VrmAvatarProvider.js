import * as THREE from "three";
import { GLTFLoader } from "three/examples/jsm/loaders/GLTFLoader.js";
import { VRMLoaderPlugin } from "@pixiv/three-vrm";
import { fixHairHighlightOverlay } from "./hairMaterialFix.js";

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
    this.expressionWeights = new Map();
    this.blinkElapsed = 0;
    this.nextBlinkAt = 2.4;
    this.mouthElapsed = 0;

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
    this.frameAvatar();
    this.applyState(0, true);
  }

  setState(nextState) {
    this.state = { ...this.state, ...nextState };
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
    if (!manager) return;
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
    for (const name of ["happy", "angry", "sad", "relaxed", "surprised"]) {
      this.applyExpression(name, (moodTargets[name] || 0) * (0.35 + intensity * 0.65), delta, immediate);
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
    this.applyExpression("blink", blink, delta, immediate);

    this.mouthElapsed += delta;
    const mouth = this.state.speaking
      ? 0.18 + Math.abs(Math.sin(this.mouthElapsed * 11.5)) * 0.48
      : 0;
    this.applyExpression("aa", mouth, delta, immediate);
  }

  animate = () => {
    if (this.disposed) return;
    const delta = Math.min(this.clock.getDelta(), 0.05);
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