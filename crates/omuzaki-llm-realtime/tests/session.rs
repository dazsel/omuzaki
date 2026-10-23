//! The Realtime session against a loopback WebSocket server playing the provider: the handshake and its key, the
//! microphone's audio on the wire, a turn's events, the loop's requests, private turns and errors.
// Test helpers may unwrap: a panic there is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use omuzaki_audio_api::ring::{Producer, TapFlags, ring};
use omuzaki_audio_api::tap::DropPolicy;
use omuzaki_llm_realtime::{OpenAiRealtime, RealtimeConfig};
use omuzaki_performer_api::duplex::{Duplex, DuplexError, DuplexEvent, DuplexUsage};
use omuzaki_secrets_api::value::SecretValue;
use serde_json::{Value, json};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{Message, WebSocket};

const KEY: &str = "sk-test-not-a-real-key-0123456789";
const WAIT: Duration = Duration::from_secs(5);

/// What the fake provider saw in the handshake.
#[derive(Debug, Default)]
struct Seen {
    path: String,
    auth: Option<String>,
}

type Ws = WebSocket<TcpStream>;

/// A one-connection provider on this machine: `refuse` answers the handshake 401; otherwise `play` drives the socket.
fn provider(refuse: bool, play: impl FnOnce(&mut Ws) + Send + 'static) -> (String, JoinHandle<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}/v1/realtime", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (tcp, _) = listener.accept().unwrap();
        tcp.set_read_timeout(Some(WAIT)).unwrap();
        tcp.set_nodelay(true).unwrap();
        let mut seen = Seen::default();
        // tungstenite's own callback signature.
        #[allow(clippy::result_large_err)]
        let callback = |req: &Request, res: Response| -> Result<Response, ErrorResponse> {
            seen.path = req.uri().to_string();
            seen.auth = req.headers().get("authorization").and_then(|v| v.to_str().ok()).map(str::to_owned);
            if refuse {
                let mut refusal = ErrorResponse::new(Some("no".to_owned()));
                *refusal.status_mut() = tungstenite::http::StatusCode::UNAUTHORIZED;
                return Err(refusal);
            }
            Ok(res)
        };
        match tungstenite::accept_hdr(tcp, callback) {
            Ok(mut ws) => play(&mut ws),
            Err(_) => assert!(refuse),
        }
        seen
    });
    (url, handle)
}

