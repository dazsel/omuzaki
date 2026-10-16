//! A differential test of the writer against the loader: for every layout the TOML parser accepts for a key, whatever
//! the loader reads, the writer can change and remove, and nothing else in the file moves.

use std::collections::BTreeMap;

use proptest::prelude::*;
use serde_json::{Value, json};

use super::*;

/// The keys a layout may hold; each one found in a document is edited in turn.
const TARGETS: [&str; 4] = ["ui.mig", "ui.theme", "ui.k", "ui.list"];

fn pick<T: Clone + std::fmt::Debug + 'static>(items: &[T]) -> BoxedStrategy<T> {
    proptest::sample::select(items.to_vec()).boxed()
}

/// `ui = { ... }` with every placement of whitespace, newlines, comments and commas the parser takes.
fn inline_layout() -> impl Strategy<Value = String> {
    let entries = Just(vec!["mig = { percent = 125 }", "theme = \"dark\"", "k = 2"]).prop_shuffle();
    let dotted = Just(vec!["mig.percent = 125", "theme = \"dark\"", "k = 2"]).prop_shuffle();
    let scalar = Just(vec!["mig = 125", "theme = \"dark\"", "k = 2"]).prop_shuffle();
    let separators = proptest::collection::vec(
        pick(&[",", " ,", ", ", ",\n", "\n,", ",\n  ", " # c\n,", ", # c\n", "\n  ,\n", " , # c\n  "]),
        2,
    );
    (
        prop_oneof![entries, dotted, scalar],
        separators,
        pick(&["", ",", ", # t\n", ",\n", "\n"]),
        pick(&["{ ", "{", "{\n  ", "{ # o\n  "]),
        pick(&[" }", "}", "\n}", "\n# z\n}"]),
        pick(&["", " # end"]),
    )
        .prop_map(|(entries, separators, trailing, open, close, end)| {
            let mut body = String::new();
            for (i, entry) in entries.iter().enumerate() {
                body.push_str(entry);
                if let Some(separator) = separators.get(i) {
                    body.push_str(separator);
                }
            }
            format!("ui = {open}{body}{trailing}{close}{end}\n")
        })
}

/// The table, dotted-key and array layouts, each holding some of the targets.
const TABLE_LAYOUTS: [&str; 14] = [
    "[ui]\nmig = { percent = 125 }\ntheme = \"dark\"\nk = 2\n",
    "[ui]\nmig.percent = 125\ntheme = \"dark\"\nk = 2\n",
    "[ui]\nmig.percent = 125\ntheme = \"dark\"\nmig.unit = 1\nk = 2\n",
    "[ui.mig]\npercent = 125\n[ui.mig.deep]\nz = 1\n[ui]\ntheme = \"dark\"\n",
    "ui.mig.percent = 125\nui.theme = \"dark\"\nui.mig.unit = 1\nui.k = 2\n",
    "ui.theme = \"dark\"\n[ui.mig]\npercent = 125\n",
    "ui.theme = \"dark\"\nui.k = 2\n[ui.mig]\npercent = 125\n[ui.mig.deep]\nz = 1\n",
    "[ui]\ntheme = \"dark\"\n[ui.mig]\npercent = 125\n[ui.k]\nq = 1\n",
    "[[ui.list]]\nx = 1\n[[ui.list]]\nx = 2\n[ui]\ntheme = \"dark\"\n",
    "[ui]\nlist = [{ x = 1 }, { x = 2 }]\ntheme = \"dark\"\n",
    "[ui]\nlist = [\n  { x = 1 }, # one\n  { x = 2 },\n]\nk = 2\n",
    "ui.list = [1, 2]\nui.theme = \"dark\"\n",
    "[[ui.list]]\nx = 1\n[ui.list.sub]\ny = 1\n[[ui.list]]\nx = 2\n",
    "[ui]\ntheme = \"dark\"\nk = 2\n",
];

fn layouts() -> impl Strategy<Value = (String, bool)> {
    let body = prop_oneof![inline_layout(), proptest::sample::select(TABLE_LAYOUTS.to_vec()).prop_map(str::to_owned)];
    (body, proptest::bool::ANY, proptest::bool::ANY, proptest::bool::ANY).prop_map(|(body, crlf, comments, lead)| {
        let head = if lead { if comments { "u0 = 1\n# c1\n" } else { "u0 = 1\n" } } else { "" };
        let tail = if comments { "# c2\n[uother]\nuk = 1\n" } else { "[uother]\nuk = 1\n" };
        let text = format!("{head}{body}{tail}");
        (if crlf { text.replace('\n', "\r\n") } else { text }, crlf)
    })
}

