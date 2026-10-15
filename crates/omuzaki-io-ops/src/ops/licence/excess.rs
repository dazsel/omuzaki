//! Where a record grants more than it may: more than the parts allow, or more than its own provenance allows.
//!
//! `licence.set` refuses a record with any [`Excess`] that is not waived, and `licence.describe` lists them, so both
//! name the same fields with the same words. A field is named by its path inside the record (`meta/commercialUsage`,
//! `provenance/commercial`), the way the file and the schema spell it.

use omuzaki_io_api::UseTerms;
use omuzaki_store_api::{Commercial, Source};

use super::derive::{DerivedLicence, PartRow, source_rank};
use super::terms::{commercial_rank, exceeding};
use crate::gather::avatar::LicenceRecord;

/// One field of a record that grants more than it may.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Excess {
    /// The field, as a path inside the record: `meta/commercialUsage`.
    pub field: String,
    /// What the record says there, as written.
    pub record: String,
    /// Why that is too much, finishing the sentence "`field` is `record`, but ...".
    pub because: String,
    /// Whether the project-owned basis waives it. The private-test flag never is: fan art stays private even when the
    /// person who drew it owns it.
    pub waivable: bool,
}

impl Excess {
    /// "`meta/commercialUsage` is `personalProfit`, but the parts allow only `personalNonProfit` (limited by `hair`)".
    pub fn sentence(&self) -> String {
        format!("`{}` is `{}`, but {}", self.field, self.record, self.because)
    }
}

/// "`hair`, `vest` (no source recorded)": the parts that set a limit, with the ones that count only because nobody
/// recorded a source marked as such.
fn naming(rows: &[&PartRow]) -> String {
    let named: Vec<String> = rows
        .iter()
        .map(|r| if r.recorded { format!("`{}`", r.part) } else { format!("`{}` (no source recorded)", r.part) })
        .collect();
    named.join(", ")
}

fn source_name(source: &Source) -> &'static str {
    match source {
        Source::Authored => "authored",
        Source::Imported { .. } => "imported",
        Source::Generated { .. } => "generated",
        Source::Library { .. } => "library",
    }
}

fn written(status: Commercial) -> String {
    match status {
        Commercial::Ok => "ok",
        Commercial::No => "no",
        Commercial::Unknown => "unknown",
    }
    .to_owned()
}

