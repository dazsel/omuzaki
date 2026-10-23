//! The phrase matcher's table (issue #753): what matches, what doesn't, in English and Japanese.

#![allow(clippy::unwrap_used)]

use omuzaki_speech::commands::{Command, PhraseList, default_phrases, match_transcript};

fn list() -> PhraseList {
    let mut commands = default_phrases().command;
    commands.push(Command::new("sit", &["sit down"], "en"));
    commands.push(Command::new("bonk", &["bonk"], "en").with_alone(false));
    commands.push(Command::new("timer", &["start the timer"], "en").with_alone(false));
    commands.push(Command::new("hydrate", &["水を飲んで"], "ja").with_alone(false));
    PhraseList::new(commands)
}

/// `(transcript, the ids it should give, in order)`.
const TABLE: &[(&str, &[&str])] = &[
    // The phrase alone, with its case and punctuation.
    ("wave", &["wave"]),
    ("Wave!", &["wave"]),
    ("WAVE.", &["wave"]),
    ("  wave  ", &["wave"]),
    ("Ｗａｖｅ", &["wave"]),
    ("say hi", &["wave"]),
    ("Say hi!", &["wave"]),
    ("wave hello", &["wave"]),
    // With the language's filler words.
    ("wave please", &["wave"]),
    ("Please wave.", &["wave"]),
    ("can you wave", &["wave"]),
    ("Could you nod, please?", &["nod"]),
    ("okay, smile now", &["smile"]),
    ("Um, take a bow.", &["bow"]),
    // Inside a longer sentence: never for an `alone` command.
    ("I waved at them", &[]),
    ("wave to the people over there", &[]),
    ("we should smile more", &[]),
    ("that was a nice bow tie", &[]),
    ("the clip that I sent you", &[]),
    // One edit per five letters.
    ("sit down", &["sit"]),
    ("sit town", &["sit"]),
    ("sitdown", &["sit"]),
    ("it's down", &[]),
    ("sit down now please", &["sit"]),
    ("waive", &[]),
    ("wav", &[]),
    ("shake your head", &["shake_head"]),
    ("shake you head", &["shake_head"]),
    ("shake your bread", &["shake_head"]),
    ("look surprized", &["surprised"]),
    ("lock surprised", &["surprised"]),
    // Two commands in one utterance, in order.
    ("wave and nod", &["wave", "nod"]),
    ("nod, wave", &["nod", "wave"]),
    ("smile then take a bow", &["smile", "bow"]),
    // Not alone: found inside a sentence.
    ("okay chat bonk him", &["bonk"]),
    ("can someone start the timer for me", &["timer"]),
    ("start the timmer", &["timer"]),
    ("bonk", &["bonk"]),
    ("he got bonked", &[]),
    // What Whisper makes of a filler: a near filler, an article.
    ("Wave pleaser", &["wave"]),
    ("Clip that pleases.", &["clip"]),
    ("the sad face", &["sad"]),
    ("A clip that.", &["clip"]),
    // Nothing to match.
    ("", &[]),
    ("please", &[]),
    ("thank you so much for the raid", &[]),
    // Japanese, with and without spaces and the polite ending.
    ("手を振って", &["wave"]),
    ("手を振ってください", &["wave"]),
    ("手を振って、ください。", &["wave"]),
    ("手を 振って", &["wave"]),
    ("「手を振って」", &["wave"]),
    ("ちょっと手を振って", &["wave"]),
    ("手を振っていたよね", &[]),
    ("首を横に振って", &["shake_head"]),
    ("うなずいて", &["nod"]),
    ("頷いてください", &["nod"]),
    ("笑ってそしてお辞儀して", &["smile", "bow"]),
    ("手を振ってと頷いて", &["wave", "nod"]),
    ("昨日は友達と笑ってたよ", &[]),
    ("今日は暑いから水を飲んでね", &["hydrate"]),
    ("ｸﾘｯﾌﾟして", &["clip"]),
];

#[test]
fn the_table() {
    let list = list();
    let mut failures = Vec::new();
    for (text, ids) in TABLE {
        let got: Vec<String> = match_transcript(&list, text).into_iter().map(|m| m.id).collect();
        if got != *ids {
            failures.push(format!("{text:?}: expected {ids:?}, got {got:?}"));
        }
    }
    assert!(TABLE.len() >= 40, "the issue asks for at least 40 cases");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_match_names_the_list_entry_never_the_transcript() {
    let found = match_transcript(&list(), "Sit town, please!");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].phrase, "sit down");
    assert!((found[0].score - (1.0 - 1.0 / 8.0)).abs() < 1e-6, "{}", found[0].score);
    let exact = match_transcript(&list(), "say hi");
    assert_eq!((exact[0].phrase.as_str(), exact[0].score), ("say hi", 1.0));
}

#[test]
fn a_list_reads_from_toml_and_its_fillers_count() {
    let list = PhraseList::from_toml(
        r#"
        [[command]]
        id = "hype"
        say = ["let's go"]

        [fillers]
        en = ["yo"]
        "#,
    )
    .unwrap();
    assert_eq!(list.command[0].lang, "en");
    assert!(list.command[0].alone);
    assert_eq!(match_transcript(&list, "Yo, LET'S GO!").len(), 1);
    assert!(match_transcript(&list, "hey lets go").len() == 1, "hey is a built-in filler");
    assert_eq!(match_transcript(&list, "so lets go"), []);
}

#[test]
fn bad_lists_are_refused() {
    let bad = [
        "[[command]]\nid = \"Wave\"\nsay = [\"wave\"]",
        "[[command]]\nid = \"wave\"\nsay = []",
        "[[command]]\nid = \"wave\"\nsay = [\"!!\"]",
        "[[command]]\nid = \"wave\"\nsay = [\"wave\"]\n[[command]]\nid = \"wave\"\nsay = [\"hi\"]\nlang = \"en-GB\"",
        "[[command]]\nid = \"wave\"\nsay = [\"wave\"]\ncolour = 1",
    ];
    for text in bad {
        assert!(PhraseList::from_toml(text).is_err(), "{text}");
    }
    let long = "x".repeat(65);
    assert!(PhraseList::from_toml(&format!("[[command]]\nid = \"a\"\nsay = [\"{long}\"]")).is_err());
}

#[test]
fn a_phrase_list_round_trips_and_has_a_schema() {
    let list = list();
    let text = toml::to_string(&list).unwrap();
    assert_eq!(PhraseList::from_toml(&text).unwrap(), list);
    let schema = serde_json::to_value(schemars::schema_for!(PhraseList)).unwrap();
    assert!(schema["properties"]["command"].is_object(), "{schema}");
    assert!(schema["properties"].get("compiled").is_none());
}
