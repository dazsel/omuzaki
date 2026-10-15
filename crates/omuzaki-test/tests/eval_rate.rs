//! The rating page's server driven with raw HTTP, and `eval agreement` through the command line.
#![allow(clippy::unwrap_used)]

mod common;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use omuzaki_test::eval::history::{self, HISTORY_FILE};
use omuzaki_test::eval::rate::sample::Item;
use omuzaki_test::eval::rate::server::{self, Ended, Session};
use omuzaki_test::eval::rate::{self, Answer};
use serde_json::{Value, json};

const TOKEN: &str = "0123456789abcdef";

fn item(task: &str, attempt: u32, questions: &[&str]) -> Item {
    Item {
        id: format!("2026-10-13T1000Z-nightly/{task}/{attempt}"),
        run: "2026-10-13T1000Z-nightly".to_owned(),
        task: task.to_owned(),
        attempt,
        prompt: format!("Make it <b>{task}</b>"),
        questions: questions.iter().map(|q| (*q).to_owned()).collect(),
    }
}

/// A reply: status, headers as text and the body.
struct Reply {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

fn request(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Reply {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut lines = vec![format!("{method} {path} HTTP/1.1")];
    if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("host")) {
        lines.push(format!("Host: 127.0.0.1:{port}"));
    }
    lines.extend(headers.iter().map(|(name, value)| format!("{name}: {value}")));
    lines.push(format!("Content-Length: {}", body.len()));
    let text = format!("{}\r\n\r\n{body}", lines.join("\r\n"));
    stream.write_all(text.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    Reply { status, head, body: raw[split + 4..].to_vec() }
}

fn rate_body(item: &str, question: &str, answer: &str) -> String {
    json!({"item": item, "question": question, "answer": answer}).to_string()
}

fn post_rate(port: u16, body: &str) -> Reply {
    request(port, "POST", "/api/rate", &[("X-Rating-Token", TOKEN)], body)
}

/// Serves two items in a thread; returns the port, the thread and where the files are.
fn start(dir: &Path) -> (u16, thread::JoinHandle<Ended>, Vec<Item>) {
    let items = vec![item("a.one", 1, &["Round?", "Blue?"]), item("b.two", 2, &["Tall?"])];
    // Item 0 has a "before" picture and no "after"; item 1 has none.
    let kept = dir.join("runs/2026-10-13T1000Z-nightly/a.one/1");
    fs::create_dir_all(&kept).unwrap();
    fs::write(kept.join("before.png"), b"\x89PNG not really").unwrap();
    let session = Session {
        items: items.clone(),
        ratings: dir.join("ratings/review-1.jsonl"),
        runs: dir.join("runs"),
        rater: "tester".to_owned(),
        token: TOKEN.to_owned(),
        clock: || UNIX_EPOCH + Duration::from_secs(1_790_000_000),
    };
    let listener = server::bind(0).unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || server::serve(&listener, &session).unwrap());
    (port, handle, items)
}

#[test]
fn the_page_lists_the_items_and_serves_what_the_attempt_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (port, handle, _) = start(dir.path());

    let page = request(port, "GET", "/", &[], "");
    assert_eq!(page.status, 200);
    assert!(page.head.contains("text/html") && page.head.contains("Content-Security-Policy"), "{}", page.head);
    assert!(String::from_utf8_lossy(&page.body).contains("<script src=\"/app.js\">"), "no inline script");
    for asset in ["/app.js", "/app.css"] {
        assert_eq!(request(port, "GET", asset, &[], "").status, 200, "{asset}");
    }

    let items = request(port, "GET", "/api/items", &[], "").json();
    assert_eq!(items["token"], TOKEN);
    assert_eq!(items["items"].as_array().unwrap().len(), 2);
    assert_eq!(items["items"][0]["questions"], json!(["Round?", "Blue?"]));
    assert_eq!(items["items"][0]["pictures"], json!([true, false]));
    assert_eq!(items["items"][1]["pictures"], json!([false, false]));
    // The prompt is data: the server hands it over as it is, and the page sets it as text.
    assert_eq!(items["items"][0]["prompt"], "Make it <b>a.one</b>");

    let before = request(port, "GET", "/img/0/before.png", &[], "");
    assert_eq!((before.status, before.body.as_slice()), (200, &b"\x89PNG not really"[..]));
    for missing in [
        "/img/0/after.png",
        "/img/1/before.png",
        "/img/9/before.png",
        "/img/0/../../x.png",
        "/img/0/before",
        "/img/x/before.png",
        "/nothing",
    ] {
        assert_eq!(request(port, "GET", missing, &[], "").status, 404, "{missing}");
    }
    assert_eq!(request(port, "DELETE", "/", &[], "").status, 405);