/// The document as JSON without `key`: what every other key means.
fn others(text: &str, key: &str) -> Value {
    let mut json = item_to_json(parse(text).unwrap().as_item());
    remove_path(&mut json, &key.split('.').collect::<Vec<_>>());
    json
}

/// The lines outside the layout under test, which no edit may touch.
fn outside(text: &str) -> Vec<&str> {
    ["u0 = 1", "# c1", "# c2", "[uother]", "uk = 1"]
        .into_iter()
        .filter(|line| text.lines().any(|l| l == *line))
        .collect()
}

fn subsequence(lines: &[&str], text: &str) -> bool {
    let mut rest = text.lines();
    lines.iter().all(|line| rest.any(|l| l == *line))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn load_set_reset_and_reopen_agree_for_every_layout((text, crlf) in layouts()) {
        let Ok(doc) = parse(&text) else { return Ok(()) };
        let keep = outside(&text);
        for key in TARGETS {
            if lookup(&doc, key).is_none() {
                continue;
            }
            // Setting a new value.
            let new = json!("new");
            let edit = Edit { key: key.into(), value: Some((new.clone(), 1)) };
            let done = apply_each(Some(&text), std::slice::from_ref(&edit), &BTreeMap::new()).unwrap();
            prop_assert!(done.refused.is_empty(), "set {key} refused: {:?}\n{:?}", done.refused, text);
            let reopened = parse(&done.text).unwrap();
            prop_assert_eq!(lookup(&reopened, key), Some(new), "set {} in {:?} gave {:?}", key, text, done.text);
            prop_assert_eq!(others(&text, key), others(&done.text, key), "set {} moved another key\n{:?}\n{:?}", key, text, done.text);
            prop_assert!(subsequence(&keep, &done.text), "set {} lost outside lines\n{:?}\n{:?}", key, text, done.text);
            if crlf {
                prop_assert_eq!(done.text.matches('\n').count(), done.text.matches("\r\n").count(), "bare LF {:?}", done.text);
            }
            // Setting it again changes nothing more.
            let again = apply_each(Some(&done.text), std::slice::from_ref(&edit), &BTreeMap::new()).unwrap();
            prop_assert_eq!(&again.text, &done.text);

            // Resetting it.
            let reset = Edit { key: key.into(), value: None };
            let done = apply_each(Some(&text), std::slice::from_ref(&reset), &BTreeMap::new()).unwrap();
            prop_assert!(done.refused.is_empty(), "reset {key} refused: {:?}\n{:?}", done.refused, text);
            let reopened = parse(&done.text).unwrap();
            prop_assert_eq!(lookup(&reopened, key), None, "reset {} in {:?} gave {:?}", key, text, done.text);
            prop_assert_eq!(others(&text, key), others(&done.text, key), "reset {} moved another key\n{:?}\n{:?}", key, text, done.text);
            if text.contains("ui = {") {
                // Inside an inline table the edit only deletes: the result is the original less the entry's own bytes
                // and its one separator, so no neighbour's comment, nor the braces', can have gone.
                let mut rest = text.chars();
                prop_assert!(done.text.chars().all(|c| rest.any(|o| o == c)), "reset {} added text\n{:?}\n{:?}", key, text, done.text);
                let comments = |t: &str, tag: &str| t.matches(tag).count();
                for tag in ["# o", "# z", "# end"] {
                    prop_assert_eq!(comments(&text, tag), comments(&done.text, tag), "reset {} lost {}\n{:?}\n{:?}", key, tag, text, done.text);
                }
                let lost_c = comments(&text, "# c") - comments(&done.text, "# c");
                prop_assert!(lost_c <= 1, "reset {} lost {} neighbours' comments\n{:?}\n{:?}", key, lost_c, text, done.text);
            }
            prop_assert!(subsequence(&keep, &done.text), "reset {} lost outside lines\n{:?}\n{:?}", key, text, done.text);

            // A group with a member the file refuses changes nothing at all, and neither member is reported as made.
            let poison = Edit { key: "uother.uk.sub".into(), value: Some((json!(1), 1)) };
            let needs = BTreeMap::from([("uother.uk.sub".to_owned(), key.to_owned())]);
            let done = apply_each(Some(&text), &[reset, poison], &needs).unwrap();
            prop_assert_eq!(&done.text, &text, "a refused group must change nothing");
            prop_assert_eq!(done.refused.len(), 2);
        }
    }
}

