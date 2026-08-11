import { describe, expect, it } from "vitest";
import * as THREE from "three";
import { fixHairHighlightOverlay } from "./hairMaterialFix.js";

describe("fixHairHighlightOverlay", () => {
  it("makes only the emissive hair highlight additive and transparent", () => {
    const root = new THREE.Group();
    const highlight = new THREE.MeshStandardMaterial({ name: "髮+" });
    highlight.emissiveMap = new THREE.Texture();
    const hair = new THREE.MeshStandardMaterial({ name: "髮" });
    const mesh = new THREE.Mesh(new THREE.BufferGeometry(), [hair, highlight]);
    root.add(mesh);

    expect(fixHairHighlightOverlay(root)).toBe(1);
    expect(highlight.transparent).toBe(true);
    expect(highlight.blending).toBe(THREE.AdditiveBlending);
    expect(highlight.depthWrite).toBe(false);
    expect(highlight.side).toBe(THREE.DoubleSide);
    expect(highlight.toneMapped).toBe(false);
    expect(mesh.renderOrder).toBe(10);
    expect(hair.transparent).toBe(false);
    expect(hair.blending).toBe(THREE.NormalBlending);
  });

  it("ignores highlight materials without an emissive texture", () => {
    const root = new THREE.Group();
    const material = new THREE.MeshStandardMaterial({ name: "髮+" });
    const mesh = new THREE.Mesh(new THREE.BufferGeometry(), material);
    root.add(mesh);

    expect(fixHairHighlightOverlay(root)).toBe(0);
    expect(material.transparent).toBe(false);
    expect(mesh.renderOrder).toBe(0);
  });
});
