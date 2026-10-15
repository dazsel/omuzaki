//! The pinned list of model files: where each is fetched from and the SHA-256 and size it must have.
//!
//! Nothing here is downloaded by this crate, and no model is committed (they are 2 MB to 574 MB). The shared verified
//! downloader fetches an entry, checks size then hash, and caches it; [`crate::SpeechHost::open`] is then given the
//! cached paths. URLs name an exact commit or release tag, so a file never changes under its hash. The hashes are the
//! ones in `models/speech/SHA256SUMS.txt` in the assets folder (and, for Whisper, the `lfs.sha256` Hugging Face lists for that
//! revision); `tests/asr_real.rs` checks the catalogue against that file when `OMUZAKI_SPEECH_MODELS` is set.
//!
//! # Licences (register rows in `docs/licensing/register.md`)
//! - Silero VAD: MIT, Copyright (c) 2020-present Silero Team.
//! - Whisper weights (`OpenAI` Whisper, converted to ggml by whisper.cpp): MIT, Copyright (c) 2022 `OpenAI`; the
//!   Hugging Face repo `ggerganov/whisper.cpp` is tagged `license: mit`.
//! - ONNX Runtime: MIT, Copyright (c) Microsoft Corporation.

/// What kind of file an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelKind {
    /// A voice-activity model (ONNX).
    Vad,
    /// A speech-recognition model (ggml).
    Asr,
    /// A native runtime library archive.
    Runtime,
}

/// One downloadable file.
#[derive(Debug, PartialEq, Eq)]
pub struct ModelSpec {
    /// Stable identifier, used in error messages and by the downloader.
    pub id: &'static str,
    /// What it is.
    pub kind: ModelKind,
    /// The file name it is cached under.
    pub file_name: &'static str,
    /// Where it is fetched from (an immutable commit or tag URL).
    pub url: &'static str,
    /// The lower-case hex SHA-256 the bytes must have.
    pub sha256: &'static str,
    /// The exact size in bytes; a download of any other length is refused before it is hashed.
    pub size: u64,
    /// SPDX licence of the file.
    pub licence: &'static str,
}

/// Id of [`SILERO_VAD`].
pub const SILERO_VAD_ID: &str = "silero-vad-v6.2.3";
/// Id of [`ORT_RUNTIME`].
pub const ORT_RUNTIME_ID: &str = "onnxruntime-linux-x64-1.28.3";
/// Id of the default Whisper model, [`WHISPER_BASE_EN`].
pub const WHISPER_BASE_EN_ID: &str = "whisper-base.en";

/// The Hugging Face revision every Whisper file is pinned to (a macro because `concat!` takes only literals).
macro_rules! whisper_base_url {
    () => {
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1"
    };
}

/// Silero VAD v6.2.3 (16 kHz and 8 kHz in one graph), from the tagged commit.
pub static SILERO_VAD: ModelSpec = ModelSpec {
    id: SILERO_VAD_ID,
    kind: ModelKind::Vad,
    file_name: "silero_vad.onnx",
    url: "https://raw.githubusercontent.com/snakers4/silero-vad/5cd7945676eb32225748052e2e6a0580e4686a08/src/silero_vad/data/silero_vad.onnx",
    sha256: "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3",
    size: 2_327_524,
    licence: "MIT",
};

/// Microsoft's official ONNX Runtime 1.28.3 CPU build for Linux x64 (the archive; `lib/libonnxruntime.so` inside).
pub static ORT_RUNTIME: ModelSpec = ModelSpec {
    id: ORT_RUNTIME_ID,
    kind: ModelKind::Runtime,
    file_name: "onnxruntime-linux-x64-1.28.3.tgz",
    url: "https://github.com/microsoft/onnxruntime/releases/download/v1.28.3/onnxruntime-linux-x64-1.28.3.tgz",
    sha256: "db14e4863bd37893fc59729d986ab2a0d043d10b7d44da1913c4982b7e3d009c",
    size: 9_130_098,
    licence: "MIT",
};

macro_rules! whisper {
    ($name:ident, $id:literal, $file:literal, $size:literal, $sha:literal) => {
        #[doc = concat!("Whisper `", $id, "` as a ggml file.")]
        pub static $name: ModelSpec = ModelSpec {
            id: $id,
            kind: ModelKind::Asr,
            file_name: $file,
            url: concat!(whisper_base_url!(), "/", $file),
            sha256: $sha,
            size: $size,
            licence: "MIT",
        };
    };
}

whisper!(
    WHISPER_TINY_EN,
    "whisper-tiny.en",
    "ggml-tiny.en.bin",
    77_704_715,
    "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f"
);
whisper!(
    WHISPER_BASE_EN,
    "whisper-base.en",
    "ggml-base.en.bin",
    147_964_211,
    "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002"
);
whisper!(
    WHISPER_BASE_EN_Q5_1,
    "whisper-base.en-q5_1",
    "ggml-base.en-q5_1.bin",
    59_721_011,
    "4baf70dd0d7c4247ba2b81fafd9c01005ac77c2f9ef064e00dcf195d0e2fdd2f"
);
whisper!(
    WHISPER_SMALL_EN,
    "whisper-small.en",
    "ggml-small.en.bin",
    487_614_201,
    "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d"
);
whisper!(
    WHISPER_SMALL_EN_Q5_1,
    "whisper-small.en-q5_1",
    "ggml-small.en-q5_1.bin",
    190_098_681,
    "bfdff4894dcb76bbf647d56263ea2a96645423f1669176f4844a1bf8e478ad30"
);
whisper!(
    WHISPER_LARGE_V3_TURBO_Q5_0,
    "whisper-large-v3-turbo-q5_0",
    "ggml-large-v3-turbo-q5_0.bin",
    574_041_195,
    "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2"
);

/// Every entry, in no particular order.
pub static CATALOG: &[&ModelSpec] = &[
    &SILERO_VAD,
    &ORT_RUNTIME,
    &WHISPER_TINY_EN,
    &WHISPER_BASE_EN,
    &WHISPER_BASE_EN_Q5_1,
    &WHISPER_SMALL_EN,
    &WHISPER_SMALL_EN_Q5_1,
    &WHISPER_LARGE_V3_TURBO_Q5_0,
];

/// The entry with this id.
#[must_use]
pub fn find(id: &str) -> Option<&'static ModelSpec> {
    CATALOG.iter().copied().find(|spec| spec.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_well_formed_and_unique() {
        for (i, spec) in CATALOG.iter().enumerate() {
            assert_eq!(spec.sha256.len(), 64, "{}", spec.id);
            assert!(spec.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{}", spec.id);
            assert!(spec.url.starts_with("https://") && spec.url.ends_with(spec.file_name), "{}", spec.id);
            assert!(spec.size > 0 && spec.licence == "MIT", "{}", spec.id);
            for other in &CATALOG[i + 1..] {
                assert_ne!((spec.id, spec.file_name, spec.sha256), (other.id, other.file_name, other.sha256));
                assert_ne!(spec.id, other.id);
                assert_ne!(spec.file_name, other.file_name);
            }
        }
    }

    #[test]
    fn ids_used_in_error_messages_exist() {
        for id in [SILERO_VAD_ID, ORT_RUNTIME_ID, WHISPER_BASE_EN_ID] {
            assert!(find(id).is_some(), "{id}");
        }
    }

    #[test]
    fn whisper_urls_pin_one_revision() {
        for spec in CATALOG.iter().filter(|s| s.kind == ModelKind::Asr) {
            assert!(spec.url.starts_with(whisper_base_url!()), "{}", spec.id);
        }
    }
}
