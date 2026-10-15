// Generates three_vrm.txt: reference trajectories from three-vrm's spring bone implementation, an independent,
// widely used implementation of the VRMC_springBone 1.0 algorithm. The Rust tests (`tests.rs`, `three_vrm`) replay the
// same scenes on `VrmSpringSolver` and compare. Only the numbers it prints are committed; three-vrm is MIT-licensed
// (docs/licensing/register.md) and is not a dependency of the project.
//
// To regenerate, in an empty directory:
//   npm install --ignore-scripts @pixiv/three-vrm-springbone@3.5.5 three@0.170.0
//   node three_vrm_golden.mjs "$PWD/node_modules" > three_vrm.txt
//
// The scene is mirrored by hand in `tests.rs`; change both together. Every number is written out in the open so the two
// stay comparable: a mismatch shows up as a failing comparison, not a silent drift.

import { pathToFileURL } from 'node:url';
import { join } from 'node:path';

const modules = process.argv[2];
if (!modules) throw new Error('usage: node three_vrm_golden.mjs <path to node_modules>');
const THREE = await import(pathToFileURL(join(modules, 'three/build/three.module.js')).href);
const {
  VRMSpringBoneJoint, VRMSpringBoneManager, VRMSpringBoneCollider,
  VRMSpringBoneColliderShapeSphere, VRMSpringBoneColliderShapeCapsule, VRMSpringBoneColliderShapePlane,
} = await import(pathToFileURL(join(modules, '@pixiv/three-vrm-springbone/lib/three-vrm-springbone.module.js')).href);

const FRAMES = 100;
const SUBSTEPS_PER_FRAME = 2; // 60 fps frames of two 1/120 s sub-steps
const DT = 1 / 120;
const v = (x, y, z) => new THREE.Vector3(x, y, z);
const axisAngle = (axis, angle) => new THREE.Quaternion().setFromAxisAngle(axis, angle);
const X = v(1, 0, 0), Y = v(0, 1, 0), Z = v(0, 0, 1);

// name, parent, rest position, rest rotation
const HAIR = [
  ['h0', 'head', v(0.02, -0.06, 0.01), axisAngle(Z, 0.15)],
  ['h1', 'h0', v(-0.01, -0.065, 0.0), axisAngle(X, 0.1)],
  ['h2', 'h1', v(0.0, -0.07, 0.015), axisAngle(Z, -0.1)],
  ['h3', 'h2', v(0.0, -0.05, 0.0), new THREE.Quaternion()],
];
const SETTINGS = { stiffness: 1.5, gravityPower: 1.0, gravityDir: v(0, -1, 0), dragForce: 0.4, hitRadius: 0.02 };

// The collider sets a scenario can name, each a function of the bones returning the colliders of its one group. Each
// is mirrored number for number in tests.rs (`mod three_vrm`). The chain hangs clear of every collider at rest and
// meets it when it swings.
const COLLIDER_SETS = {
  none: () => [],
  body: (bones) => {
    const sphere = new VRMSpringBoneCollider(new VRMSpringBoneColliderShapeSphere({ offset: v(0, -0.12, 0), radius: 0.1 }));
    bones.head.add(sphere);
    const capsule = new VRMSpringBoneCollider(new VRMSpringBoneColliderShapeCapsule({ offset: v(0, 0, 0), radius: 0.06, tail: v(0, 0.15, 0.03) }));
    bones.spine.add(capsule);
    return [sphere, capsule];
  },
  // An inside sphere 0.1 below the root h0.
  sphere_inside: (bones) => {
    const shape = new VRMSpringBoneColliderShapeSphere({ offset: v(0.02, -0.16, 0.01), radius: 0.12, inside: true });
    const collider = new VRMSpringBoneCollider(shape);
    bones.head.add(collider);
    return [collider];
  },
  // An inside capsule from the root h0 straight down.
  capsule_inside: (bones) => {
    const shape = new VRMSpringBoneColliderShapeCapsule({ offset: v(0.02, -0.06, 0.01), radius: 0.05, tail: v(0.02, -0.26, 0.01), inside: true });
    const collider = new VRMSpringBoneCollider(shape);
    bones.head.add(collider);
    return [collider];
  },
  // A plane just behind the spine, facing forward.
  plane: (bones) => {
    const collider = new VRMSpringBoneCollider(new VRMSpringBoneColliderShapePlane({ offset: v(0, 0, -0.02), normal: v(0, 0, 1) }));
    bones.spine.add(collider);
    return [collider];
  },
};

