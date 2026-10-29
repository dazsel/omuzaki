# Lip-sync evaluation on real speech (#2351)

Scores the character's mouth against Kokoro-82M's own speech, with the spans its duration predictor gave each phoneme
as the ground truth (the truth open-annie scored against). The test is
`crates/omuzaki-track-voice/tests/lipsync_kokoro.rs`; it skips unless `OMUZAKI_LIPSYNC_CORPUS` names a corpus.

Everything here is dev-only. The model files are the pinned ones of `crates/omuzaki-speech/models/kokoro-82m.model.toml`
(Apache-2.0; `docs/licensing/deps/kokoro-82m-weights.toml`), fetched into a scratch folder and checked against their
SHA-256, never committed. The script needs `onnx`, `onnxruntime` and `numpy` (already in the licence register as dev
tooling). The sentences are the project's own.

```sh
K=/tmp/kokoro          # model.onnx, tokenizer.json, us_gold.json and a voice (af_heart.bin), from the pinned URLs
C=/tmp/lipsync-corpus
mkdir -p $C
cargo run -q -p omuzaki-speech --example phonemize -- $K/us_gold.json < tools/lipsync-eval/sentences.txt > $C/phonemes.txt
python3 tools/lipsync-eval/kokoro_truth.py --model $K --voice af_heart --phonemes $C/phonemes.txt --out $C/af_heart
OMUZAKI_LIPSYNC_CORPUS=$C/af_heart cargo test -p omuzaki-track-voice --test lipsync_kokoro -- --nocapture
```

About a minute in all on a laptop CPU. The test prints the four mouths' scores twice: against the durations as Kokoro
gives them, and against the sound (the durations run about 60 ms behind Kokoro's audio; the test measures the lag
from the audio's loudness against the truth's vowels and moves the truth back by it). It fails if the phone mouth
falls under open-annie's bar: 40.9% closure recall and 34.6% five-vowel accuracy.
