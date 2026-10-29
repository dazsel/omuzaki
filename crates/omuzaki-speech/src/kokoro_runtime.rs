//! The `kokoro` runtime: Kokoro-82M (Apache-2.0) as a `tts@1` model, with our own G2P in front of its ONNX graph
//! (`docs/research/2026-10-gpl-free-tts.md`, ADR 0061).
//!
//! A Kokoro model's files, by name:
//! - the graph (the first file, `.onnx`): `input_ids i64[1, n]`, `style f32[1, 256]`, `speed f32[1]` →
//!   `waveform f32[1, samples]` at 24 kHz. It loads through the `onnx` runtime, so it passes the same scan and
//!   provider choice as every other ONNX model;
//! - `tokenizer.json`: the phoneme vocabulary (one character to one id; `$` pads both ends);
//! - `us_gold.json`, and `gb_gold.json` if present: misaki's gold lexicons, which [`crate::g2p`] reads. A voice whose
//!   name starts with `b` (British, in Kokoro's naming) reads with `gb_gold.json` when the model has it;
//! - one `<voice>.bin` per voice: 510 style rows of 256 `f32`s, the row picked by the number of phonemes.
//!
//! Text longer than Kokoro's 510 phonemes is cut at the last sentence or clause break that fits (a space if none)
//! and the pieces' audio joined. `speed` is honoured; `expressiveness` is not (the model has no such control).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use omuzaki_ai_api::{
    DType, Dim, LoadError, LocalFile, ModelSource, Probe, RunError, Runtime, RuntimeId, Session, SessionInfo,
    SessionOptions, Signature, TensorBuf, TensorData, TensorDataMut, TensorRef, TensorSpec, find_runtime,
};

use crate::g2p::Lexicon;
use crate::slots::tts::tts_signature;

/// The runtime's id.
pub const KOKORO: &str = "kokoro";

/// Phonemes Kokoro reads at once (512 positions, less the two pads).
pub const MAX_PHONEMES: usize = 510;

/// Floats in one style row.
const STYLE: usize = 256;

/// Style rows in a voice file.
const STYLE_ROWS: usize = 510;

/// The `kokoro` runtime.
#[derive(Default)]
pub struct Kokoro;

omuzaki_ai_api::register_runtime!(Kokoro);

/// The graph's own signature, which the `onnx` runtime loads it as.
#[must_use]
pub fn graph_signature() -> Signature {
    Signature {
        inputs: vec![
            TensorSpec::new("input_ids", DType::I64, [Dim::Fixed(1), Dim::ANY]),
            TensorSpec::new("style", DType::F32, [Dim::Fixed(1), Dim::Fixed(STYLE)]),
            TensorSpec::new("speed", DType::F32, [Dim::Fixed(1)]),
        ],
        outputs: vec![TensorSpec::new("waveform", DType::F32, [Dim::Fixed(1), Dim::ANY])],
    }
}

/// A model's files, sorted by role.
struct Parts<'a> {
    graph: &'a LocalFile,
    tokenizer: &'a Path,
    us: &'a Path,
    gb: Option<&'a Path>,
    voices: Vec<(&'a str, &'a Path)>,
}

impl<'a> Parts<'a> {
    fn of(source: &'a ModelSource<'_>) -> Result<Self, LoadError> {
        let graph = source
            .files
            .first()
            .filter(|f| Path::new(&f.name).extension().is_some_and(|e| e.eq_ignore_ascii_case("onnx")))
            .ok_or_else(|| LoadError::MissingFile("the Kokoro graph (.onnx, first)".into()))?;
        let need = |name: &str| source.file(name).ok_or_else(|| LoadError::MissingFile(name.into()));
        let voices: Vec<(&str, &Path)> = source
            .files
            .iter()
            .filter_map(|f| f.name.strip_suffix(".bin").map(|voice| (voice, f.path.as_path())))
            .collect();
        if voices.is_empty() {
            return Err(LoadError::MissingFile("a voice (<voice>.bin)".into()));
        }
        Ok(Self {
            graph,
            tokenizer: need("tokenizer.json")?,
            us: need("us_gold.json")?,
            gb: source.file("gb_gold.json"),
            voices,
        })
    }

