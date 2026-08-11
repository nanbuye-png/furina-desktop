import * as THREE from "three";

export function fixHairHighlightOverlay(root) {
  let fixedMeshes = 0;
  root.traverse((object) => {
    if (!object.isMesh || !object.material) return;
    const materials = Array.isArray(object.material) ? object.material : [object.material];
    let usesHighlightOverlay = false;
    for (const material of materials) {
      if (material.name !== "髮+" || !material.emissiveMap) continue;
      material.transparent = true;
      material.blending = THREE.AdditiveBlending;
      material.depthWrite = false;
      material.side = THREE.DoubleSide;
      material.toneMapped = false;
      material.needsUpdate = true;
      usesHighlightOverlay = true;
    }
    if (usesHighlightOverlay) {
      object.renderOrder = 10;
      fixedMeshes += 1;
    }
  });
  return fixedMeshes;
}
