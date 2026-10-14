//! The measure family: `omuzaki://measure/liveliness/..`. Reading one runs `measure.liveliness` and returns its answer.
//!
//! Nothing stores a report. A read is a query: the op runs on the way to the reader, and its answer is the resource.

use omuzaki_ops_api::{Actor, ErrorCode, Grants, Mode, OpError, Request, Runner};
use rmcp::model::{ResourceContents, ResourceTemplate};
use serde_json::{Map, Value, json};

use super::ResourceProvider;

pub(crate) const MEASURE_PREFIX: &str = "omuzaki://measure/";
const LIVELINESS_PREFIX: &str = "omuzaki://measure/liveliness/";
const LIVELINESS_OP: &str = "measure.liveliness";
const JSON: &str = "application/json";

const LIFE_FORM: &str =
    "`omuzaki://measure/liveliness/life/<seed>/<seconds>/<baseline>` (optionally `?personality=<name>`)";
const RECORDING_FORM: &str = "`omuzaki://measure/liveliness/recording/<baseline>/<path>`";

/// Serves `omuzaki://measure/liveliness/..`: reading one runs `measure.liveliness` and returns its answer.
///
/// **Cost.** A read pays for the measurement it names, at the op's own price. The measure itself is at most 1 ms per
/// minute of 60 fps data (acceptance, `a_minute_of_60_fps_data_measures_in_a_millisecond`). A life run adds the
/// life layer's step for each frame it generates, up to 3,600 s (216,000 frames at 60 fps). A recording is read and
/// parsed first, up to 64 MiB. Nothing is cached, so every read pays again.
///
/// **Updates.** Nothing in the document feeds these URIs, and a recording is read from outside it, so no document change
/// touches them. `touched` keeps its default, so an applied op never notifies a measure URI. A subscriber is still
/// notified, like every subscribed URI, on a rollback, an undo, a redo or a missed event window. It reads again to
/// re-measure.
///
/// The provider is advertised whether or not the op is registered, because `templates` has no runner to ask. Without
/// the op, a read is `not_found` ("unknown op").
#[derive(Debug, Default)]
pub struct MeasureProvider;

const LIFE_DESCRIPTION: &str = "Runs the life layer for `seconds` from `seed` (optional `personality`: natural, \
     energetic, calm, shy or sleepy) and judges it against `baseline` (rest, talking or reading). Reading this runs \
     the measurement.";
const RECORDING_DESCRIPTION: &str = "Measures the recorded track at `path`, relative to the project folder, against \
     `baseline`. Reading this runs the measurement.";

impl ResourceProvider for MeasureProvider {
    fn templates(&self) -> Vec<ResourceTemplate> {
        vec![
            ResourceTemplate::new(
                format!("{LIVELINESS_PREFIX}life/{{seed}}/{{seconds}}/{{baseline}}{{?personality}}"),
                "liveliness of a life run",
            )
            .with_title("Liveliness of a life-layer run")
            .with_description(LIFE_DESCRIPTION)
            .with_mime_type(JSON),
            ResourceTemplate::new(
                format!("{LIVELINESS_PREFIX}recording/{{baseline}}/{{+path}}"),
                "liveliness of a recording",
            )
            .with_title("Liveliness of a recorded track")
            .with_description(RECORDING_DESCRIPTION)
            .with_mime_type(JSON),
        ]
    }

    /// Measurements are computed on read, so there is no stored set to list.
    fn list(&self, _runner: &dyn Runner, _grants: &Grants) -> Result<Vec<rmcp::model::Resource>, OpError> {
        Ok(Vec::new())
    }

    fn serves(&self, uri: &str) -> bool {
        uri.starts_with(MEASURE_PREFIX)
    }

    fn read(&self, runner: &dyn Runner, grants: &Grants, uri: &str) -> Result<Vec<ResourceContents>, OpError> {
        let args = args_of(uri)?;
        let outcome = runner.run(request(args.clone(), grants)).map_err(|error| refusal(uri, error))?;
        let answer =
            outcome.plan.output.ok_or_else(|| OpError::internal(format!("{LIVELINESS_OP} returned no answer")))?;
        let body = json!({"op": LIVELINESS_OP, "args": args, "result": answer});
        let text = serde_json::to_string_pretty(&body)
            .map_err(|e| OpError::internal(format!("couldn't encode {uri}: {e}")))?;
        Ok(vec![ResourceContents::text(text, uri).with_mime_type(JSON)])
    }
}

