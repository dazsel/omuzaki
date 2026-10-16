---
title: "Do the face models' remaining licence questions (GHUM, Google download terms) block shipping? (2026-10-15)"
summary: "Asked whether GHUM's licence or Google's download terms limit the MediaPipe face networks we ship; answer (Director ruling): cleared, ship under Apache-2.0 with the cards, hashes and notices archived; GHUM's own licence text is not public but binds its signatories, not recipients of the weights."
area: research
---

# Do the face models' remaining licence questions (GHUM, Google download terms) block shipping? (2026-10-15)

Research for #1887. Source marks: **verified** (I read the primary source), **secondary** (someone's summary), **unverified** (not found or not readable). All pages were retrieved on 2026-10-15.

## 1. The answer

- **Verdict: cleared. Ship the face models under Apache-2.0, as the note's "clear with conditions" reading, and keep the record listed below.** The Director settled the seven questions of section 7 with the owner; none needs legal review. Google states Apache-2.0 on each of the three model cards, and the unchanged `geometry_pipeline_metadata_landmarks.binarypb` ships under the licence of the same bundle (see the next bullets and Q2).
- **GHUM is a training-data question for one network only.** Only the Blendshape V2 card mentions GHUM. The other two cards do not. Nothing from GHUM itself (mesh, weights, code) is in the files we ship. **Verified.**
- **GHUM's licence text is not public, and it does not need to be.** The release is gated behind a request form that asks for an academic institution, an academic email address and a legal representative "for the license signature". The GHUM page says "The models are available for research." That reads as a research-oriented licence for GHUM itself. The text of it is **unverified**. Google owns both GHUM and the network and published the weights under Apache-2.0. That grant stands on its own, and GHUM's research terms bind GHUM's signatories, not recipients of the weights (Director ruling, Q1). `MODELS.md` says the network was trained by Google using the GHUM mesh.
- **Google's download terms do not reach a bare download.** Those terms bind a person who is "accessing or using our APIs". The model files are plain public objects in a storage bucket, and the files themselves carry no terms. A public, unauthenticated object fetch does not make us a party to Google's API terms (Director ruling, Q3), and we redistribute our own copies under Apache-2.0 anyway. Using MediaPipe's *software API* is a different matter and is not what we do (we run our own ONNX runtime).
- **The metadata file.** `geometry_pipeline_metadata_landmarks.binarypb` is copied unchanged from the bundle and no card describes it. It ships under the same bundle's licence (Director ruling, Q2). Supporting evidence: the MediaPipe repository, itself Apache-2.0 (https://github.com/google-ai-edge/mediapipe/blob/master/LICENSE), holds the text-format version of the same data at https://github.com/google-ai-edge/mediapipe/blob/master/mediapipe/modules/face_geometry/data/geometry_pipeline_metadata_landmarks.pbtxt (existence checked 2026-10-16).
- **Decision this supports:** keep the face models in the V0 zip as ONNX conversions, with the three cards and their hashes archived and the Apache-2.0 text and a notice shipped. The register row's `condition` carries the narrower wording in section 8. The fallback in section 6 stays as a contingency if Google ever changes its licence statement, not as a gate.


## 2. What exactly we ship and download

From `spikes/webcam-track/models/PROVENANCE.md`, `tools/models/mediapipe-face.sh`, `dist/notices/MODELS.md` and `docs/licensing/register.md` on `origin/main`.

