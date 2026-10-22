//! The phrase matcher's cost: a 30-word transcript against 50 commands (the shipped 20 in English and Japanese, and 30
//! more, ten of them found inside sentences). Issue #753 holds it to 50 µs.

#![allow(missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_speech::commands::{Command, PhraseList, default_phrases, match_transcript};

fn fifty_commands() -> PhraseList {
    let mut commands = default_phrases().command;
    let words = [
        "jump",
        "dance",
        "sleep",
        "wake up",
        "look left",
        "look right",
        "look up",
        "look down",
        "blink",
        "wink",
        "laugh",
        "cry",
        "shrug",
        "clap",
        "point",
        "think",
        "yawn",
        "salute",
        "peace sign",
        "thumbs up",
    ];
    for (i, word) in words.iter().enumerate() {
        commands.push(Command::new(format!("c{i}"), &[word, &format!("{word} now")], "en"));
    }
    let inside = [
        "bonk",
        "hydrate",
        "start the timer",
        "stop the timer",
        "next scene",
        "previous scene",
        "zoom in",
        "zoom out",
        "lights off",
        "lights on",
    ];
    for (i, phrase) in inside.iter().enumerate() {
        commands.push(Command::new(format!("s{i}"), &[phrase], "en").with_alone(false));
    }
    assert_eq!(commands.len(), 50);
    PhraseList::new(commands)
}

fn matcher(c: &mut Criterion) {
    let list = fifty_commands();
    let chat = "okay so I think we should take a break now and then come back to the boss fight after \
                everyone has had a chance to grab some water and stretch";
    let command = "alright chat that was a great run so lets go to the next scene and see what happens after we \
                   finish the level with the new character build later today";
    assert_eq!(chat.split(' ').filter(|w| !w.is_empty()).count(), 30);
    assert_eq!(command.split(' ').filter(|w| !w.is_empty()).count(), 30);
    assert_eq!(match_transcript(&list, command).len(), 1);
    c.bench_function("thirty_words_fifty_commands_none", |b| b.iter(|| match_transcript(&list, chat)));
    c.bench_function("thirty_words_fifty_commands_one_inside", |b| b.iter(|| match_transcript(&list, command)));
    c.bench_function("one_command_alone", |b| b.iter(|| match_transcript(&list, "Wave, please!")));
}

criterion_group!(benches, matcher);
criterion_main!(benches);