/// Renames of every relation between the old and the new key: (file, old key, new key).
const RENAMES: [(&str, &str, &str); 11] = [
    ("[ui]\nold = 7\n", "ui.old", "ui.old.new"),
    ("[ui.old]\nnew = 7\n", "ui.old.new", "ui.old"),
    ("[ui]\na = 7\nz = 1\n", "ui.a", "ui.b"),
    ("[x]\na = 7\n", "x.a", "ui.b"),
    ("ui = { old = 7 }\n", "ui.old", "ui.new"),
    ("ui = { old = 7 # c\n, k = 1 }\n", "ui.old", "ui.new"),
    ("ui.old = 7\nui.k = 1\n", "ui.old", "ui.new"),
    ("[ui]\nold.p = 7\n", "ui.old", "ui.new"),
    ("[ui]\nold = { p = 7 }\nnew2 = 1\n", "ui.old", "ui.new2x"),
    ("ui = { old = 7, new = { q = 1 } }\n", "ui.old", "ui.new.r"),
    ("ui = {\n  new = 1,\n  old = 7 # old\n  ,\n}\n", "ui.old", "ui.new"),
];

fn rename_edits(from: &str, to: &str, value: &Value) -> ([Edit; 2], BTreeMap<String, String>) {
    let edits = [Edit { key: to.into(), value: Some((value.clone(), 1)) }, Edit { key: from.into(), value: None }];
    (edits, BTreeMap::from([(from.to_owned(), to.to_owned())]))
}

fn related(a: &str, b: &str) -> bool {
    a.starts_with(&format!("{b}.")) || b.starts_with(&format!("{a}."))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1500))]

    #[test]
    fn a_rename_group_applies_in_full_or_not_at_all_and_a_retry_after_repair_succeeds(
        which in 0..RENAMES.len(),
        crlf in proptest::bool::ANY,
        comments in proptest::bool::ANY,
        blocked in proptest::bool::ANY,
    ) {
        let (body, from, to) = RENAMES[which];
        // A blocked rename has its new name under a plain value; repair removes that value.
        let (to, blocker) = if blocked { ("zz.new", "zz = 0\n") } else { (to, "") };
        let head = if comments { "u0 = 1\n# c1\n" } else { "u0 = 1\n" };
        let tail = if comments { "# c2\n[uother]\nuk = 1\n" } else { "[uother]\nuk = 1\n" };
        let wrap = |t: String| if crlf { t.replace('\n', "\r\n") } else { t };
        let text = wrap(format!("{head}{blocker}{body}{tail}"));
        let Ok(doc) = parse(&text) else { return Ok(()) };
        let Some(value) = lookup(&doc, from) else { return Ok(()) };
        let (edits, needs) = rename_edits(from, to, &value);
        let keep = outside(&text);

        let done = apply_each(Some(&text), &edits, &needs).unwrap();
        if blocked {
            prop_assert_eq!(&done.text, &text, "a refused group changes nothing");
            prop_assert_eq!(done.refused.len(), 2);
            // Repaired, the same edits apply.
            let repaired = wrap(format!("{head}{body}{tail}"));
            let retry = apply_each(Some(&repaired), &edits, &needs).unwrap();
            prop_assert!(retry.refused.is_empty(), "retry refused: {:?}\n{:?}", retry.refused, repaired);
            let reopened = parse(&retry.text).unwrap();
            prop_assert_eq!(lookup(&reopened, to), Some(value));
        } else {
            prop_assert!(done.refused.is_empty(), "{from} -> {to} refused: {:?}\n{:?}", done.refused, text);
            let reopened = parse(&done.text).unwrap();
            prop_assert_eq!(lookup(&reopened, to), Some(value.clone()), "{} -> {} in {:?} gave {:?}", from, to, text, done.text);
            if !related(from, to) {
                prop_assert_eq!(lookup(&reopened, from), None, "{} -> {} left the old name in {:?}", from, to, done.text);
            }
            prop_assert!(subsequence(&keep, &done.text), "{} -> {} lost outside lines\n{:?}\n{:?}", from, to, text, done.text);
            if crlf {
                prop_assert_eq!(done.text.matches('\n').count(), done.text.matches("\r\n").count(), "bare LF {:?}", done.text);
            }
            let strip = |t: &str| {
                let mut json = item_to_json(parse(t).unwrap().as_item());
                for key in [from, to] {
                    remove_path(&mut json, &key.split('.').collect::<Vec<_>>());
                }
                json
            };
            if !related(from, to) {
                prop_assert_eq!(strip(&text), strip(&done.text), "{} -> {} moved another key", from, to);
            }
            // Applying the group again changes nothing more.
            let again = apply_each(Some(&done.text), &edits[..1], &BTreeMap::new()).unwrap();
            prop_assert_eq!(&again.text, &done.text);
        }
    }
}