    /// A source of the graph alone, for the `onnx` runtime.
    fn graph_source<'s>(&'s self, source: &'s ModelSource<'_>) -> ModelSource<'s> {
        ModelSource {
            model: source.model,
            files: std::slice::from_ref(self.graph),
            stages: &[],
            natives: source.natives,
        }
    }
}

fn onnx() -> Result<Box<dyn Runtime>, LoadError> {
    let id = RuntimeId::new("onnx").unwrap_or_else(|_| unreachable!("a literal slug"));
    find_runtime(&id).ok_or_else(|| LoadError::Runtime("the onnx runtime isn't linked into this app".into()))
}

#[allow(
    clippy::disallowed_methods,
    reason = "a model file the registry fetched and checked by hash, not a file an op names; the fence is for ops"
)]
fn read(path: &Path) -> Result<Vec<u8>, LoadError> {
    std::fs::read(path).map_err(|e| LoadError::Format(format!("can't read {}: {e}", path.display())))
}

/// The phoneme vocabulary from `tokenizer.json`: `model.vocab`, one character to one id.
///
/// # Errors
/// [`LoadError::Format`] when it isn't Kokoro's.
pub fn read_vocabulary(bytes: &[u8]) -> Result<HashMap<char, i64>, LoadError> {
    let bad = |why: &str| LoadError::Format(format!("tokenizer.json: {why}"));
    let json: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| bad(&e.to_string()))?;
    let vocab =
        json.pointer("/model/vocab").and_then(serde_json::Value::as_object).ok_or_else(|| bad("no model.vocab"))?;
    let mut out = HashMap::with_capacity(vocab.len());
    for (token, id) in vocab {
        let mut chars = token.chars();
        let (Some(c), None, Some(id)) = (chars.next(), chars.next(), id.as_i64()) else {
            return Err(bad(&format!("{token:?} isn't one character with a number")));
        };
        out.insert(c, id);
    }
    if out.get(&'$') != Some(&0) || out.len() < 50 {
        return Err(bad("not Kokoro's phoneme vocabulary ($ = 0 and at least 50 symbols)"));
    }
    Ok(out)
}

/// A voice file's style rows.
///
/// # Errors
/// [`LoadError::Format`] when it isn't 510 rows of 256 floats.
pub fn read_voice(bytes: &[u8]) -> Result<Vec<f32>, LoadError> {
    if bytes.len() != STYLE_ROWS * STYLE * 4 {
        return Err(LoadError::Format(format!(
            "a Kokoro voice is {} bytes, not {}",
            STYLE_ROWS * STYLE * 4,
            bytes.len()
        )));
    }
    let rows: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    if rows.iter().any(|v| !v.is_finite()) {
        return Err(LoadError::Format("a Kokoro voice has values that aren't numbers".into()));
    }
    Ok(rows)
}

/// Cuts `ids` into pieces of at most `max`, each ending at the last sentence break that fits, else the last clause
/// break, else the last space, else at `max`; `sentence`, `clause` and `space` are the ids of those symbols.
#[must_use]
pub fn split_ids<'a>(ids: &'a [i64], max: usize, sentence: &[i64], clause: &[i64], space: i64) -> Vec<&'a [i64]> {
    let mut pieces = Vec::new();
    let mut rest = ids;
    while rest.len() > max {
        let window = &rest[..max];
        let last = |set: &dyn Fn(i64) -> bool| window.iter().rposition(|&id| set(id)).filter(|&i| i > 0);
        let cut = last(&|id| sentence.contains(&id))
            .or_else(|| last(&|id| clause.contains(&id)))
            .or_else(|| last(&|id| id == space))
            .map_or(max, |i| i + 1);
        let (piece, tail) = rest.split_at(cut);
        pieces.push(piece);
        rest = tail;
        while rest.first() == Some(&space) {
            rest = &rest[1..];
        }
    }
    if !rest.is_empty() {
        pieces.push(rest);
    }
    pieces
}

impl Runtime for Kokoro {
    fn id(&self) -> RuntimeId {
        RuntimeId::new(KOKORO).unwrap_or_else(|_| unreachable!("a literal slug"))
    }

