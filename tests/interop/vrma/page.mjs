// Plays Omuzaki's .vrma files on Omuzaki's own VRMs with three-vrm, the way a three.js app does, and reads back what
// the player poses at each probe time. `check.mjs` compares the readings with Omuzaki's own (`expected.json`).

import * as THREE from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import { VRMLoaderPlugin } from '@pixiv/three-vrm';
import {
  VRMAnimationLoaderPlugin,
  VRMLookAtQuaternionProxy,
  createVRMAnimationClip,
} from '@pixiv/three-vrm-animation';

// What the player warned about while it read our animation files: the check fails on any.
const warnings = [];
let phase = null;
const warn = console.warn.bind(console);
console.warn = (...args) => {
  if (phase) warnings.push(`${phase}: ${args.map(String).join(' ')}`);
  warn(...args);
};

const loader = new GLTFLoader();
loader.register((parser) => new VRMLoaderPlugin(parser));
loader.register((parser) => new VRMAnimationLoaderPlugin(parser));

async function loadVrm(file) {
  const vrm = (await loader.loadAsync(`/fixtures/${file}`)).userData.vrm;
  if (vrm.lookAt) {
    // A named proxy, as three-vrm asks an app to add, so the clip drives the gaze without a warning.
    const proxy = new VRMLookAtQuaternionProxy(vrm.lookAt);
    proxy.name = 'lookAtQuaternionProxy';
    vrm.scene.add(proxy);
  }
  return vrm;
}

// The pose `vrm` shows at each of `animation.times`, read from the normalised bones as an app reads them.
function sample(vrm, clip, animation) {
  const proxy = vrm.scene.children.find((o) => o instanceof VRMLookAtQuaternionProxy);
  const mixer = new THREE.AnimationMixer(vrm.scene);
  const action = mixer.clipAction(clip);
  if (!animation.looped) {
    action.setLoop(THREE.LoopOnce, 1);
    action.clampWhenFinished = true;
  }
  action.play();
  const out = animation.times.map((t) => {
    mixer.setTime(t);
    const bones = {};
    for (const name of Object.keys(animation.bones)) {
      const node = vrm.humanoid.getNormalizedBoneNode(name);
      bones[name] = node ? node.quaternion.toArray() : null;
    }
    const expressions = {};
    for (const name of Object.keys(animation.expressions)) {
      expressions[name] = vrm.expressionManager ? vrm.expressionManager.getValue(name) : null;
    }
    return {
      bones,
      hips: vrm.humanoid.getNormalizedBoneNode('hips').position.toArray(),
      expressions,
      look_at: proxy ? { quaternion: proxy.quaternion.toArray(), yaw: vrm.lookAt.yaw, pitch: vrm.lookAt.pitch } : null,
    };
  });
  mixer.stopAllAction();
  mixer.uncacheRoot(vrm.scene);
  vrm.humanoid.resetNormalizedPose();
  vrm.expressionManager?.resetValues();
  proxy?.quaternion.identity();
  return out;
}

window.play = async (manifest) => {
  const result = { models: {}, warnings };
  for (const model of Object.keys(manifest.models)) {
    const vrm = await loadVrm(model);
    const played = {
      look_at: vrm.lookAt != null,
      player_rest_hips_y: vrm.humanoid.normalizedRestPose.hips.position[1],
      animations: {},
    };
    for (const animation of manifest.animations) {
      phase = `${model} ${animation.file}`;
      const gltf = await loader.loadAsync(`/fixtures/${animation.file}`);
      const vrmAnimation = gltf.userData.vrmAnimations?.[0];
      if (!vrmAnimation) {
        warnings.push(`${phase}: no VRM animation in the file`);
        continue;
      }
      const clip = createVRMAnimationClip(vrmAnimation, vrm);
      phase = null;
      played.animations[animation.file] = {
        duration: vrmAnimation.duration,
        rest_hips_y: vrmAnimation.restHipsPosition.y,
        samples: sample(vrm, clip, animation),
      };
    }
    result.models[model] = played;
  }
  return result;
};

window.ready = true;