/// Reads until the client closes the session.
fn until_closed(ws: &mut Ws) {
    loop {
        match ws.read() {
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

/// The next client event of `kind`, skipping the others.
fn next(ws: &mut Ws, kind: &str) -> Value {
    loop {
        match ws.read().unwrap() {
            Message::Text(t) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                if v["type"] == kind {
                    return v;
                }
            }
            Message::Close(_) => panic!("closed while waiting for {kind}"),
            _ => {}
        }
    }
}

fn say(ws: &mut Ws, event: &Value) {
    ws.send(Message::text(event.to_string())).unwrap();
}

/// Polls until `n` events came or the wait ends.
fn events(client: &mut OpenAiRealtime, n: usize) -> Vec<DuplexEvent> {
    let start = Instant::now();
    let mut out = Vec::new();
    while out.len() < n && start.elapsed() < WAIT {
        out.extend(client.poll());
        std::thread::yield_now();
    }
    out
}

fn config(url: &str) -> RealtimeConfig {
    RealtimeConfig::new("You are Mochi.").endpoint(url).voice("marin")
}

fn key() -> SecretValue {
    SecretValue::new(KEY)
}

#[test]
fn the_key_goes_in_the_handshake_and_the_session_is_set_up_first() {
    let (url, server) = provider(false, |ws| {
        let update = next(ws, "session.update");
        assert_eq!(update["session"]["instructions"], "You are Mochi.");
        assert_eq!(update["session"]["audio"]["output"]["voice"], "marin");
        assert_eq!(update["session"]["audio"]["input"]["format"]["rate"], 24_000);
        let td = &update["session"]["audio"]["input"]["turn_detection"];
        assert_eq!((td["create_response"].as_bool(), td["interrupt_response"].as_bool()), (Some(false), Some(false)));
    });
    let client = OpenAiRealtime::connect(&config(&url), Some(&key()), None).unwrap();
    let seen = server.join().unwrap();
    drop(client);
    assert_eq!(seen.auth.as_deref(), Some(format!("Bearer {KEY}").as_str()));
    assert_eq!(seen.path, "/v1/realtime?model=gpt-realtime");
}

#[test]
fn a_refused_key_says_so_without_the_key() {
    let (url, server) = provider(true, |_| {});
    let error = OpenAiRealtime::connect(&config(&url), Some(&key()), None).unwrap_err();
    server.join().unwrap();
    let DuplexError::Provider(message) = error else { panic!("{error:?}") };
    assert!(message.contains("refused the key"), "{message}");
    assert!(!message.contains(KEY));
}

#[test]
fn plain_websockets_to_another_machine_are_refused() {
    let error = OpenAiRealtime::connect(&config("ws://example.com/v1/realtime"), Some(&key()), None).unwrap_err();
    assert!(matches!(error, DuplexError::Provider(m) if m.contains("wss://")));
}

fn tone(n: usize) -> Vec<f32> {
    #[allow(clippy::cast_precision_loss)]
    (0..n).map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5).collect()
}

fn wire_samples(append: &Value) -> usize {
    STANDARD.decode(append["audio"].as_str().unwrap()).unwrap().len() / 2
}

#[test]
fn the_microphone_reaches_the_wire_at_24_khz_as_it_is_captured() {
    let (producer, consumer) = ring(48_000, DropPolicy::DropNewest, 0);
    let (pushed_tx, pushed_rx) = mpsc::channel::<Instant>();
    let (url, server) = provider(false, move |ws| {
        next(ws, "session.update");
        let pushed = pushed_rx.recv().unwrap();
        // 200 ms at 48 kHz is 4800 samples on the wire.
        let mut samples = 0;
        let mut first = None;
        while samples < 4_800 {
            let append = next(ws, "input_audio_buffer.append");
            first.get_or_insert_with(|| pushed.elapsed());
            samples += wire_samples(&append);
        }
        assert_eq!(samples, 4_800);
        let first = first.unwrap();
        println!("microphone to wire: {:.2} ms", first.as_secs_f64() * 1000.0);
        assert!(first < Duration::from_millis(100), "{first:?}");
    });
    let client = OpenAiRealtime::connect(&config(&url), Some(&key()), Some(consumer)).unwrap();
    let mut producer: Producer = producer;
    pushed_tx.send(Instant::now()).unwrap();
    assert_eq!(producer.push(&tone(9_600)), 9_600);
    server.join().unwrap();
    drop(client);
}

#[test]
fn a_turn_streams_back_as_duplex_events_in_order() {
    let (url, server) = provider(false, |ws| {
        next(ws, "session.update");
        let audio = STANDARD.encode([0u8, 64, 0, 192]);
        for event in [
            json!({"type": "session.created"}),
            json!({"type": "input_audio_buffer.speech_started", "item_id": "u1"}),
            json!({"type": "input_audio_buffer.speech_stopped", "item_id": "u1"}),
            json!({"type": "conversation.item.input_audio_transcription.completed", "item_id": "u1", "transcript": "Hi Mochi"}),
            json!({"type": "response.created", "response": {"id": "r1"}}),
            json!({"type": "response.output_audio.delta", "response_id": "r1", "item_id": "i1", "delta": audio}),
            json!({"type": "response.output_audio_transcript.delta", "response_id": "r1", "delta": "Hello!"}),
            json!({"type": "response.done", "response": {"id": "r1", "status": "completed", "usage": {
                "input_token_details": {"audio_tokens": 50, "text_tokens": 400, "cached_tokens": 0},
                "output_token_details": {"audio_tokens": 20, "text_tokens": 5}}}}),
        ] {
            say(ws, &event);
        }
        until_closed(ws);
    });
    let mut client = OpenAiRealtime::connect(&config(&url), Some(&key()), None).unwrap();
    let got = events(&mut client, 7);
    assert_eq!(
        got,
        [
            DuplexEvent::SpeechStarted,
            DuplexEvent::SpeechStopped,
            DuplexEvent::heard("Hi Mochi"),
            DuplexEvent::reply_started("r1"),
            DuplexEvent::reply_audio("r1", "i1", vec![0.5, -0.5], 24_000),
            DuplexEvent::reply_text("r1", "Hello!"),
            DuplexEvent::reply_done("r1", false, Some(DuplexUsage::new(50, 400, 0, 20, 5))),
        ]
    );
    drop(client);
    server.join().unwrap();
}

#[test]
fn the_loops_requests_reach_the_provider() {
    let (url, server) = provider(false, |ws| {
        next(ws, "session.update");
        let create = next(ws, "response.create");
        assert_eq!(create["response"]["instructions"], "Say: hi");
        assert_eq!(next(ws, "response.create").get("response"), None);
        assert_eq!(next(ws, "response.cancel")["response_id"], "r1");
        let truncate = next(ws, "conversation.item.truncate");
        assert_eq!((truncate["item_id"].as_str(), truncate["audio_end_ms"].as_u64()), (Some("i1"), Some(1_234)));
        let auto = next(ws, "session.update");
        assert_eq!(auto["session"]["audio"]["input"]["turn_detection"]["create_response"], true);
        until_closed(ws);
    });
    let mut client = OpenAiRealtime::connect(&config(&url), Some(&key()), None).unwrap();
    client.reply(Some("Say: hi")).unwrap();
    client.reply(None).unwrap();
    client.cancel("r1", Some("i1"), 1_234).unwrap();
    client.auto_reply(true).unwrap();
    client.close();
    assert_eq!(client.reply(None), Err(DuplexError::Closed));
    server.join().unwrap();
}

#[test]
fn a_line_spoken_in_cohost_only_is_heard_as_private() {
    let (producer, consumer) = ring(48_000, DropPolicy::DropNewest, 0);
    let (sent_tx, sent_rx) = mpsc::channel::<()>();
    let (url, server) = provider(false, move |ws| {
        next(ws, "session.update");
        sent_rx.recv().unwrap();
        next(ws, "input_audio_buffer.append");
        for event in [
            json!({"type": "input_audio_buffer.speech_started", "item_id": "u1"}),
            json!({"type": "input_audio_buffer.speech_stopped", "item_id": "u1"}),
            json!({"type": "conversation.item.input_audio_transcription.completed", "item_id": "u1", "transcript": "between us"}),
        ] {
            say(ws, &event);
        }
        until_closed(ws);
    });
    let mut client = OpenAiRealtime::connect(&config(&url), Some(&key()), Some(consumer)).unwrap();
    let mut producer: Producer = producer;
    producer.set_flags(TapFlags { muted: false, private: true });
    producer.push(&tone(4_800));
    sent_tx.send(()).unwrap();
    let got = events(&mut client, 3);
    assert_eq!(got.last(), Some(&DuplexEvent::heard_private("between us")));
    drop(client);
    server.join().unwrap();
}

#[test]
fn errors_reach_the_loop_and_a_fatal_one_ends_the_session() {
    let (url, server) = provider(false, |ws| {
        next(ws, "session.update");
        for event in [
            json!({"type": "error", "error": {"code": "response_cancel_not_active", "message": "nothing to cancel"}}),
            json!({"type": "error", "error": {"code": "invalid_value", "message": "bad voice"}}),
            json!({"type": "error", "error": {"code": "invalid_api_key", "message": "Incorrect API key provided: sk-abcdef1234"}}),
        ] {
            say(ws, &event);
        }
        let _ = ws.read();
    });
    let mut client = OpenAiRealtime::connect(&config(&url), Some(&key()), None).unwrap();
    let got = events(&mut client, 2);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(matches!(&got[0], DuplexEvent::Error { message, fatal: false, .. } if message.contains("bad voice")));
    let DuplexEvent::Error { message, fatal: true, .. } = &got[1] else { panic!("{got:?}") };
    assert!(!message.contains("sk-abcdef1234"), "{message}");
    assert_eq!(client.reply(None), Err(DuplexError::Closed));
    server.join().unwrap();
}

#[test]
fn reply_audio_reaches_the_loop_within_a_few_milliseconds_of_a_round_trip() {
    const DELTAS: usize = 50;
    let (url, server) = provider(false, |ws| {
        next(ws, "session.update");
        // 100 ms of audio per delta, as the provider streams it; each one answers the client's request.
        let audio = STANDARD.encode(vec![0u8; 4_800]);
        for _ in 0..DELTAS {
            next(ws, "response.create");
            say(
                ws,
                &json!({"type": "response.output_audio.delta", "response_id": "r1", "item_id": "i1", "delta": audio}),
            );
        }
    });
    let mut client = OpenAiRealtime::connect(&config(&url), Some(&key()), None).unwrap();
    let mut latencies = Vec::new();
    for _ in 0..DELTAS {
        let asked = Instant::now();
        client.reply(None).unwrap();
        let got = events(&mut client, 1);
        assert!(matches!(got.first(), Some(DuplexEvent::ReplyAudio { pcm, .. }) if pcm.len() == 2_400), "{got:?}");
        latencies.push(asked.elapsed().as_secs_f64() * 1000.0);
    }
    server.join().unwrap();
    latencies.sort_by(f64::total_cmp);
    let (p50, p90) = (latencies[DELTAS / 2], latencies[DELTAS * 9 / 10]);
    println!(
        "request to reply audio in the loop, round trip through the session thread: p50 {p50:.2} ms, p90 {p90:.2} ms"
    );
    assert!(p90 < 50.0, "p90 {p90} ms");
}