/// The fields on which `record` grants more than the join of the parts and applications in `derived`, in the order the record lists
/// them (private test, commercial status, attribution, then the flags of the file's terms). Empty when it grants no
/// more.
pub(crate) fn against_parts(record: &LicenceRecord, derived: &DerivedLicence) -> Vec<Excess> {
    let limit = &derived.record;
    let contributions = derived.contributions();
    let mut found = Vec::new();

    if derived.private_test && !record.provenance.private_test {
        let rows: Vec<&PartRow> = contributions.iter().filter(|r| r.private_test).collect();
        found.push(Excess {
            field: "provenance/private_test".to_owned(),
            record: "false".to_owned(),
            because: format!("{} private test material, which can never be exported for any purpose but private", {
                if rows.len() == 1 {
                    format!("part {} is", naming(&rows))
                } else {
                    format!("parts {} are", naming(&rows))
                }
            }),
            waivable: false,
        });
    }

    let (kept, needed) = (&record.provenance.source, &limit.provenance.source);
    if source_rank(kept) < source_rank(needed) {
        let rows: Vec<&PartRow> = contributions.iter().filter(|r| r.source.as_ref() == Some(needed)).collect();
        found.push(Excess {
            field: "provenance/source".to_owned(),
            record: source_name(kept).to_owned(),
            because: format!(
                "{} {} imported or generated, and the creator's own terms apply to the file",
                if rows.len() == 1 { "part" } else { "parts" },
                naming(&rows)
            ),
            waivable: true,
        });
    }

    let (status, allowed) = (record.provenance.commercial, limit.provenance.commercial);
    if commercial_rank(status) > commercial_rank(allowed) {
        found.push(Excess {
            field: "provenance/commercial".to_owned(),
            record: written(status),
            because: format!(
                "the parts allow only `{}` (limited by {})",
                written(allowed),
                naming(&contributions.iter().filter(|r| r.commercial == allowed).collect::<Vec<_>>())
            ),
            waivable: true,
        });
    }

    let kept = record.provenance.attribution.as_deref().unwrap_or_default();
    let dropped: Vec<&String> = derived.attributions.iter().filter(|credit| !keeps_credit(kept, credit)).collect();
    if !dropped.is_empty() {
        let rows: Vec<&PartRow> =
            contributions.iter().filter(|r| r.attribution.as_ref().is_some_and(|a| dropped.contains(&a))).collect();
        let credits: Vec<String> = dropped.iter().map(|c| format!("\"{c}\"")).collect();
        found.push(Excess {
            field: "provenance/attribution".to_owned(),
            record: if kept.is_empty() { "(none)".to_owned() } else { kept.to_owned() },
            because: format!("it must keep the credit {} that {} require", credits.join(", "), naming(&rows)),
            waivable: true,
        });
    }

    // The join of the parts' licences, flattened. It is read from the derived record so a character with no parts, which
    // joins to `LicenseRef-Unknown`, is held to that floor too.
    let kept_terms = licence_terms(&record.provenance.licence);
    let missing: Vec<String> =
        licence_terms(&limit.provenance.licence).into_iter().filter(|term| !has_term(&kept_terms, term)).collect();
    if !missing.is_empty() {
        let rows: Vec<&PartRow> =
            contributions.iter().filter(|r| licence_terms(&r.licence).iter().any(|t| has_term(&missing, t))).collect();
        let who = if rows.is_empty() { "the character has no parts".to_owned() } else { naming(&rows) };
        found.push(Excess {
            field: "provenance/licence".to_owned(),
            record: record.provenance.licence.clone(),
            because: format!(
                "it must keep every licence the parts carry, and it leaves out {} of {who}",
                missing.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(", ")
            ),
            waivable: true,
        });
    }

    for axis in exceeding(&record.meta.terms, &limit.meta.terms) {
        let allowed = axis.rank_of(&limit.meta.terms);
        let by_terms: Vec<&PartRow> = contributions.iter().filter(|r| axis.rank_of(&r.terms) == allowed).collect();
        let reason = if by_terms.is_empty() {
            // No part holds the flag back by itself: it is held back because a commercial status that is not `ok` allows
            // no permission at all.
            let rows: Vec<&PartRow> =
                contributions.iter().filter(|r| r.commercial == limit.provenance.commercial).collect();
            format!(
                "commercial status `{}` of {} allows no permission",
                written(limit.provenance.commercial),
                naming(&rows)
            )
        } else {
            format!("limited by {}", naming(&by_terms))
        };
        found.push(Excess {
            field: format!("meta/{}", axis.key),
            record: axis.show_of(&record.meta.terms),
            because: format!("the parts allow only `{}` ({reason})", axis.show_of(&limit.meta.terms)),
            waivable: true,
        });
    }
    found
}

/// The fields on which `record` grants more than its own provenance does, the rule the exporters enforce
/// (`rights.terms_exceed_provenance`): private test material, and anything not cleared for commercial use, carries only
/// the most restrictive value of every flag. Nothing waives these: the record would not export.
pub(crate) fn against_itself(record: &LicenceRecord) -> Vec<Excess> {
    let why = if record.provenance.private_test {
        "`provenance/private_test` is `true`, and private test material carries only the most restrictive terms"
            .to_owned()
    } else if record.provenance.commercial != Commercial::Ok {
        format!(
            "`provenance/commercial` is `{}`, and terms grant nothing while it is not `ok`",
            written(record.provenance.commercial)
        )
    } else {
        return Vec::new();
    };
    let strictest = UseTerms::default();
    exceeding(&record.meta.terms, &strictest)
        .into_iter()
        .map(|axis| Excess {
            field: format!("meta/{}", axis.key),
            record: axis.show_of(&record.meta.terms),
            because: format!("{why} (the most it may say is `{}`)", axis.show_of(&strictest)),
            waivable: false,
        })
        .collect()
}

/// Whether `kept` holds `credit` as a whole token: the credit is bounded on both sides by a non-alphanumeric character
/// or the edge of the text, so "Ann" is not kept by "Joanna".
fn keeps_credit(kept: &str, credit: &str) -> bool {
    if credit.is_empty() {
        return true;
    }
    // Every start is tried, overlapping ones included: in "JoAnn; Ann; Ann" the first "Ann" fails its left edge and the
    // second, which overlaps nothing but is found only by looking past the first, passes.
    kept.char_indices().any(|(start, _)| {
        let rest = &kept[start..];
        rest.starts_with(credit)
            && !kept[..start].chars().next_back().is_some_and(char::is_alphanumeric)
            && !rest[credit.len()..].chars().next().is_some_and(char::is_alphanumeric)
    })
}

/// The conjuncts of an SPDX licence expression: tokenised, outer brackets stripped, split at top-level `AND`, ids
/// compared ASCII-case-insensitively (see `has_term`). An expression with a top-level `OR` is one term (`AND` binds
/// tighter, so none of its conjuncts is required by itself), and so is a `WITH` expression, as `join_licences` brackets
/// them. A malformed expression (an empty conjunct, unbalanced or empty brackets) is never split: it is one term that
/// no well-formed licence contains. This is a comparison aid, not an SPDX parser.
fn licence_terms(expr: &str) -> Vec<String> {
    let mut terms = Vec::new();
    collect_terms(&tokens(expr), &mut terms);
    terms.sort_unstable_by_key(|t| t.to_ascii_lowercase());
    terms.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    terms
}