/// The op arguments a URI names. This checks syntax only: ranges and names are the op's to judge.
fn args_of(uri: &str) -> Result<Value, OpError> {
    // Percent-encoding isn't decoded here, so an encoded path would reach the op under the wrong name.
    if uri.contains('%') {
        return Err(unrecognised(
            uri,
            "percent-encoding is not supported; use the `measure_liveliness` tool for such a path",
        ));
    }
    let Some(rest) = uri.strip_prefix(LIVELINESS_PREFIX) else {
        return Err(unrecognised(uri, "only the liveliness measurement is served"));
    };
    if let Some(life) = rest.strip_prefix("life/") {
        life_args(uri, life)
    } else if let Some(recording) = rest.strip_prefix("recording/") {
        recording_args(uri, recording)
    } else {
        Err(unrecognised(uri, "the measurement is neither a life run nor a recording"))
    }
}

/// `<seed>/<seconds>/<baseline>`, then optionally `?personality=<name>` and nothing else.
fn life_args(uri: &str, rest: &str) -> Result<Value, OpError> {
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    let personality = match query {
        None => None,
        Some(query) => match query.strip_prefix("personality=") {
            Some(name) if !name.is_empty() && !name.contains(['&', '=', '?']) => Some(name),
            _ => return Err(unrecognised(uri, "the only query is `?personality=<name>`")),
        },
    };
    let [seed, seconds, baseline] = path.split('/').collect::<Vec<_>>()[..] else {
        return Err(unrecognised(uri, "a life run needs a seed, seconds and a baseline"));
    };
    let seed: u64 = seed.parse().map_err(|_| unrecognised(uri, "the seed is not a whole number"))?;
    let seconds: f64 = seconds
        .parse()
        .ok()
        .filter(|s: &f64| s.is_finite())
        .ok_or_else(|| unrecognised(uri, "the seconds are not a finite number"))?;
    if baseline.is_empty() {
        return Err(unrecognised(uri, "the baseline is missing"));
    }
    let mut life = Map::new();
    life.insert("seed".to_owned(), json!(seed));
    life.insert("seconds".to_owned(), json!(seconds));
    if let Some(name) = personality {
        life.insert("personality".to_owned(), json!(name));
    }
    Ok(json!({"source": {"life": life}, "baseline": baseline}))
}

/// `<baseline>/<path>`: the path is relative to the project folder, so it can't start with `/`.
fn recording_args(uri: &str, rest: &str) -> Result<Value, OpError> {
    let Some((baseline, path)) = rest.split_once('/') else {
        return Err(unrecognised(uri, "a recording needs a baseline and a path"));
    };
    if baseline.is_empty() {
        return Err(unrecognised(uri, "the baseline is missing"));
    }
    if path.is_empty() || path.starts_with('/') {
        return Err(unrecognised(uri, "the path must be non-empty and relative to the project folder"));
    }
    if !path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '-' | '/')) {
        return Err(unrecognised(uri, "the path may use only letters, digits and `. _ ~ - /`"));
    }
    Ok(json!({"source": {"recording": path}, "baseline": baseline}))
}

/// A URI of this family that names no measurement, saying what was wrong and what the two forms are.
fn unrecognised(uri: &str, reason: &str) -> OpError {
    OpError::not_found(format!("{uri} names no measurement: {reason}"))
        .hint(format!("read {LIFE_FORM} or {RECORDING_FORM}"))
}

/// The request that runs the measurement: a query, so it changes nothing and stores nothing. It runs as the system
/// with the caller's grants, so the session can measure only what its own grants reach (the file roots of a recording
/// included).
fn request(args: Value, grants: &Grants) -> Request {
    Request {
        op: LIVELINESS_OP.to_owned(),
        args,
        mode: Mode::Apply,
        actor: Actor::System,
        grants: grants.clone(),
        cause: None,
        tx: None,
        base: None,
    }
}

