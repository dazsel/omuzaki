//! Injection latency against `VTube Studio` (#727): a square wave on one custom parameter at 60 Hz, printing p50, p95
//! and max of (a) the inject request's round trip and (b) inject to read-back through `ParameterValueRequest`, as JSON.
//!
//! `cargo run -p omuzaki-vts --features testing --example vts_latency -- --mock [--seconds 60]` runs against the
//! loopback mock; `-- --port 8001` against a running VTS on this computer (VTS shows its popup once per run).

use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_vts::clock::SystemClock;
use omuzaki_vts::limits::{CustomParamName, ParamValue};
use omuzaki_vts::protocol::{
    AuthenticationRequest, AuthenticationTokenResponse, Envelope, InjectMode, InjectParameterDataRequest,
    InjectedValue, Message, ParameterCreationRequest, ParameterValueRequest,
};
use omuzaki_vts::testing::{MockConfig, MockVts};
use serde::Serialize;
use serde_json::{Value, json};
use tungstenite::protocol::WebSocket;

type Ws = WebSocket<TcpStream>;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let seconds: u64 = flag("--seconds").map_or(Ok(60), |s| s.parse())?;
    let mock = if args.iter().any(|a| a == "--mock") {
        Some(MockVts::start(&MockConfig { clock: Arc::new(SystemClock::new()), ..MockConfig::default() })?)
    } else {
        None
    };
    let port = match &mock {
        Some(mock) => mock.port(),
        None => flag("--port").map_or(Ok(8001), |p| p.parse())?,
    };
    let stream = TcpStream::connect(("127.0.0.1", port))?;
    let (mut ws, _) = tungstenite::client(format!("ws://127.0.0.1:{port}"), stream)?;
    let mut n = 0u64;
    let token: AuthenticationTokenResponse =
        serde_json::from_value(call(&mut ws, &mut n, omuzaki_vts::protocol::AuthenticationTokenRequest::omuzaki())?)?;
    call(&mut ws, &mut n, AuthenticationRequest::omuzaki(token.authentication_token))?;
    let name = "OmuzakiLatency";
    call(
        &mut ws,
        &mut n,
        ParameterCreationRequest {
            parameter_name: CustomParamName::new(name)?,
            explanation: Some("Omuzaki's latency probe".into()),
            min: ParamValue::new(0.0)?,
            max: ParamValue::new(1.0)?,
            default_value: ParamValue::new(0.0)?,
        },
    )?;
    let (mut round_trip, mut read_back) = (Vec::new(), Vec::new());
    let period = Duration::from_nanos(1_000_000_000 / 60);
    let start = Instant::now();
    for frame in 0..seconds * 60 {
        let value = if (frame / 30) % 2 == 0 { 0.0 } else { 1.0 };
        let sent = Instant::now();
        let inject = InjectParameterDataRequest {
            face_found: true,
            mode: InjectMode::Set,
            parameter_values: vec![InjectedValue { id: name.into(), value: ParamValue::new(value)?, weight: None }],
        };
        call(&mut ws, &mut n, inject)?;
        round_trip.push(sent.elapsed());
        if frame % 30 == 0 {
            // The first read-back that shows the new value.
            loop {
                let answer = call(&mut ws, &mut n, ParameterValueRequest { name: name.into() })?;
                if (answer["value"].as_f64().unwrap_or(f64::NAN) - value).abs() < 1e-9 {
                    read_back.push(sent.elapsed());
                    break;
                }
                if sent.elapsed() > Duration::from_secs(1) {
                    break;
                }
            }
        }
        let next = start + period * u32::try_from(frame + 1)?;
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    let report = json!({
        "target": if mock.is_some() { "mock" } else { "vts" },
        "frames": round_trip.len(),
        "round_trip_ms": stats(&mut round_trip),
        "inject_to_read_back_ms": stats(&mut read_back),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// Sends one request and returns its answer's `data`, or VTS's error as an error.
fn call<T: Message + Serialize>(ws: &mut Ws, n: &mut u64, data: T) -> Result<Value, Box<dyn std::error::Error>> {
    *n += 1;
    let id = format!("latency-{n}");
    ws.send(tungstenite::Message::text(serde_json::to_string(&Envelope::new(id.clone(), data))?))?;
    loop {
        let message = ws.read()?;
        let Ok(text) = message.to_text() else { continue };
        let envelope: Envelope<Value> = serde_json::from_str(text)?;
        if envelope.request_id == id {
            if envelope.is_error() {
                return Err(format!("VTube Studio refused: {}", envelope.data).into());
            }
            return Ok(envelope.data);
        }
    }
}

fn stats(samples: &mut [Duration]) -> Value {
    if samples.is_empty() {
        return Value::Null;
    }
    samples.sort();
    let at = |q: f64| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
        let i = ((samples.len() - 1) as f64 * q).round() as usize;
        samples[i].as_secs_f64() * 1000.0
    };
    json!({ "p50": at(0.5), "p95": at(0.95), "max": at(1.0) })
}
