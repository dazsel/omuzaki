//! The Realtime API's events: what the client sends ([`session_update`], [`append`], [`create`], [`cancel`],
//! [`truncate`]) and what it reads back ([`decode`]), under both the GA names (`response.output_audio.delta`) and the
//! beta ones (`response.audio.delta`); and the audio: PCM16 little-endian at 24 kHz, base64 on the wire.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use omuzaki_performer_api::duplex::DuplexUsage;
use serde_json::{Value, json};

/// The wire's sample rate.
pub const WIRE_RATE: u32 = 24_000;

/// How the provider finds the end of the person's turn.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TurnDetection {
    /// A model judges when the person has finished (the provider's `semantic_vad`): fewer replies to a pause.
    Semantic,
    /// Silence after speech (`server_vad`): the fastest reply.
    Silence,
}

/// The session's settings.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Session<'a> {
    pub instructions: &'a str,
    pub voice: &'a str,
    pub transcription: &'a str,
    pub turns: TurnDetection,
    pub auto: bool,
}

/// `session.update`: the persona, the voice, PCM16 both ways, the person's speech transcribed, and turn detection
/// that replies by itself only when `auto`. The provider never cuts its own reply: the performer does, so it can tell
/// the provider how much was heard.
pub(crate) fn session_update(s: &Session<'_>) -> Value {
    let turn_detection = match s.turns {
        TurnDetection::Semantic => {
            json!({"type": "semantic_vad", "create_response": s.auto, "interrupt_response": false})
        }
        TurnDetection::Silence => json!({"type": "server_vad", "create_response": s.auto, "interrupt_response": false}),
    };
    json!({
        "type": "session.update",
        "session": {
            "type": "realtime",
            "instructions": s.instructions,
            "output_modalities": ["audio"],
            "audio": {
                "input": {
                    "format": {"type": "audio/pcm", "rate": WIRE_RATE},
                    "turn_detection": turn_detection,
                    "transcription": {"model": s.transcription},
                },
                "output": {
                    "format": {"type": "audio/pcm", "rate": WIRE_RATE},
                    "voice": s.voice,
                },
            },
        },
    })
}

/// Turns automatic replies on or off, keeping the rest of the session.
pub(crate) fn auto_update(turns: TurnDetection, auto: bool) -> Value {
    let kind = match turns {
        TurnDetection::Semantic => "semantic_vad",
        TurnDetection::Silence => "server_vad",
    };
    json!({
        "type": "session.update",
        "session": {
            "type": "realtime",
            "audio": {"input": {"turn_detection": {"type": kind, "create_response": auto, "interrupt_response": false}}},
        },
    })
}

/// `input_audio_buffer.append` with `pcm` (24 kHz, -1.0 to 1.0).
pub(crate) fn append(pcm: &[f32]) -> Value {
    json!({"type": "input_audio_buffer.append", "audio": STANDARD.encode(pcm16(pcm))})
}

/// `response.create`, with `instructions` for this reply only.
pub(crate) fn create(instructions: Option<&str>) -> Value {
    match instructions {
        Some(i) => json!({"type": "response.create", "response": {"instructions": i}}),
        None => json!({"type": "response.create"}),
    }
}

/// `response.cancel` for `reply`.
pub(crate) fn cancel(reply: &str) -> Value {
    json!({"type": "response.cancel", "response_id": reply})
}

/// `conversation.item.truncate`: only `audio_end_ms` of `item`'s audio was heard.
pub(crate) fn truncate(item: &str, audio_end_ms: u64) -> Value {
    json!({"type": "conversation.item.truncate", "item_id": item, "content_index": 0, "audio_end_ms": audio_end_ms})
}

/// One server event, as the session thread needs it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Server {
    SpeechStarted,
    SpeechStopped {
        item: Option<String>,
    },
    Heard {
        item: Option<String>,
        text: String,
    },
    ReplyStarted {
        reply: String,
    },
    Audio {
        reply: String,
        item: String,
        pcm: Vec<f32>,
    },
    Text {
        reply: String,
        delta: String,
    },
    ReplyDone {
        reply: String,
        cancelled: bool,
        usage: Option<DuplexUsage>,
    },
    Error {
        message: String,
        code: Option<String>,
    },
    /// Anything else (`session.created`, rate limits, item bookkeeping).
    Other,
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn count(v: Option<&Value>, key: &str) -> u64 {
    v.and_then(|v| v.get(key)).and_then(Value::as_u64).unwrap_or(0)
}

/// What a `response.done`'s `usage` bills: audio and text in, cache reads apart, audio and text out.
fn usage(v: &Value) -> Option<DuplexUsage> {
    let usage = v.get("usage").filter(|u| u.is_object())?;
    let input = usage.get("input_token_details");
    let output = usage.get("output_token_details");
    let cached = count(input, "cached_tokens");
    let cached_details = input.and_then(|i| i.get("cached_tokens_details"));
    // The cache reads are counted inside the audio and text inputs; bill them apart.
    let cached_audio = count(cached_details, "audio_tokens");
    let cached_text = count(cached_details, "text_tokens");
    let (cached_audio, cached_text) =
        if cached_audio + cached_text == cached { (cached_audio, cached_text) } else { (0, cached) };
    Some(DuplexUsage::new(
        count(input, "audio_tokens").saturating_sub(cached_audio),
        count(input, "text_tokens").saturating_sub(cached_text),
        cached,
        count(output, "audio_tokens"),
        count(output, "text_tokens"),
    ))
}