/// Whether `terms` holds `term`, ids compared ASCII-case-insensitively.
fn has_term(terms: &[String], term: &str) -> bool {
    terms.iter().any(|t| t.eq_ignore_ascii_case(term))
}

/// Words of an expression with each bracket as a token of its own: `(MIT OR X)` is `(`, `MIT`, `OR`, `X`, `)`.
fn tokens(expr: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for word in expr.split_whitespace() {
        let rest = word.trim_start_matches('(');
        out.extend(std::iter::repeat_n("(", word.len() - rest.len()));
        let body = rest.trim_end_matches(')');
        if !body.is_empty() {
            out.push(body);
        }
        out.extend(std::iter::repeat_n(")", rest.len() - body.len()));
    }
    out
}

/// The tokens written back as one normalised string, with no space inside brackets.
fn render(tokens: &[&str]) -> String {
    let mut out = String::new();
    for (i, token) in tokens.iter().enumerate() {
        if i > 0 && *token != ")" && tokens[i - 1] != "(" {
            out.push(' ');
        }
        out.push_str(token);
    }
    out
}

fn collect_terms(tokens: &[&str], out: &mut Vec<String>) {
    let tokens = strip_brackets(tokens);
    if tokens.is_empty() {
        return;
    }
    let (mut depth, mut balanced, mut top_level_or) = (0usize, true, false);
    let mut conjuncts: Vec<&[&str]> = Vec::new();
    let mut start = 0;
    for (i, token) in tokens.iter().enumerate() {
        match *token {
            "(" => depth += 1,
            ")" => match depth.checked_sub(1) {
                Some(d) => depth = d,
                None => balanced = false,
            },
            t if depth == 0 && t.eq_ignore_ascii_case("AND") => {
                conjuncts.push(&tokens[start..i]);
                start = i + 1;
            }
            t if depth == 0 && t.eq_ignore_ascii_case("OR") => top_level_or = true,
            _ => {}
        }
    }
    conjuncts.push(&tokens[start..]);
    let malformed = !balanced || depth != 0 || conjuncts.iter().any(|c| c.is_empty());
    if conjuncts.len() > 1 && !top_level_or && !malformed {
        for conjunct in conjuncts {
            collect_terms(conjunct, out);
        }
    } else {
        out.push(render(tokens));
    }
}

/// Removes brackets that wrap the whole expression, as many times as they do. Empty brackets stay.
fn strip_brackets<'a, 'b>(mut tokens: &'b [&'a str]) -> &'b [&'a str] {
    while let [first, inner @ .., last] = tokens {
        if *first != "(" || *last != ")" || inner.is_empty() || !wraps(tokens) {
            break;
        }
        tokens = inner;
    }
    tokens
}

