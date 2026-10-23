//! Measures a decision provider on the labelled moments (#2348): latency p50 and p90, and agreement on the face and the
//! gesture, through the gates and the rule floor the co-host uses. Prints the report as JSON.
//!
//! ```text
//! cargo run -p omuzaki-cohost --example decide_eval                      # the rule floor
//! cargo run -p omuzaki-cohost --example decide_eval -- llm http://127.0.0.1:8080/v1 qwen3-0.6b
//! OMUZAKI_DECIDER_KEY=… cargo run -p omuzaki-cohost --example decide_eval -- systemone https://api.typesafe.ai jev-latest
//! ```
//!
//! Arguments: the kind (`rules`, `llm`, `systemone`), the endpoint, the model, and optionally the wait in
//! milliseconds (default 400, the show's; a larger wait measures a slow provider's full latency). A hosted endpoint
//! reads its key from `OMUZAKI_DECIDER_KEY`, and the key is never printed. A hosted provider bills the key's owner.

use std::sync::Arc;
use std::time::Duration;

use omuzaki_llm::OpenAiClient;
use omuzaki_llm::json::JsonClient;
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::provider::{ProviderConfig, ProviderEnv, SecretLabel, SecretRef, SecretStore, SecretValue};
use omuzaki_performer::decide::{LlmDecider, RuleFloor};
use omuzaki_performer::eval::{MOMENTS, parse, run};
use omuzaki_performer::systemone::{self, JsonTransport, SystemOneDecider};
use omuzaki_performer_api::decide::{DecideError, Decider};
use serde_json::Value;

const KEY_VAR: &str = "OMUZAKI_DECIDER_KEY";

struct EnvKey;

impl SecretStore for EnvKey {
    fn get(&self, _: &SecretLabel) -> Option<SecretValue> {
        std::env::var(KEY_VAR).ok().map(SecretValue::new)
    }
}

struct Http(JsonClient);

impl JsonTransport for Http {
    fn post(&self, body: &Value) -> Result<Value, DecideError> {
        self.0.post(systemone::PATH, body).map_err(|e| DecideError::provider(e.to_string()))
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let kind = args.first().map_or("rules", String::as_str);
    let endpoint = args.get(1).cloned().unwrap_or_else(|| "http://127.0.0.1:8080/v1".into());
    let model = args.get(2).cloned();
    let wait = args.get(3).and_then(|ms| ms.parse::<u64>().ok()).map_or(Duration::from_millis(400), Duration::from_millis);
    let key = std::env::var(KEY_VAR).ok().map(SecretValue::new);
    let runtime = omuzaki_llm::runtime::dedicated().map_err(|e| e.to_string())?;

    let mut decider: Box<dyn Decider> = match kind {
        "rules" => Box::new(RuleFloor),
        "llm" => {
            let mut config = ProviderConfig::new("decide-eval", "openai-compatible", endpoint.as_str())
                .with_allow_remote(true)
                .with_key(SecretRef(SecretLabel("decider".into())));
            if let Some(model) = &model {
                config = config.with_model(model.as_str());
            }
            let env = ProviderEnv::new(Arc::new(EnvKey), runtime.clone());
            let client: Arc<dyn LlmClient> =
                Arc::new(OpenAiClient::new(&config, &env).map_err(|e| e.to_string())?);
            let decider = LlmDecider::new(client, "llm");
            let qwen3 = model.as_deref().unwrap_or_default().to_ascii_lowercase().starts_with("qwen3");
            Box::new(if qwen3 { decider.with_note("/no_think") } else { decider })
        }
        "systemone" => {
            let client = JsonClient::new(&endpoint, key, true, &runtime, Duration::from_secs(10))
                .map_err(|e| e.to_string())?;
            let model = model.as_deref().unwrap_or(systemone::DEFAULT_MODEL);
            Box::new(SystemOneDecider::new(Arc::new(Http(client)), model))
        }
        other => return Err(format!("unknown kind `{other}`: rules, llm or systemone")),
    };

    let moments = parse(MOMENTS).map_err(|e| e.to_string())?;
    let report = run(decider.as_mut(), &moments, wait);
    println!("{}", serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?);
    Ok(())
}