| Item | Where it comes from | Shipped? | Card / licence |
|---|---|---|---|
| `face_landmarker.task` (float16, v1, 3,758,596 bytes, SHA-256 `64184e22...c9ff`) | Downloaded by `tools/models/mediapipe-face.sh` from `https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/face_landmarker.task` | No. It is the build input; the script keeps it outside the repo. | Three cards below |
| `face_detector.onnx` | Converted from the `.task`'s BlazeFace short-range network | Yes (V0 zip) | BlazeFace card: Apache-2.0 |
| `face_landmarks_detector.onnx` | Converted from the Face Mesh V2 network | Yes (V0 zip) | Face Mesh V2 card: Apache-2.0 |
| `face_blendshapes.onnx` | Converted from the Blendshape V2 network | Yes (V0 zip) | Blendshape V2 card: Apache-2.0; **mentions GHUM** |
| `geometry_pipeline_metadata_landmarks.binarypb` | Copied unchanged from the bundle (`xtask/src/dist.rs` requires it in every V0 zip that has models) | Yes (V0 zip) | No card of its own; ships under the same bundle's licence (Director ruling, Q2) |
| The three model-card PDFs | `https://storage.googleapis.com/mediapipe-assets/...` | Archived as the licence record, hashes in `SHA256SUMS`; the register says to archive them | n/a |

Two facts worth noting:

- **Two buckets are involved.** The model bundle comes from `mediapipe-models`. The cards come from `mediapipe-assets`. The issue text names only `mediapipe-assets`; the build script uses `mediapipe-models`. The terms analysis is the same for both. **Verified** (script and `PROVENANCE.md`; both buckets answered a plain `GET` on 2026-10-15 with no sign-in).
- **We ship conversions of the three networks, plus one unchanged metadata file; not the bundle.** The conversion (TFLite to ONNX, PReLU rewrite, batch fixed to 1) keeps the weights' values. Apache-2.0 allows derivative works, so the conversions inherit the Apache-2.0 label. **Verified** against section 4 of the licence (below).

Nothing else in the face-tracking path is a model with a licence question: the ONNX runtime is MIT, and the register rows for it and for `ort` are separate.

## 3. What each model card says

I extracted the text of all three PDFs. Retrieved 2026-10-15 from `https://storage.googleapis.com/mediapipe-assets/`.

| Card | File and SHA-256 of the PDF I read | Licence line | Authors | Training data, as stated |
|---|---|---|---|---|
| BlazeFace (Short Range) | `MediaPipe BlazeFace Model Card (Short Range).pdf`, `cd335c06fc0de7807cd815a0777a697932598bcdb28fa98adaaabf847485f758` | "LICENSED UNDER Apache License, Version 2.0" | V. Bazarevsky, Y. Kartynnik, A. Ablavatski (Google) | "trained and evaluated on consented images of people using a mobile AR application" |
| Face Mesh V2 | `Model Card MediaPipe Face Mesh V2.pdf`, `c6add060f4ebfb37b2690136b6c711c7e5fcb7038baa2649ae3338b83979565a` | "LICENSED UNDER Apache License, Version 2.0" | G. Yan, I. Grishchenko (Google) | "captured on a diverse set of smartphone cameras ... via an AR (Augmented Reality) application"; the card does not use the word "consent"; "the z-coordinate is obtained from synthetic data" (source of that data not stated) |
| Blendshape V2 | `Model Card Blendshape V2.pdf`, `c8e9cf60a39998f4b341740623917590e050d1c97004e2de4568d84e026445ae` | "LICENSED UNDER Apache License, Version 2.0" | I. Grishchenko, G. Yan, A. Zanfir, E. G. Bazavan (Google) | "All dataset samples were captured in a controlled environment in the lab ... reconstructed unified 3D GHUM Mesh of a human subject"; "Blendshapes for the canonical GHUM Mesh were designed by an artist" |

All three: **verified.** The PDFs were last modified 2023-05-12 (Blendshape, per the `Last-Modified` header). The cards are dated 2021-06-09 (BlazeFace), 2022-09-15 (Face Mesh V2) and 2022-11-11 (Blendshape V2).

Use limits stated on the cards (these are intended-use notes, not licence terms; Apache-2.0 has no field-of-use clause):

- BlazeFace: "Any form of surveillance or identity recognition is explicitly out of scope and not enabled by this technology."
- Face Mesh and Blendshape: "This model is not intended for human life-critical decisions." The landmarks and blendshapes "do not provide facial recognition or identification and do not store any unique face representation."
- Both Face Mesh and Blendshape name the intended use as "AR entertainment" and "controlling an avatar's face". That matches our use.