function build({ center, colliders }) {
  const bones = {};
  const add = (name, parent, position, rotation) => {
    const o = new THREE.Object3D();
    o.name = name;
    o.position.copy(position);
    o.quaternion.copy(rotation);
    if (parent) bones[parent].add(o);
    bones[name] = o;
  };
  add('hips', null, v(0, 1, 0), new THREE.Quaternion());
  add('spine', 'hips', v(0, 0.1, 0), new THREE.Quaternion());
  add('head', 'spine', v(0, 0.1, 0), new THREE.Quaternion());
  for (const [name, parent, position, rotation] of HAIR) add(name, parent, position, rotation);

  const groups = [];
  const placed = COLLIDER_SETS[colliders ?? 'none'](bones);
  if (placed.length > 0) groups.push({ colliders: placed });
  const manager = new VRMSpringBoneManager();
  const names = HAIR.map(([name]) => name);
  for (let i = 0; i < names.length - 1; i++) {
    const joint = new VRMSpringBoneJoint(bones[names[i]], bones[names[i + 1]], SETTINGS, groups);
    // three-vrm re-measures the bone from its child's world matrix on every update, and a child that is itself a
    // spring bone has not been updated yet that sub-step, so the "length" wobbles by however far the parents just
    // swung. The spec's `boneLength` is fixed at initialisation; with no scale in the scene that is the rest length.
    joint._calcWorldSpaceBoneLength = function () {
      this._worldSpaceBoneLength = this.initialLocalChildPosition.length();
    };
    if (center) joint.center = bones[center];
    manager.addJoint(joint);
  }
  return { bones, manager, placed, swinging: names.slice(0, -1) };
}

// The animated pose at a frame; mirrored in `Fixture`-free form by `golden_pose` in tests.rs.
function animate(bones, frame) {
  const f = frame;
  bones.hips.position.set(0.35 * Math.sin(0.09 * f), 1 + 0.04 * Math.sin(0.13 * f), 0.15 * Math.cos(0.07 * f));
  bones.hips.quaternion.copy(axisAngle(Y, 0.4 * Math.sin(0.06 * f)));
  bones.head.quaternion.copy(axisAngle(X, 0.3 * Math.sin(0.11 * f)).multiply(axisAngle(Z, 0.35 * Math.sin(0.15 * f))));
}

function run(name, options) {
  const { bones, manager, placed, swinging } = build(options);
  animate(bones, 0);
  bones.hips.updateMatrixWorld(true);
  manager.setInitState();
  const lines = [`scenario ${name} frames=${FRAMES} joints=${swinging.length}`];
  for (let frame = 0; frame < FRAMES; frame++) {
    animate(bones, frame);
    for (let s = 0; s < SUBSTEPS_PER_FRAME; s++) {
      for (const collider of placed) collider.updateWorldMatrix(true, false);
      manager.update(DT);
    }
    const q = swinging.flatMap((n) => bones[n].quaternion.toArray());
    lines.push(q.map((x) => x.toFixed(7)).join(' '));
  }
  return lines;
}

const out = [
  `# three-vrm spring bone reference trajectories: @pixiv/three-vrm-springbone 3.5.5, three 0.170.0`,
  `# one line per 60 fps frame: the local quaternion (x y z w) of each swinging bone, h0 first`,
  ...run('free', { center: null, colliders: null }),
  ...run('centre', { center: 'hips', colliders: null }),
  ...run('colliders', { center: null, colliders: 'body' }),
  ...run('sphere_inside', { center: null, colliders: 'sphere_inside' }),
  ...run('capsule_inside', { center: null, colliders: 'capsule_inside' }),
  ...run('plane', { center: null, colliders: 'plane' }),
];
console.log(out.join('\n'));