    let quit = request(port, "POST", "/api/quit", &[("X-Rating-Token", TOKEN)], "");
    assert_eq!(quit.status, 200);
    assert_eq!(handle.join().unwrap(), Ended::Quit);
}

#[test]
fn a_rating_needs_the_token_the_right_host_and_origin_and_a_known_question() {
    let dir = tempfile::tempdir().unwrap();
    let (port, handle, items) = start(dir.path());
    let good = rate_body(&items[0].id, "Round?", "yes");

    assert_eq!(request(port, "POST", "/api/rate", &[], &good).status, 403, "no token");
    assert_eq!(request(port, "POST", "/api/rate", &[("X-Rating-Token", "wrong")], &good).status, 403, "wrong token");
    assert_eq!(request(port, "GET", "/api/items", &[("Host", "evil.example")], "").status, 403, "another host");
    assert_eq!(request(port, "GET", "/", &[("Host", &format!("evil.example:{port}"))], "").status, 403);
    let foreign =
        request(port, "POST", "/api/rate", &[("X-Rating-Token", TOKEN), ("Origin", "https://evil.example")], &good);
    assert_eq!(foreign.status, 403, "another origin");
    let local_origin = format!("http://localhost:{port}");
    assert_eq!(
        request(
            port,
            "POST",
            "/api/rate",
            &[("X-Rating-Token", TOKEN), ("Origin", &local_origin), ("Host", &format!("localhost:{port}"))],
            &rate_body(&items[0].id, "Round?", "no")
        )
        .status,
        200
    );

    for (body, why) in [
        (rate_body("nope/x/1", "Round?", "yes"), "unknown item"),
        (rate_body(&items[0].id, "Tall?", "yes"), "a question of another item"),
        (rate_body(&items[0].id, "Blue?", "maybe"), "an answer that is none"),
        ("not json".to_owned(), "a body that is not JSON"),
        (json!({"item": 1}).to_string(), "fields of the wrong type"),
    ] {
        assert_eq!(post_rate(port, &body).status, 400, "{why}");
    }
    assert_eq!(post_rate(port, &good).status, 409, "a question is answered once, and the first answer stands");
    assert_eq!(request(port, "POST", "/api/quit", &[], "").status, 403, "quitting needs the token too");
    // The item's second question completes it, and only then do its ratings reach the file.
    assert_eq!(post_rate(port, &rate_body(&items[0].id, "Blue?", "yes")).status, 200);

    assert_eq!(request(port, "POST", "/api/quit", &[("X-Rating-Token", TOKEN)], "").status, 200);
    handle.join().unwrap();
    // Only the accepted ratings of the completed item are in the file, one line each.
    let lines = rate::read(&dir.path().join("ratings/review-1.jsonl")).unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!((lines[1].question.as_str(), lines[1].answer), ("Blue?", Answer::Yes));
    assert_eq!(
        (lines[0].question.as_str(), lines[0].answer, lines[0].rater.as_str()),
        ("Round?", Answer::No, "tester")
    );
}

#[test]
fn an_item_left_half_rated_leaves_no_rating_so_it_is_asked_about_again_in_full() {
    let dir = tempfile::tempdir().unwrap();
    let (port, handle, items) = start(dir.path());
    assert_eq!(post_rate(port, &rate_body(&items[0].id, "Round?", "yes")).status, 200);
    assert_eq!(request(port, "POST", "/api/quit", &[("X-Rating-Token", TOKEN)], "").status, 200);
    assert_eq!(handle.join().unwrap(), Ended::Quit);
    assert_eq!(rate::read(&dir.path().join("ratings/review-1.jsonl")).unwrap().len(), 0);
}

#[test]
fn a_rating_that_cannot_be_written_can_be_sent_again_and_is_written_once() {
    let dir = tempfile::tempdir().unwrap();
    // The ratings file is a folder, so nothing can be appended until it is removed.
    let file = dir.path().join("ratings/review-1.jsonl");
    fs::create_dir_all(&file).unwrap();
    let (port, handle, items) = start(dir.path());
    assert_eq!(post_rate(port, &rate_body(&items[0].id, "Round?", "yes")).status, 200);
    assert_eq!(post_rate(port, &rate_body(&items[0].id, "Blue?", "no")).status, 500);
    fs::remove_dir(&file).unwrap();
    let again = post_rate(port, &rate_body(&items[0].id, "Blue?", "no"));
    assert_eq!((again.status, again.json()["remaining"].clone()), (200, json!(1)));
    assert_eq!(post_rate(port, &rate_body(&items[1].id, "Tall?", "yes")).status, 200);
    assert_eq!(handle.join().unwrap(), Ended::Done);
    let ratings = rate::read(&file).unwrap();
    let seen: Vec<&str> = ratings.iter().map(|r| r.question.as_str()).collect();
    assert_eq!(seen, ["Round?", "Blue?", "Tall?"], "each answer once");
}

