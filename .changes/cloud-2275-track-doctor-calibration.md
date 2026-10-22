# Tracking: the doctor and the guided calibration

- The tracking doctor watches the webcam and the phone while they run, and says what is hurting tracking and how to
  fix it: too dark, a light behind you, glare on glasses, your face at the edge of the picture, lost frames, a
  machine that can't keep up, or a phone on uneven Wi-Fi. `input.doctor` reads it; `input.recheck` measures again
  after a fix.
- The guided calibration: about forty seconds of prompts (relax, big smile, brows up and down, wide eyes, squint,
  mouth open, kiss, puffed cheeks, one-sided smiles, eyes closed, look around) fit each control to your face, and the
  result is used at once. `input.calibrate` starts or cancels it while the preview runs; `input.calibration` shows the
  prompt and the time left. The Live workspace's panel for it comes next.
- User-supplied models: add a model file to a slot, accept its notice, select, check and remove it (`model.*`).