    fn probe(&self, source: &ModelSource<'_>) -> Result<Probe, LoadError> {
        let parts = Parts::of(source)?;
        let graph = onnx()?.probe(&parts.graph_source(source))?;
        graph_signature().accepts(&graph.signature).map_err(|mismatches| {
            let list: Vec<String> = mismatches.iter().map(ToString::to_string).collect();
            LoadError::Format(format!("not a Kokoro graph: {}", list.join("; ")))
        })?;
        read_vocabulary(&read(parts.tokenizer)?)?;
        Ok(Probe { signature: tts_signature(), opset: graph.opset })
    }

    fn load(
        &self,
        source: &ModelSource<'_>,
        slot: &Signature,
        _io: &BTreeMap<String, String>,
        options: &SessionOptions,
    ) -> Result<Box<dyn Session>, LoadError> {
        if slot != &tts_signature() {
            return Err(LoadError::Format("kokoro models only fit tts@1".into()));
        }
        let parts = Parts::of(source)?;
        let vocabulary = read_vocabulary(&read(parts.tokenizer)?)?;
        let lexicon = |path: &Path| {
            Lexicon::from_json(&read(path)?).map_err(|e| LoadError::Format(format!("{}: {e}", path.display())))
        };
        let us = lexicon(parts.us)?;
        let gb = parts.gb.map(lexicon).transpose()?;
        let voices = parts
            .voices
            .iter()
            .map(|(name, path)| Ok(((*name).to_owned(), read_voice(&read(path)?)?)))
            .collect::<Result<Vec<_>, LoadError>>()?;
        let graph = onnx()?.load(&parts.graph_source(source), &graph_signature(), &BTreeMap::new(), options)?;
        let id = |c: char| vocabulary.get(&c).copied();
        let ids_of = |s: &str| s.chars().filter_map(id).collect::<Vec<_>>();
        Ok(Box::new(KokoroSession {
            sentence: ids_of(".!?…"),
            clause: ids_of(",;:—"),
            space: id(' ').ok_or_else(|| LoadError::Format("tokenizer.json has no space".into()))?,
            graph,
            vocabulary,
            us,
            gb,
            voices,
            signature: tts_signature(),
            ids: Vec::new(),
            waveform: TensorBuf::for_outputs(&graph_signature()),
            spoken: String::new(),
        }))
    }
}

/// A loaded Kokoro model.
struct KokoroSession {
    graph: Box<dyn Session>,
    vocabulary: HashMap<char, i64>,
    us: Lexicon,
    gb: Option<Lexicon>,
    /// Name and style rows, in file order: the first is the default.
    voices: Vec<(String, Vec<f32>)>,
    sentence: Vec<i64>,
    clause: Vec<i64>,
    space: i64,
    signature: Signature,
    /// The padded ids of one piece.
    ids: Vec<i64>,
    waveform: Vec<TensorBuf>,
    /// The phonemes of the last run, as spoken: those with a token in the vocabulary.
    spoken: String,
}

impl KokoroSession {
    fn speak(&mut self, text: &str, voice: &str, speed: f32, out: &mut Vec<f32>) -> Result<(), RunError> {
        let index = if voice.is_empty() {
            0
        } else {
            self.voices
                .iter()
                .position(|(name, _)| name == voice)
                .ok_or_else(|| RunError::Failed(format!("this model has no voice {voice:?}")))?
        };
        let lexicon = match &self.gb {
            Some(gb) if self.voices[index].0.starts_with('b') => gb,
            _ => &self.us,
        };
        let phonemes = lexicon.phonemize(text);
        self.spoken.clear();
        self.spoken.extend(phonemes.chars().filter(|c| self.vocabulary.contains_key(c)));
        let ids: Vec<i64> = phonemes.chars().filter_map(|c| self.vocabulary.get(&c).copied()).collect();
        let speed = [if speed.is_finite() { speed.clamp(0.5, 2.0) } else { 1.0 }];
        for piece in split_ids(&ids, MAX_PHONEMES, &self.sentence, &self.clause, self.space) {
            let row = piece.len().min(STYLE_ROWS - 1) * STYLE;
            let style = &self.voices[index].1[row..row + STYLE];
            self.ids.clear();
            self.ids.push(0);
            self.ids.extend_from_slice(piece);
            self.ids.push(0);
            let ids_shape = [1, self.ids.len()];
            let inputs = [
                TensorRef::i64(&self.ids, &ids_shape),
                TensorRef::f32(style, &[1, STYLE]),
                TensorRef::f32(&speed, &[1]),
            ];
            self.graph.run(&inputs, &mut self.waveform)?;
            out.extend_from_slice(self.waveform[0].as_f32().unwrap_or_default());
        }
        Ok(())
    }
}