#[test]
fn a_task_that_asks_one_question_twice_cannot_be_rated() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session {
        items: vec![item("a.one", 1, &["Round?", "Round?"])],
        ratings: dir.path().join("ratings/review-1.jsonl"),
        runs: dir.path().join("runs"),
        rater: "tester".to_owned(),
        token: TOKEN.to_owned(),
        clock: || UNIX_EPOCH + Duration::from_secs(1_790_000_000),
    };
    let listener = server::bind(0).unwrap();
    let error = server::serve(&listener, &session).unwrap_err().to_string();
    assert!(error.contains("same question twice"), "{error}");
}

#[test]
fn answering_every_question_writes_the_ratings_and_ends_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let (port, handle, items) = start(dir.path());
    let answers = [(0, "Round?", "yes", 2), (0, "Blue?", "unsure", 1), (1, "Tall?", "no", 0)];
    for (index, question, answer, remaining) in answers {
        let reply = post_rate(port, &rate_body(&items[index].id, question, answer));
        assert_eq!(reply.status, 200);
        assert_eq!(reply.json()["remaining"], remaining);
        assert_eq!(reply.json()["done"], remaining == 0);
    }
    assert_eq!(handle.join().unwrap(), Ended::Done);
    let text = fs::read_to_string(dir.path().join("ratings/review-1.jsonl")).unwrap();
    assert_eq!(text.lines().count(), 3);
    let ratings = rate::read(&dir.path().join("ratings/review-1.jsonl")).unwrap();
    let seen: Vec<(&str, Answer, &str)> =
        ratings.iter().map(|r| (r.question.as_str(), r.answer, r.at.as_str())).collect();
    assert_eq!(
        seen,
        [
            ("Round?", Answer::Yes, "2026-09-21T14:13Z"),
            ("Blue?", Answer::Unsure, "2026-09-21T14:13Z"),
            ("Tall?", Answer::No, "2026-09-21T14:13Z")
        ]
    );
    assert_eq!(ratings[0].item, items[0].id);
    assert_eq!((ratings[2].task.as_str(), ratings[2].attempt), ("b.two", 2));
}

/// A task with a judge check and questions for people, and a history of reference attempts the judge scored.
fn agreement_world(dir: &Path) -> (String, String) {
    let ws = dir.join("ws");
    fs::create_dir_all(ws.join("crates/c/evals")).unwrap();
    fs::write(
        ws.join("crates/c/evals/oval.toml"),
        "id = 'face.oval'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'Make her face more oval.'\nfixture = 'f'\nscopes = ['read']\n\
         rubric_for_people = ['Is the face more oval?']\n\
         [[check]]\nid = 'hard'\nkind = 'doc.exists'\npath = 'a'\n\
         [[check]]\nid = 'reads-oval'\nhard = false\nkind = 'judge'\nrubric = ['Is the face more oval?']\n",
    )
    .unwrap();
    let status = dir.join("status");
    fs::create_dir_all(&status).unwrap();
    let mut lines = Vec::new();
    for attempt in 1..=11 {
        // The judge says yes to attempts 1-10 and no to the 11th.
        let score = if attempt <= 10 { 1.0 } else { 0.0 };
        lines.push(
            json!({
                "v": 1, "run": "2026-10-13T1000Z-nightly", "commit": "abc1234", "family": "task", "driver": "reference",
                "model": "m", "effort": null, "task": "face.oval", "task_version": 1, "task_hash": "b3:x", "feature": "QG-03",
                "attempt": attempt, "outcome": "pass", "reason": null,
                "checks": [{"id": "hard", "hard": true, "passed": true}, {"id": "reads-oval", "hard": false, "score": score}],
                "turns": 3, "tool_calls": 5, "tool_errors": 0, "retries": 0,
                "usage": {"input": 1, "output": 1, "cache_read": 1, "cache_write": 0}, "usd_micros": 10, "wall_ms": 1,
                "tools_hash": "b3:t", "prompt_hash": "b3:p", "harness": "0", "transcript": "evals/runs/x/face.oval/1/"
            })
            .to_string(),
        );
    }
    fs::write(status.join(HISTORY_FILE), lines.join("\n") + "\n").unwrap();
    // The raters: yes on attempts 1-9, no on 10 (the judge said yes: a miss), no on 11 (a hit), unsure on 12 (no record).
    let ratings = dir.join("evals/ratings");
    fs::create_dir_all(&ratings).unwrap();
    let rating = |attempt: u32, answer: &str| {
        json!({"item": format!("2026-10-13T1000Z-nightly/face.oval/{attempt}"), "task": "face.oval", "attempt": attempt,
               "question": "Is the face more oval?", "answer": answer, "rater": "r", "at": "2026-10-13T11:00Z"})
        .to_string()
    };
    let mut text: Vec<String> = (1..=9).map(|n| rating(n, "yes")).collect();
    text.extend([rating(10, "no"), rating(11, "no"), rating(12, "unsure")]);
    fs::write(ratings.join("review-1.jsonl"), text.join("\n") + "\n").unwrap();
    (common::arg(&ws).to_owned(), common::arg(dir).to_owned())
}