/// `message` with anything shaped like a key cut to its prefix (the provider masks keys it quotes, but a message
/// never carries one onward either way).
fn redact(message: &str) -> String {
    message
        .split(' ')
        .map(|word| if word.starts_with("sk-") && word.len() > 6 { "sk-…" } else { word })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Reads one server event.
///
/// # Errors
/// What was wrong with it: not JSON, or a known event missing a field it needs.
pub(crate) fn decode(message: &str) -> Result<Server, String> {
    let v: Value = serde_json::from_str(message).map_err(|e| format!("the provider sent something unreadable: {e}"))?;
    let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
    let missing = |what: &str| format!("the provider's `{kind}` had no {what}");
    Ok(match kind {
        "input_audio_buffer.speech_started" => Server::SpeechStarted,
        "input_audio_buffer.speech_stopped" => Server::SpeechStopped { item: text(&v, "item_id") },
        "conversation.item.input_audio_transcription.completed" => {
            Server::Heard { item: text(&v, "item_id"), text: text(&v, "transcript").unwrap_or_default() }
        }
        "response.created" => Server::ReplyStarted {
            reply: v.get("response").and_then(|r| text(r, "id")).ok_or_else(|| missing("response id"))?,
        },
        "response.output_audio.delta" | "response.audio.delta" => {
            let delta = v.get("delta").and_then(Value::as_str).ok_or_else(|| missing("audio"))?;
            let bytes = STANDARD.decode(delta).map_err(|e| format!("the provider's audio wasn't base64: {e}"))?;
            Server::Audio {
                reply: text(&v, "response_id").ok_or_else(|| missing("response id"))?,
                item: text(&v, "item_id").unwrap_or_default(),
                pcm: from_pcm16(&bytes),
            }
        }
        "response.output_audio_transcript.delta" | "response.audio_transcript.delta" => Server::Text {
            reply: text(&v, "response_id").ok_or_else(|| missing("response id"))?,
            delta: text(&v, "delta").unwrap_or_default(),
        },
        "response.done" => {
            let response = v.get("response").ok_or_else(|| missing("response"))?;
            let status = response.get("status").and_then(Value::as_str).unwrap_or_default();
            Server::ReplyDone {
                reply: text(response, "id").ok_or_else(|| missing("response id"))?,
                cancelled: status == "cancelled",
                usage: usage(response),
            }
        }
        "error" => {
            let error = v.get("error");
            Server::Error {
                message: error
                    .and_then(|e| text(e, "message"))
                    .map_or_else(|| "the provider failed".to_owned(), |m| redact(&m)),
                code: error.and_then(|e| text(e, "code")),
            }
        }
        _ => Server::Other,
    })
}

/// `pcm` as PCM16 little-endian, clipped.
pub(crate) fn pcm16(pcm: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len() * 2);
    for s in pcm {
        // Clamped to the i16 range first.
        #[allow(clippy::cast_possible_truncation)]
        let v = (s.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// PCM16 little-endian as samples; an odd last byte is dropped.
pub(crate) fn from_pcm16(bytes: &[u8]) -> Vec<f32> {
    bytes.as_chunks::<2>().0.iter().map(|b| f32::from(i16::from_le_bytes(*b)) / 32_768.0).collect()
}

/// The half-band low-pass for 48 → 24 kHz: its length, a windowed sinc (Blackman) cut at 12 kHz that passes speech
/// to about 9 kHz flat and stops from about 15 kHz, so nothing folds back into the band speech uses.
const TAPS: usize = 47;

/// The filter's taps, summing to 1.
fn half_band() -> Vec<f32> {
    // The filter is a few dozen taps long: its indices are exact as `u16`.
    let taps = u16::try_from(TAPS).unwrap_or(u16::MAX);
    let centre = f64::from((taps - 1) / 2);
    let span = f64::from(taps - 1);
    let half_pi = std::f64::consts::FRAC_PI_2;
    let taps: Vec<f64> = (0..taps)
        .map(|n| {
            let x = f64::from(n) - centre;
            let sinc = if x == 0.0 { 1.0 } else { (half_pi * x).sin() / (half_pi * x) };
            let t = std::f64::consts::TAU * f64::from(n) / span;
            let blackman = 0.42 - 0.5 * t.cos() + 0.08 * (2.0 * t).cos();
            sinc * blackman
        })
        .collect();
    let gain: f64 = taps.iter().sum();
    // Filter taps of magnitude at most 1.
    #[allow(clippy::cast_possible_truncation)]
    taps.iter().map(|h| (h / gain) as f32).collect()
}

/// Takes the microphone's 48 kHz mono to the wire's 24 kHz, keeping its history across calls.
#[derive(Clone, Debug)]
pub(crate) struct Decimator {
    taps: Vec<f32>,
    history: Vec<f32>,
    /// Whether the next input sample lands on an output.
    phase: bool,
}

impl Decimator {
    pub(crate) fn new() -> Self {
        Self { taps: half_band(), history: vec![0.0; TAPS - 1], phase: true }
    }

    /// Appends the 24 kHz samples for `input` (48 kHz) to `out`.
    pub(crate) fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &x in input {
            self.history.push(x);
            if self.phase {
                let recent = self.history.iter().rev().take(TAPS);
                out.push(recent.zip(self.taps.iter()).map(|(a, h)| a * h).sum());
            }
            self.phase = !self.phase;
            if self.history.len() >= 4_096 {
                let keep = self.history.len() - (TAPS - 1);
                self.history.drain(..keep);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm16_round_trips_and_clips() {
        let pcm = [0.0, 0.5, -0.5, 1.5, -1.5];
        let back = from_pcm16(&pcm16(&pcm));
        assert_eq!(back.len(), 5);
        assert!((back[1] - 0.5).abs() < 1e-3 && (back[2] + 0.5).abs() < 1e-3);
        assert!(back[3] <= 1.0 && back[3] > 0.99 && back[4] >= -1.0 && back[4] < -0.99);
    }

    #[test]
    fn the_half_band_passes_speech_and_stops_what_would_alias() {
        let gain: f32 = half_band().iter().sum();
        assert!((gain - 1.0).abs() < 1e-3, "{gain}");
        let tone = |hz: f32| -> f32 {
            let input: Vec<f32> =
                (0..4_800u16).map(|i| (f32::from(i) * hz * std::f32::consts::TAU / 48_000.0).sin()).collect();
            let mut out = Vec::new();
            Decimator::new().push(&input, &mut out);
            assert_eq!(out.len(), 2_400);
            out[200..].iter().fold(0.0_f32, |m, s| m.max(s.abs()))
        };
        assert!(tone(1_000.0) > 0.98, "speech passes");
        assert!(tone(4_000.0) > 0.98, "speech passes");
        assert!(tone(16_000.0) < 0.01, "16 kHz would fold to 8 kHz; it is stopped");
        assert!(tone(20_000.0) < 0.01, "20 kHz would fold to 4 kHz; it is stopped");
    }

    #[test]
    fn server_events_decode_under_both_names() {
        let audio = STANDARD.encode(pcm16(&[0.25; 4]));
        for kind in ["response.output_audio.delta", "response.audio.delta"] {
            let m = json!({"type": kind, "response_id": "r1", "item_id": "i1", "delta": audio}).to_string();
            let Server::Audio { reply, item, pcm } = decode(&m).unwrap() else { panic!("{m}") };
            assert_eq!((reply.as_str(), item.as_str(), pcm.len()), ("r1", "i1", 4));
        }
        for kind in ["response.output_audio_transcript.delta", "response.audio_transcript.delta"] {
            let m = json!({"type": kind, "response_id": "r1", "delta": "Hi"}).to_string();
            assert_eq!(decode(&m).unwrap(), Server::Text { reply: "r1".into(), delta: "Hi".into() });
        }
        assert_eq!(decode(r#"{"type":"session.created"}"#).unwrap(), Server::Other);
        assert!(decode("not json").is_err());
        assert!(decode(r#"{"type":"response.created","response":{}}"#).is_err());
    }

    #[test]
    fn a_reply_bills_its_cache_reads_apart() {
        let m = json!({
            "type": "response.done",
            "response": {
                "id": "r1",
                "status": "cancelled",
                "usage": {
                    "input_token_details": {
                        "audio_tokens": 500, "text_tokens": 300, "cached_tokens": 400,
                        "cached_tokens_details": {"audio_tokens": 300, "text_tokens": 100},
                    },
                    "output_token_details": {"audio_tokens": 120, "text_tokens": 30},
                },
            },
        })
        .to_string();
        let Server::ReplyDone { reply, cancelled, usage } = decode(&m).unwrap() else { panic!() };
        assert_eq!((reply.as_str(), cancelled), ("r1", true));
        assert_eq!(usage, Some(DuplexUsage::new(200, 200, 400, 120, 30)));
    }

    #[test]
    fn the_session_never_lets_the_provider_cut_its_own_reply() {
        let s = Session {
            instructions: "Be Mochi.",
            voice: "marin",
            transcription: "t",
            turns: TurnDetection::Semantic,
            auto: true,
        };
        let v = session_update(&s);
        let td = &v["session"]["audio"]["input"]["turn_detection"];
        assert_eq!((td["type"].as_str(), td["create_response"].as_bool()), (Some("semantic_vad"), Some(true)));
        assert_eq!(td["interrupt_response"].as_bool(), Some(false));
        assert_eq!(v["session"]["audio"]["output"]["voice"], "marin");
        assert_eq!(
            auto_update(TurnDetection::Silence, false)["session"]["audio"]["input"]["turn_detection"]["create_response"],
            false
        );
    }
}