impl Session for KokoroSession {
    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        self.signature.check_inputs(inputs)?;
        self.signature.check_outputs(outputs)?;
        let (TensorData::U8(text), TensorData::U8(voice), TensorData::F32(speed)) =
            (inputs[0].data, inputs[1].data, inputs[2].data)
        else {
            return Err(RunError::Failed("tts@1 takes UTF-8 text and voice and an f32 speed".into()));
        };
        let utf8 =
            |bytes, what| std::str::from_utf8(bytes).map_err(|_| RunError::Failed(format!("the {what} isn't UTF-8")));
        let (text, voice) = (utf8(text, "text")?, utf8(voice, "voice")?);
        let mut pcm = Vec::new();
        self.speak(text, voice, speed.first().copied().unwrap_or(1.0), &mut pcm)?;
        let problem = |problem| RunError::Mismatch { port: omuzaki_ai_api::Port::Output, index: 0, problem };
        if let TensorDataMut::F32(out) = outputs[0].resize(&[pcm.len()]).map_err(problem)? {
            out.copy_from_slice(&pcm);
        }
        Ok(())
    }

    fn reset(&mut self) {}

    fn info(&self) -> SessionInfo {
        self.graph.info()
    }

    fn phonemes(&self) -> Option<&str> {
        Some(&self.spoken)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_is_cut_at_the_best_break_that_fits() {
        // 1 = '.', 2 = ',', 9 = ' ', other = phonemes.
        let ids: Vec<i64> = [5, 5, 2, 9, 5, 1, 9, 5, 5, 9, 5, 5, 5].to_vec();
        let pieces = split_ids(&ids, 8, &[1], &[2], 9);
        assert_eq!(pieces, [&ids[..6], &ids[7..]], "the sentence break, and the space after it dropped");
        let pieces = split_ids(&ids[..5], 4, &[1], &[2], 9);
        assert_eq!(pieces, [&ids[..3], &ids[4..5]], "a clause break when no sentence ends in reach");
        let no_breaks = [5i64; 10];
        assert_eq!(split_ids(&no_breaks, 4, &[1], &[2], 9).iter().map(|p| p.len()).collect::<Vec<_>>(), [4, 4, 2]);
        assert_eq!(split_ids(&[], 4, &[1], &[2], 9), Vec::<&[i64]>::new());
    }

    #[test]
    fn a_voice_file_must_be_510_rows_of_256_floats() {
        assert_eq!(read_voice(&vec![0u8; STYLE_ROWS * STYLE * 4]).unwrap().len(), STYLE_ROWS * STYLE);
        assert!(read_voice(&[0u8; 1024]).is_err());
        let mut nan = vec![0u8; STYLE_ROWS * STYLE * 4];
        nan[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(read_voice(&nan).is_err());
    }

    #[test]
    fn the_vocabulary_is_one_character_per_id() {
        let mut vocab = serde_json::Map::new();
        vocab.insert("$".into(), 0.into());
        for (i, c) in ('a'..='z').chain('A'..='Z').enumerate() {
            vocab.insert(c.to_string(), (i + 1).into());
        }
        let json = serde_json::json!({ "model": { "vocab": vocab } }).to_string();
        let v = read_vocabulary(json.as_bytes()).unwrap();
        assert_eq!(v[&'$'], 0);
        assert_eq!(v[&'a'], 1);
        assert!(read_vocabulary(br#"{"model":{"vocab":{"$":0,"ab":1}}}"#).is_err());
        assert!(read_vocabulary(b"{}").is_err());
    }
}
