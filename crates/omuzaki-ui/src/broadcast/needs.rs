//! A show's needs in plain words (`docs/design/automation.md` D11): what the approval card says before a person allows
//! a show to reach outside the studio. Each need is one line that names exactly what it lets the show do, so "Allow
//! on this machine" is a decision a person can make by reading the card.

use egui::Context;
use omuzaki_live::compile::{FileHasher, ProgramHasher};
use omuzaki_live_api::need::{ArgTemplate, Invocation, Need, NetScheme};
use omuzaki_ops_api::OpInfo;

use super::text::{Word, say};

/// How many hex digits of a program's fingerprint the card shows: enough to tell two builds apart at a glance.
const SHORT_HASH: usize = 12;

/// `need` as one line for the approval card. `ops` is the runner's catalogue, for what an op does.
pub fn line(ctx: &Context, need: &Need, ops: &[OpInfo]) -> String {
    match need {
        Need::Scope(scope) => say(ctx, Word::NeedScope, &[("scope", scope.name())]),
        Need::Op { op, .. } => match ops.iter().find(|o| o.name == op.as_str()) {
            Some(info) => say(ctx, Word::NeedOp, &[("op", op), ("summary", info.summary.trim_end_matches('.'))]),
            None => say(ctx, Word::NeedOpUnknown, &[("op", op)]),
        },
        Need::Host { scheme, host, port, .. } => {
            let scheme = match scheme {
                NetScheme::Http => "http",
                NetScheme::Https => "https",
                _ => "udp",
            };
            say(ctx, Word::NeedHost, &[("address", &format!("{scheme}://{host}:{port}"))])
        }
        Need::File { path, write, .. } => {
            say(ctx, if *write { Word::NeedFileWrite } else { Word::NeedFileRead }, &[("path", path)])
        }
        Need::Program(invocation) => program(ctx, invocation),
        Need::Secret { name, host, .. } => say(ctx, Word::NeedSecret, &[("name", name), ("host", host)]),
        Need::Process { name, .. } => say(ctx, Word::NeedProcess, &[("name", name)]),
        Need::Variable { name, .. } => say(ctx, Word::NeedVariable, &[("name", name.as_str())]),
        Need::Group { group, .. } => say(ctx, Word::NeedGroup, &[("group", group.as_str())]),
        _ => format!("{need:?}"),
    }
}

/// A program, its argument template and a short fingerprint of the file as it is now: the card pins exactly this file.
fn program(ctx: &Context, invocation: &Invocation) -> String {
    let args: Vec<String> = invocation
        .args
        .iter()
        .map(|arg| match arg {
            ArgTemplate::Literal(text) => text.clone(),
            ArgTemplate::Input(port) => format!("{{{port}}}"),
            _ => String::from("?"),
        })
        .collect();
    let command = std::iter::once(invocation.program.clone()).chain(args).collect::<Vec<_>>().join(" ");
    match FileHasher.hash(&invocation.program) {
        Some(hash) => {
            let hex = hash.to_string();
            let short = hex.get(..SHORT_HASH).unwrap_or(&hex);
            say(ctx, Word::NeedProgram, &[("command", &command), ("hash", short)])
        }
        None => say(ctx, Word::NeedProgramMissing, &[("command", &command)]),
    }
}

/// The secrets among `needs`: the name each is stored under in the keychain (`automation/<name>`) and its host.
pub fn secrets<'a>(needs: impl IntoIterator<Item = &'a Need>) -> Vec<(String, String)> {
    needs
        .into_iter()
        .filter_map(|need| match need {
            Need::Secret { name, host, .. } => Some((name.clone(), host.clone())),
            _ => None,
        })
        .collect()
}

/// The keychain label of the show secret `name` (design §8 answer 5).
pub fn secret_label(name: &str) -> String {
    format!("automation/{name}")
}
