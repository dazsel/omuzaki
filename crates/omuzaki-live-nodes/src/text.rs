//! The text nodes (design §6, category Text): `text.format` and `text.contains` are pure, `text.to_number` is active
//! (it can fail, and a failure is an event).

use omuzaki_l10n_api::Text;
use omuzaki_live_api::ids::PortName;
use omuzaki_live_api::node::{
    Category, EvalCx, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, ParamError, ParamErrors, Ports,
};
use omuzaki_live_api::register_node;
use omuzaki_live_api::value::{PortType, Val};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::fold::fold;
use crate::l10n;

/// The most placeholders a template has.
const MAX_PLACEHOLDERS: usize = 32;

/// A pure node's handle: it never gets an event (design D4).
fn pure_handle(input: &Input<'_>) -> Result<(), NodeError> {
    match input {
        Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
        Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
        other => Err(NodeError::unhandled(other)),
    }
}

// ── text.format ────────────────────────────────────────────────────────────────────────────────────────────────

/// One piece of a parsed template.
#[derive(Clone, Debug, PartialEq)]
enum Piece {
    Literal(String),
    Input(String),
}

/// Why a template doesn't parse: the error's path detail and the offending text.
#[derive(Debug)]
enum TemplateError {
    /// `{` with no `}`.
    Unclosed,
    /// A lone `}`.
    StrayClose,
    /// A placeholder whose name can't be a port.
    BadName(String),
}

/// The template's pieces: `{name}` is an input, `{{` and `}}` write braces.
fn parse(template: &str) -> Result<Vec<Piece>, TemplateError> {
    let mut pieces = Vec::new();
    let mut literal = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err(TemplateError::Unclosed),
                    }
                }
                if PortName::new(name.as_str()).is_err() {
                    return Err(TemplateError::BadName(name));
                }
                if !literal.is_empty() {
                    pieces.push(Piece::Literal(std::mem::take(&mut literal)));
                }
                pieces.push(Piece::Input(name));
            }
            '}' => return Err(TemplateError::StrayClose),
            c => literal.push(c),
        }
    }
    if !literal.is_empty() {
        pieces.push(Piece::Literal(literal));
    }
    Ok(pieces)
}

/// The distinct input names of a template, in first-use order.
fn names(pieces: &[Piece]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for piece in pieces {
        if let Piece::Input(name) = piece
            && !names.contains(name)
        {
            names.push(name.clone());
        }
    }
    names
}

/// Parameters of `text.format`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FormatParams {
    /// The text to build. Each `{name}` becomes an input called `name` (lowercase letters, digits and `_`); write
    /// `{{` and `}}` for literal braces.
    #[serde(default)]
    pub template: String,
}

/// Builds text from a template and its inputs.
#[derive(Debug)]
pub struct Format {
    pieces: Vec<Piece>,
}

impl NodeKind for Format {
    type Params = FormatParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("text.format", "Build text from a template, filling in values.", NodeClass::Pure, Category::Text)
    }

    fn ports(params: &FormatParams) -> Ports {
        let pieces = parse(&params.template).unwrap_or_default();
        let ports = names(&pieces).into_iter().fold(Ports::new(), |ports, name| match PortName::new(name.clone()) {
            Ok(port) => ports.dynamic_in(port, PortType::Text, format!("{{{name}}}")),
            Err(_) => ports,
        });
        ports.live_out("text", PortType::Text, "The text, with the values filled in.")
    }

    fn check(params: &FormatParams) -> Result<(), ParamErrors> {
        let error = |text| Err(ParamErrors(vec![ParamError::new("template", text)]));
        match parse(&params.template) {
            Ok(pieces) if names(&pieces).len() > MAX_PLACEHOLDERS => {
                error(l10n::too_many_placeholders(crate::flow::int(MAX_PLACEHOLDERS)))
            }
            Ok(_) => Ok(()),
            Err(TemplateError::Unclosed) => error(l10n::template_unclosed()),
            Err(TemplateError::StrayClose) => error(l10n::template_stray_close()),
            Err(TemplateError::BadName(name)) => error(l10n::template_bad_name(name)),
        }
    }

    fn new(params: FormatParams) -> Self {
        Self { pieces: parse(&params.template).unwrap_or_default() }
    }
}