Corroboration that the cards are meant to cover the weights: a maintainer wrote "Our source and the models (unless otherwise noted) are licensed under Apache2" (`google-ai-edge/mediapipe` issue #6355, reply dated 2026-10-05; **secondary**: a maintainer comment, not a licence file). The same thread shows another user asking for licence terms that "explicitly cover the embedded model weights" and getting only that sentence, so Google has not published more.

There is **no licence file inside the `.task` bundle** (it holds three `.tflite` files and one `.binarypb`), so the cards are the only licence record. **Verified** locally by the earlier audit (A1); I did not re-open the bundle.

## 4. Question 1: GHUM

### What GHUM is

GHUM and GHUML are Google Research's statistical 3D human body models (CVPR 2020, Xu, Bazavan, Zanfir, Freeman, Sukthankar, Sminchisescu). The repository page says they were trained on "over 60, 000 diverse human configurations" from "60 people". The Blendshape V2 card uses the **GHUM mesh** (a template face mesh with artist-made blendshapes) to build training samples: lab scans of subjects are fitted to that mesh and the network learns to predict blendshape coefficients from landmarks.

### The sources on GHUM's terms

| Source | What it says | URL | Mark |
|---|---|---|---|
| GHUM page at Google Research | "The models are available for research." | https://research.google/pubs/ghum-ghuml-generative-3d-human-shape-and-articulated-pose-models/ | verified |
| `google-research/google-research`, `ghum/README.md` | Download is by "GHUM/GHUM-L request form". Contact `ghum-inquiry@google.com`. No licence statement in the folder; the folder holds `README.md`, `release.md` (changelog: "V1.0 [June 15, 2020] GHUM & GHUML model is released."), `figures/` and `inference/`. | https://github.com/google-research/google-research/tree/master/ghum | verified |
| The request form ("Google 3D Human Models Request Form") | Asks for a university or academic institution, an affiliation type (student, postdoc, research scientist, professor), an "Academic email address (must be affiliated with a University or Research Institute)", and the "Legal representative for license signature (Mandatory)" of the institution. Intro: "Please fill out this form for code and model repository access." | the form linked from the README | verified |
| The repository-wide licence | `google-research` is Apache-2.0 at the top level. But the GHUM folder holds no code or weights to cover, and the models come through the form, so the top-level licence does not tell us the model terms. | https://github.com/google-research/google-research/blob/master/LICENSE | verified (top-level text); the inference is mine |
| The GHUM licence agreement itself | Not published anywhere I could reach. | n/a | **unverified** |

The form's wording ("academic", "license signature") shows that GHUM itself is released under a **signed, research-oriented agreement**. I could not read its text. It may be non-commercial. It may restrict derived models. It may say nothing about them. I do not know.

### Do GHUM's conditions flow to the Blendshape V2 weights?

Four points. The first three are facts from the sources, the fourth is a reading that the Director has ruled on (Q1).

1. **We are not a party to the GHUM agreement and never touch GHUM.** We never request, receive or use GHUM. Our files contain a trained network (a small MLP-Mixer, 146 landmarks in, 52 coefficients out), not the GHUM mesh, its shape spaces or its code. **Verified** from the card and the files' own description.
2. **Google is the licensor of both.** The card authors list Google staff, including two GHUM authors (A. Zanfir, E. G. Bazavan). The card itself says how the network was trained, so Google released it knowing the recipe. A licensor can license its own work on terms different from another licence it offers; a GHUM licensee's duties bind that licensee.
3. **The Apache-2.0 grant is explicit and has no GHUM exception.** The card says "LICENSED UNDER Apache License, Version 2.0" with no carve-out, and the maintainer says "the models (unless otherwise noted) are licensed under Apache2". Apache-2.0 section 3 gives a "perpetual, worldwide, non-exclusive, no-charge, royalty-free, irrevocable" patent licence from "each Contributor"; that also covers Google's own patents.
4. **My reading: a network trained on GHUM-fitted data is not GHUM, and the GHUM agreement binds its signers.** Weights learned from data are not a copy of the template mesh. The residual risk is: (a) the GHUM agreement may say that anything trained with GHUM must stay research-only, and a court may treat the Apache grant as overriding that for Google's own release or may not; (b) Google could not give away rights it did not hold, for example a third party's rights in the scan subjects' data. For (a), Google is the one who would be bound and the one who published, so the chance a third party can assert it against a downstream user looks small. The Director ruled on this point (Q1): the Apache-2.0 grant stands on its own.

### What the card does not tell us

- **The scan subjects' consent.** The BlazeFace card says its images were "consented". The Blendshape card says samples were captured "in a controlled environment in the lab" but does not say what the subjects agreed to. The GHUM page says the data came from 60 people. This is a **provenance question, not a licence question**, and it is the same kind of item the register already flags for ICT-FaceKit.
- **Whether any identifiable face data is stored in the weights.** The card says the model does "not store any unique face representation". Self-declared; not independently checked.

### Recommendation for question 1

**Cleared.** Ship the Blendshape V2 network under the Apache-2.0 label, keep the card as the licence record, and say in `MODELS.md` that the network was trained by Google using the GHUM mesh (section 7, Q1). If Google ever changes its licence statement, replace this one network (section 6). It is the smallest of the three (955,312 bytes as TFLite) and the only one that mentions GHUM.

## 5. Question 2: Google's download terms

### What binds whom

| Document | What it says | URL | Date | Mark |
|---|---|---|---|---|
| Google APIs Terms of Service | Opens: "By accessing or using our APIs, you are agreeing to the terms below." Defines APIs as "Google's APIs, other developer services, and associated software (collectively, "APIs")." Section 2e: "Some of the software required by or included in our APIs may be offered under an open source license." and the open source licence governs "to the limited extent the open source software license expressly supersedes the Terms". Section 10a: continued use "constitutes your acceptance of the modified Terms". The page says nothing about downloading public files or models. | https://developers.google.com/terms | Last modified 2021-11-09 | verified |
| MediaPipe APIs Terms of Service | "Use of MediaPipe Solution APIs is subject to the Google APIs Terms of Service." The APIs "send metrics about the performance and utilization of the APIs in your app to Google" (input media is not sent). The APIs "will contact Google servers from time to time in order to receive things like bug fixes, updated models". "You are responsible for obtaining informed consent from your app users." Nothing on commercial use or suspension on this page. | https://developers.google.com/edge/mediapipe/legal/tos | Last modified 2026-04-07 | verified (the page's text, via a summarising fetch; an earlier audit read the same page) |
| Google Terms of Service | "by accessing or using our services (whether you're signed in to a Google account or not)" you agree; some services "include software that's offered under open source license terms", and those licences can override parts of the terms. | https://policies.google.com/terms | Effective 2026-07-30 | verified |
| Google Cloud Terms of Service | "entered into by Google and the entity or person agreeing to these terms ("Customer")", effective "when Customer clicks to accept or otherwise agrees to it". This is the contract of the people who *run* the bucket. I read the first 100,000 of 145,933 characters; the part defining "Services" was not in what I read. | https://cloud.google.com/terms | no date on the text I read | verified for the quoted lines; the rest unread |
| Google Cloud docs, "Make data public" | A public object is readable by `allUsers`, so "anyone who knows an object's URI" can fetch it while it stays public. | https://docs.cloud.google.com/storage/docs/access-control/making-data-public | n/a | secondary (a search snippet) |
| The bucket and object headers | Plain `GET`/`HEAD` with no sign-in returns the file; `cache-control: public, max-age=3600`; `access-control-allow-origin: *`; no `Content-Disposition`, no licence header, no custom metadata. Object generation `1683136941916318`, ETag `b0e7274907a1644404fef66b28dd6d85`, last modified 2023-05-03. | the bundle URL in section 2 | 2026-10-15 | verified |
| Apache-2.0, section 4 | We may "reproduce and distribute copies of the Work or Derivative Works" provided we give recipients "a copy of this License", mark changed files, keep notices, and, if the Work has a NOTICE file, include its attributions. The bundle has no NOTICE file as far as the audit found. | https://www.apache.org/licenses/LICENSE-2.0.txt | n/a | verified |

### What this settles

- **The bucket owner's contract is with the bucket owner's customer (Google itself here), not with the people downloading.** The Cloud Terms define the Customer as the party that "agrees to these terms". An anonymous `GET` of a public object has no click, no sign-in, and no agreement step. **Verified for the headers and the Customer definition; the legal consequence is my reading.**
- **The Google APIs Terms are triggered by "accessing or using our APIs".** Google defines APIs to include "associated software". A model file in a bucket is not named. Whether a court would count a model file as part of "associated software" is the open point. The terms' own open-source clause (2e) says an open-source licence wins where it "expressly supersedes". Apache-2.0 does say what we may do with the weights, but it does not name the Terms, so "expressly supersedes" is arguable either way.
- **The MediaPipe ToS speaks about using the MediaPipe Solution APIs (the SDK) in an app.** We do not call that SDK in the shipped product. We run our own ONNX Runtime on converted files, so we do not trigger its telemetry sentence, its server-contact sentence, or its "informed consent" sentence. **Verified** that the shipped path uses `ort`, not MediaPipe (`PROVENANCE.md`, register).
- **Today the fetch happens at build time, but on every clean build, not once.** `tools/models/mediapipe-face.sh` downloads the bundle into its `--out` folder unless a file with the pinned SHA-256 is already there, and the `models` job in `.github/workflows/runtime-litert.yml` passes a fresh `$RUNNER_TEMP/face`, so CI contacts Google on every run. Users of the shipped zip never contact Google's bucket. The script pins the SHA-256 and size and refuses a mismatch, so a changed bucket fails the build rather than shipping something else, but a removed object would break clean builds. The terms analysis below therefore covers every build-time fetch. Making release builds read from our own archive is follow-up task 6. **Verified** (script and workflow).

### What self-hosting changes

| If we ... | Effect |
|---|---|
| ... keep fetching from `storage.googleapis.com` at install or run time | Each user fetches from Google. We would be the party that "causes" the access, and the open point above applies to every user. Not recommended. |
| ... fetch at build time (ideally once, into our archive), then ship our own ONNX conversions | The Google APIs Terms question applies to us, as a build-time fetcher of public files (CI and any clean build today; only the first archive fetch once follow-up task 6 is done). The recipients receive Apache-2.0 files from us, with no Google service involved. This is the plan. |
| ... also archive the `.task`, the cards and their hashes (the register already asks for it) | We can show what the licence statement was on the day we took the file, which matters because the cards could change. The `Last-Modified` and generation values tie the archive to one object. |

### Recommendation for question 2

**Cleared.** Download only at build time, ship only the conversions, the unchanged metadata file and notices, and never load models from Google at run time. A bare download of a public file does not bind us to the Google APIs Terms (section 7, Q3). The strongest facts for us are (a) the files are public with no sign-in, (b) Apache-2.0 on each card, (c) Google's own terms let an open-source licence override them, and (d) the shipped product never uses a Google service.

## 6. Fallbacks, if Google withdraws or changes its licence statement

This is a contingency, not a gate. A change would affect 

| Option | What changes | Terms | Cost and risk | Mark |
|---|---|---|---|---|
| **A. Retrain a blendshape network ourselves** (preferred fallback) | Replace `face_blendshapes.onnx` with our own landmarks-to-52-coefficients network, trained on landmarks from our own face rig. The architecture is small (MLP-Mixer, 146 points). The input contract (146 of the 478 landmarks) stays. | Our own weights. Training data would come from our own synthetic renders. Inputs must be clean: FLAME 2023 Open is CC-BY-4.0 per the licence audit; ICT-FaceKit is `unverified` (scan consent) in the register and would need clearing first. | One training task. Quality bar is the current network: we already have a numeric comparison harness (`tools/models/convert_mediapipe_face.py`). Risk: landmark noise differs between rigs and real webcams; needs the augmentation the card describes. | plan, unverified until built |
| **B. Replace the whole chain** with another permissive tracker | New detector, landmarks and expression heads | OpenSeeFace is **treated as NO** in the audit (training data non-commercial). I found no other fully-cleared candidate in the register. | Largest cost; loses 52-blendshape parity | no candidate cleared |
| **C. Ask Google** (optional; not required for the ruling above) | Write to `ghum-inquiry@google.com` and the MediaPipe maintainers for a written statement that the Blendshape V2 weights are Apache-2.0 without GHUM conditions | A statement from the licensor is better evidence than any reading of mine | Needs a reply; the issue tracker shows only a one-line answer so far | action, not a model |

Order of work if needed: C first, then A. B only if A fails its numeric gate.

## 7. The seven questions and their settled answers

These were the questions drafted for a legal review. The Director settled each with the owner; no legal review is needed. One line per question:

1. **GHUM flow-down.** Google owns both GHUM and the network, and published the weights under Apache-2.0. That grant stands on its own, and GHUM's research terms bind GHUM's signatories, not recipients of the weights. `MODELS.md` says the network was trained by Google using the GHUM mesh.
2. **Chain of title.** The model card's licence line is the grant. Keep the record we already keep: the cards, hashes, object generation numbers and the MediaPipe maintainer's sentence (mediapipe issue 6355). The unchanged `.binarypb` ships under the same bundle's licence. No written confirmation is needed.
3. **Bare downloads.** A public, unauthenticated object fetch does not make us a party to Google's API terms. We redistribute our own copies under Apache-2.0 anyway.
4. **ONNX conversions** are derivative works, which Apache-2.0 section 4 allows with the notice and change statement `MODELS.md` already carries.
5. **Training-data consent** is the licensor's responsibility. We record Google's own wording from the cards (section 3).
6. **Intended use.** Apache-2.0 has no field-of-use limits. Our docs may repeat Google's out-of-scope uses as guidance, not as terms.
7. **Revocation.** Apache-2.0 grants are perpetual and irrevocable (section 2 of the licence). We archive the versions we took. The patent clause only ends a grant for someone who sues over it.

## 8. What this PR changes in the register

`docs/licensing/deps/mediapipe-face-landmarker.toml`: the `condition` now records the settled answer and the archive duties. The status stays `conditions`. The sister row for the ONNX conversions (`mediapipe-face-onnx.toml`) says "GHUM licence and Google ToS with legal review"; it is outside this issue's fence, so I left it. It should be reworded in the same way in a follow-up.

## 9. Remaining items (none blocks shipping)

| Item | What settles it | Who |
|---|---|---|
| Text of the GHUM agreement | Not needed for the ruling; a copy from Google (`ghum-inquiry@google.com`) would only add evidence | optional |
| Source of the "synthetic data" behind Face Mesh z-coordinates | The Face Mesh authors; not in any public document found | low priority |
| Whether the cards or objects have changed since 2023-05 | Re-hash on each release; compare to the table in section 3 | release checklist |

## 10. Follow-up tasks

1. **Task (docs):** reword `docs/licensing/deps/mediapipe-face-onnx.toml` `condition` like the landmarker row, and add the GHUM sentence to `dist/notices/MODELS.md` ("The Blendshape V2 network was trained by Google on samples fitted to Google's GHUM mesh; no GHUM data is included").
2. **Task (ops):** archive the three card PDFs with these hashes, and the `.task` object metadata (generation, ETag, `Last-Modified`), next to the model assets; add the hash check to the release checklist.
3. **Task (outreach, optional):** ask the MediaPipe maintainers to publish a LICENSE/NOTICE in the bundle, and `ghum-inquiry@google.com` for the licence statement for models trained on GHUM.
4. **Task (tracking, fallback A, only if Google changes its licence statement):** scope a self-trained blendshape network on cleared data, with the numeric gate against the current network.
5. **Task (build):** make release builds take the `.task` from our own archive (hash-checked) and keep the Google fetch to the archive step; today every clean build and the CI `models` job fetch from Google.
6. **Task (research, only if fallback A starts):** clear ICT-FaceKit's scan consent, since it is the likely data source for fallback A.