/// A runner's refusal as a resource error. An invalid argument means the URI names no measurement, so it becomes
/// `not_found` and keeps the op's hint. Every other code is the op's own answer and passes through.
fn refusal(uri: &str, error: OpError) -> OpError {
    if error.code != ErrorCode::Invalid {
        return error;
    }
    let refused = OpError::not_found(format!("{uri} names no measurement: {}", error.message));
    match error.hint {
        Some(hint) => refused.hint(hint),
        None => refused,
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::Scope;
    use serde_json::json;

    use super::*;

    fn life(seed: u64, seconds: f64, baseline: &str) -> Value {
        json!({"source": {"life": {"seed": seed, "seconds": seconds}}, "baseline": baseline})
    }

    #[test]
    fn a_life_run_is_its_seed_seconds_and_baseline() {
        assert_eq!(args_of("omuzaki://measure/liveliness/life/1/20/rest").unwrap(), life(1, 20.0, "rest"));
        assert_eq!(args_of("omuzaki://measure/liveliness/life/7/0.5/talking").unwrap(), life(7, 0.5, "talking"));
    }

    #[test]
    fn a_personality_is_passed_through_only_when_given() {
        let shy = args_of("omuzaki://measure/liveliness/life/1/20/rest?personality=shy").unwrap();
        assert_eq!(shy["source"]["life"]["personality"], "shy");
        assert!(
            args_of("omuzaki://measure/liveliness/life/1/20/rest").unwrap()["source"]["life"]
                .get("personality")
                .is_none()
        );
    }

    #[test]
    fn a_recording_is_its_path_and_baseline() {
        assert_eq!(
            args_of("omuzaki://measure/liveliness/recording/talking/takes/a-1.json").unwrap(),
            json!({"source": {"recording": "takes/a-1.json"}, "baseline": "talking"})
        );
    }

    #[test]
    fn anything_else_names_no_measurement_and_says_the_two_forms() {
        let bad = [
            "omuzaki://measure/liveliness",
            "omuzaki://measure/likeness/x",
            "omuzaki://measure/liveliness/life/x/20/rest",
            "omuzaki://measure/liveliness/life/1/abc/rest",
            "omuzaki://measure/liveliness/life/1/20",
            "omuzaki://measure/liveliness/life/1/20/rest?color=red",
            "omuzaki://measure/liveliness/life/1/20/rest?personality=",
            "omuzaki://measure/liveliness/life/1/inf/rest",
            "omuzaki://measure/liveliness/life/1/20/",
            "omuzaki://measure/liveliness/recording/rest/a%20b.json",
            "omuzaki://measure/liveliness/recording/rest/",
            "omuzaki://measure/liveliness/recording/rest//etc/passwd",
            "omuzaki://measure/liveliness/recording/rest/a b.json",
            "omuzaki://measure/liveliness/recording/rest",
        ];
        for uri in bad {
            let error = args_of(uri).unwrap_err();
            assert_eq!(error.code, ErrorCode::NotFound, "{uri}");
            assert!(error.message.contains(uri), "{uri}: {}", error.message);
            assert!(
                error.hint.as_deref().is_some_and(|h| h.contains("life/<seed>") && h.contains("recording/<baseline>")),
                "{uri}"
            );
        }
    }

    #[test]
    fn a_request_is_the_op_as_the_system_with_the_callers_grants() {
        let grants = Grants::of([Scope::Read]);
        let req = request(life(1, 20.0, "rest"), &grants);
        assert_eq!(req.op, LIVELINESS_OP);
        assert_eq!(req.actor, Actor::System);
        assert_eq!(req.mode, Mode::Apply);
        assert!(req.grants.allows(Scope::Read));
        assert!(!req.grants.allows(Scope::Publish));
        assert!(req.cause.is_none() && req.tx.is_none() && req.base.is_none());
    }

    #[test]
    fn an_invalid_argument_is_not_found_and_everything_else_passes_through() {
        let uri = "omuzaki://measure/liveliness/life/1/20/dancing";
        let invalid = OpError::invalid("baseline", "unknown baseline `dancing`").hint("use rest, talking or reading");
        let refused = refusal(uri, invalid);
        assert_eq!(refused.code, ErrorCode::NotFound);
        assert!(refused.message.contains("names no measurement") && refused.message.contains("dancing"));
        assert_eq!(refused.hint.as_deref(), Some("use rest, talking or reading"));

        assert_eq!(refusal(uri, OpError::not_found("gone")).code, ErrorCode::NotFound);
        assert_eq!(refusal(uri, OpError::not_found("gone")).message, "gone");
        assert_eq!(refusal(uri, OpError::denied("no read")).code, ErrorCode::Denied);
    }
}