impl Node for Format {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        pure_handle(&input)
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        if port != "text" {
            return Err(NodeError::not_live(port));
        }
        let mut text = String::new();
        for piece in &self.pieces {
            match piece {
                Piece::Literal(literal) => text.push_str(literal),
                // Numbers arrive as text by the coercion rules (design D3): shortest round-trip, whole numbers
                // without `.0`; an input with nothing wired or set reads as empty.
                Piece::Input(name) => text.push_str(&cx.input_text(name)?),
            }
        }
        Ok(Val::text(text))
    }
}
register_node!(Format);

// ── text.to_number ─────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `text.to_number`: none.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToNumberParams {}

/// Reads a number out of text.
#[derive(Debug)]
pub struct ToNumber;

/// The number in `text` (surrounding space allowed, exponents too), or why there is none.
pub(crate) fn parse_number(text: &str) -> Result<f64, Text> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(l10n::not_a_number_empty());
    }
    // Rust's parser takes "inf" and "NaN", which aren't numbers a person means here.
    let plain = trimmed.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E'));
    match trimmed.parse::<f64>() {
        Ok(x) if plain && x.is_finite() => Ok(x),
        _ => Err(l10n::not_a_number(trimmed)),
    }
}

impl NodeKind for ToNumber {
    type Params = ToNumberParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("text.to_number", "Read a number out of text.", NodeClass::Active, Category::Text)
    }

    fn ports(_: &ToNumberParams) -> Ports {
        Ports::new()
            .event_in("in", "Reads the number.")
            .value_in("text", PortType::Text, Val::text(""), "The text, such as \"42\" or \" 4.5 \".")
            .event_out("done", "The text was a number; it is on `value`.")
            .event_out("failed", "The text wasn't a number; the reason is on `error`.")
            .value_out("value", PortType::Number, "The number.")
            .value_out("error", PortType::Text, "Why it wasn't a number, in English.")
    }

    fn new(_: ToNumberParams) -> Self {
        Self
    }
}

impl Node for ToNumber {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                let text = cx.input_text("text")?;
                match parse_number(&text) {
                    Ok(x) => {
                        cx.set("value", Val::Number(x))?;
                        cx.fire("done")
                    }
                    Err(why) => cx.fail("failed", why),
                }
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(ToNumber);

// ── text.contains ──────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `text.contains`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContainsParams {
    /// Compare case and width exactly; otherwise "ＢＯＮＫ" contains "bonk".
    #[serde(default)]
    pub case_sensitive: bool,
}

/// Whether `text` contains `part`.
#[derive(Debug)]
pub struct Contains {
    case_sensitive: bool,
}

impl NodeKind for Contains {
    type Params = ContainsParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("text.contains", "Whether a text contains another.", NodeClass::Pure, Category::Text)
    }

    fn ports(_: &ContainsParams) -> Ports {
        Ports::new()
            .value_in("text", PortType::Text, Val::text(""), "The text to look in.")
            .value_in("part", PortType::Text, Val::text(""), "The text to look for.")
            .live_out("value", PortType::Bool, "Whether it is there.")
    }

    fn new(params: ContainsParams) -> Self {
        Self { case_sensitive: params.case_sensitive }
    }
}

impl Node for Contains {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        pure_handle(&input)
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        if port != "value" {
            return Err(NodeError::not_live(port));
        }
        let (haystack, needle) = (cx.input_text("text")?, cx.input_text("part")?);
        let found =
            if self.case_sensitive { haystack.contains(&*needle) } else { fold(&haystack).contains(&fold(&needle)) };
        Ok(Val::Bool(found))
    }
}
register_node!(Contains);