#[test]
fn eval_agreement_refuses_a_question_the_raters_were_not_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, base) = agreement_world(dir.path());
    let envs = [("OMUZAKI_EVAL_OUT", format!("{base}/evals")), ("OMUZAKI_EVAL_STATUS", format!("{base}/status"))];
    let envs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let args = [
        "eval",
        "agreement",
        "--judge",
        "reads-oval",
        "--set",
        "review-1",
        "--question",
        "Is the face moer oval?",
        "--root",
        &ws,
    ];
    let refused = common::run(&args, &envs);
    assert_eq!(common::code(&refused), 2, "{}", common::text(&refused.stderr));
    assert!(common::text(&refused.stderr).contains("rubric_for_people"), "{}", common::text(&refused.stderr));
    let history = fs::read_to_string(Path::new(&base).join("status").join(HISTORY_FILE)).unwrap();
    assert!(!history.contains("\"family\":\"judge\""), "nothing was recorded");
}

#[test]
fn eval_agreement_measures_a_judge_against_the_ratings_and_records_it() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, base) = agreement_world(dir.path());
    let envs = [("OMUZAKI_EVAL_OUT", format!("{base}/evals")), ("OMUZAKI_EVAL_STATUS", format!("{base}/status"))];
    let envs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let run = |extra: &[&str]| {
        common::run(&[&["eval", "agreement", "--set", "review-1", "--root", &ws], extra].concat(), &envs)
    };

    let done = run(&["--judge", "reads-oval"]);
    assert_eq!(common::code(&done), 0, "{}", common::text(&done.stderr));
    let text = common::text(&done.stdout);
    // Eleven clear-cut ratings: ten agree (attempts 1-9 and 11, but not attempt 10), and the unsure one is left out.
    assert!(text.contains("agrees with the raters on 10 of 11 clear-cut items (90.9%)"), "{text}");
    assert!(text.contains("1 unsure left out") && text.contains("these numbers do not"), "{text}");

    let attempts = history::read_attempts(&dir.path().join("status").join(HISTORY_FILE)).unwrap();
    assert_eq!(attempts.len(), 11, "the judge record is its own family, not an attempt");
    let last: Value = serde_json::from_str(
        fs::read_to_string(dir.path().join("status").join(HISTORY_FILE)).unwrap().lines().last().unwrap(),
    )
    .unwrap();
    assert_eq!(
        (last["family"].clone(), last["judge"].clone(), last["items"].clone(), last["agree"].clone()),
        ("judge".into(), "reads-oval".into(), 11.into(), 10.into())
    );
    assert_eq!((last["agreement"].clone(), last["meets_threshold"].clone()), ((10.0_f64 / 11.0).into(), false.into()));

    // Not recorded with --no-record; a check that does not name its question needs --question; an unknown check is refused.
    let before = fs::read_to_string(dir.path().join("status").join(HISTORY_FILE)).unwrap().lines().count();
    assert_eq!(common::code(&run(&["--judge", "reads-oval", "--no-record"])), 0);
    assert_eq!(fs::read_to_string(dir.path().join("status").join(HISTORY_FILE)).unwrap().lines().count(), before);
    let hard = run(&["--judge", "hard"]);
    assert_eq!(common::code(&hard), 2);
    assert!(common::text(&hard.stderr).contains("--question"), "{}", common::text(&hard.stderr));
    assert_eq!(common::code(&run(&["--judge", "no-such-check"])), 2);
}