/// Whether the opening bracket of `tokens` closes only at its last token.
fn wraps(tokens: &[&str]) -> bool {
    let mut depth = 0usize;
    for (i, token) in tokens.iter().enumerate() {
        match *token {
            "(" => depth += 1,
            ")" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1 == tokens.len();
                }
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_flatten_and_ignore_order_case_brackets_and_spacing() {
        let same = |a: &str, b: &str| {
            let (a, b) = (licence_terms(a), licence_terms(b));
            a.len() == b.len() && a.iter().all(|t| has_term(&b, t))
        };
        assert!(same("CC-BY-4.0 AND CC0-1.0", "(cc0-1.0  AND cc-by-4.0)"));
        assert!(same("CC-BY-4.0\u{a0}AND\u{a0}CC0-1.0", "CC-BY-4.0 AND CC0-1.0"), "Unicode whitespace separates too");
        assert_eq!(licence_terms("CC-BY-4.0 AND (MIT OR Apache-2.0)"), ["CC-BY-4.0", "MIT OR Apache-2.0"]);
        assert_eq!(licence_terms("(MIT OR Apache-2.0) AND (mit or apache-2.0)").len(), 1);
        assert_eq!(licence_terms("MIT OR Apache-2.0"), ["MIT OR Apache-2.0"]);
        assert_eq!(licence_terms("GPL-2.0-only WITH Classpath-exception-2.0").len(), 1);
        assert_eq!(licence_terms("(A) AND (B)"), ["A", "B"]);
        assert_eq!(licence_terms("MIT OR Apache-2.0 AND CC-BY-4.0").len(), 1, "a top-level OR keeps the whole");
        assert_eq!(licence_terms("MIT OR 日本語 AND 日本語").len(), 1, "multi-byte text never splits mid-character");
        for malformed in
            ["MIT AND AND Apache-2.0", "AND MIT", "MIT AND", "(MIT", "MIT)", "()", "MIT AND (", "A AND AND AND B"]
        {
            assert_eq!(licence_terms(malformed), [malformed], "{malformed} is one term nothing satisfies");
        }
        assert_eq!(
            licence_terms("(MIT AND AND X) AND CC0-1.0"),
            ["CC0-1.0", "MIT AND AND X"],
            "a bad part stays its own term"
        );
        assert_eq!(licence_terms("(A) OR (B)"), ["(A) OR (B)"], "brackets that do not wrap the whole stay");
        assert_eq!(licence_terms("  "), Vec::<String>::new());
    }

    /// A small deterministic generator, so the fuzz below needs no dependency.
    fn next(state: &mut u64) -> u64 {
        *state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        *state >> 33
    }

    #[test]
    fn no_string_makes_the_term_scanner_panic() {
        const WORDS: [&str; 14] =
            ["AND", "OR", "WITH", "(", ")", " ", "  ", "MIT", "Apache-2.0", "日本語", "CC0-1.0", "-", "and", "é"];
        let mut state = 7;
        for _ in 0..20_000 {
            let len = next(&mut state) % 12;
            let text: String = (0..len).map(|_| WORDS[(next(&mut state) % 14) as usize]).collect();
            let _ = licence_terms(&text);
        }
        for text in ["", " ", "(", ")", "((", "))", "()()", "AND", "OR", "(AND)", "( ) AND ( )"] {
            let _ = licence_terms(text);
        }
    }

    #[test]
    fn the_join_of_any_licences_flattens_back_to_the_licences_joined() {
        // The shape `derive::join_licences` writes: distinct licences, any with whitespace bracketed, joined by `AND`.
        const LICENCES: [&str; 7] = [
            "CC0-1.0",
            "CC-BY-4.0",
            "MIT OR Apache-2.0",
            "GPL-2.0-only WITH Classpath-exception-2.0",
            "MIT AND AND X",
            "日本語",
            "LicenseRef-Unknown",
        ];
        let mut state = 11;
        for _ in 0..2_000 {
            let mut chosen: Vec<&str> =
                (0..=next(&mut state) % 5).map(|_| LICENCES[(next(&mut state) % 7) as usize]).collect();
            chosen.sort_unstable();
            chosen.dedup();
            let joined = match chosen.as_slice() {
                [one] => (*one).to_owned(),
                many => many
                    .iter()
                    .map(|l| if l.contains(char::is_whitespace) { format!("({l})") } else { (*l).to_owned() })
                    .collect::<Vec<_>>()
                    .join(" AND "),
            };
            let flat = licence_terms(&joined);
            let mut expected: Vec<String> = chosen.iter().flat_map(|l| licence_terms(l)).collect();
            expected.sort_unstable_by_key(|t| t.to_ascii_lowercase());
            expected.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
            assert_eq!(flat, expected, "{joined}");
        }
    }

    #[test]
    fn the_credit_matcher_never_panics_and_finds_every_credit_at_a_token_boundary() {
        const PIECES: [&str; 9] = ["Ann", "Jo", "; ", " ", "日本語", "é", "Ann; Ann", "-", "。"];
        let mut state = 3;
        let text =
            |state: &mut u64| -> String { (0..next(state) % 8).map(|_| PIECES[(next(state) % 9) as usize]).collect() };
        for _ in 0..5_000 {
            let (before, after, credit) = (text(&mut state), text(&mut state), text(&mut state));
            let _ = keeps_credit(&before, &credit);
            // A credit set off by a space or the edge of the text is always kept, wherever the junk around it overlaps.
            let kept = format!("{before} {credit} {after}");
            assert!(keeps_credit(&kept, &credit), "{kept:?} should keep {credit:?}");
        }
    }

    #[test]
    fn a_credit_is_kept_only_as_a_whole_token() {
        assert!(keeps_credit("Model by Ann", "Ann"));
        assert!(keeps_credit("Ann", "Ann"));
        assert!(keeps_credit("by Ann, Bob", "Ann"));
        assert!(!keeps_credit("Joanna", "Ann"));
        assert!(!keeps_credit("Annabel", "Ann"));
        assert!(keeps_credit("Annabel and Ann", "Ann"));
        assert!(keeps_credit("JoAnn; Ann; Ann", "Ann; Ann"), "overlapping candidates are tried");
        assert!(keeps_credit("日本語 Ann。", "Ann"));
    }
}
