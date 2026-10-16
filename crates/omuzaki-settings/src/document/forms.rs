//! Exact-byte tests across the hard TOML forms: for each form, a key is set, reset, renamed and removed, and the result
//! must be the original text with only that entry's bytes changed (and still parse and read back).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::*;

/// One form: the text, the key to edit, the text of that key's value (replaced when the key is set), and the whole text
/// as it must be once the key is removed.
struct Form {
    name: &'static str,
    text: &'static str,
    key: &'static str,
    value: &'static str,
    removed: &'static str,
}

const FORMS: &[Form] = &[
    Form {
        name: "dotted key under a header, with a comment above and one beside",
        text: "[ui]\n# lead\nscale.x = 41 # c\ntheme = 2\n",
        key: "ui.scale.x",
        value: "41",
        removed: "[ui]\n# lead\ntheme = 2\n",
    },
    Form {
        name: "dotted keys at the root",
        text: "ui.scale = 41 # keep\nui.theme = \"a\"\nz = 2\n",
        key: "ui.scale",
        value: "41",
        removed: "ui.theme = \"a\"\nz = 2\n",
    },
    Form {
        name: "quoted header and quoted key",
        text: "[\"ui\"]\n\"sca le\" = 41\n'theme' = 2\n",
        key: "ui.sca le",
        value: "41",
        removed: "[\"ui\"]\n'theme' = 2\n",
    },
    Form {
        name: "quoted dotted key at the root",
        text: "\"a b\".c = 41\nz = 1\n",
        key: "a b.c",
        value: "41",
        removed: "z = 1\n",
    },
    Form {
        name: "last entry of an inline table in an inline table",
        text: "ui = { x = { p = 1, q = 41 }, y = 3 }\n",
        key: "ui.x.q",
        value: "41",
        removed: "ui = { x = { p = 1 }, y = 3 }\n",
    },
    Form {
        name: "last entry of the only inline table in an inline table",
        text: "ui = { x = { p = 1, q = 41 } }\n",
        key: "ui.x.q",
        value: "41",
        removed: "ui = { x = { p = 1 } }\n",
    },
    Form {
        name: "first entry of an inline table in an inline table",
        text: "ui = { x = { p = 41, q = 2 }, y = 3 }\n",
        key: "ui.x.p",
        value: "41",
        removed: "ui = { x = { q = 2 }, y = 3 }\n",
    },
    Form {
        name: "inline tables in inline tables, over several lines with comments",
        text: "ui = {\n  x = { p = 1, # one\n    q = 41 }, # two\n  y = 3 # three\n}\n",
        key: "ui.x.q",
        value: "41",
        removed: "ui = {\n  x = { p = 1 # one\n     }, # two\n  y = 3 # three\n}\n",
    },
    Form {
        name: "dotted keys inside a nested inline table (first)",
        text: "ui = { x = { p.a = 41, p.b = 2, q = 3 }, y.z = 4 }\n",
        key: "ui.x.p.a",
        value: "41",
        removed: "ui = { x = { p.b = 2, q = 3 }, y.z = 4 }\n",
    },
    Form {
        name: "dotted keys inside a nested inline table (neighbour of a dotted chain)",
        text: "ui = { x = { p.a = 1, p.b = 2, q = 41 }, y.z = 4 }\n",
        key: "ui.x.q",
        value: "41",
        removed: "ui = { x = { p.a = 1, p.b = 2 }, y.z = 4 }\n",
    },
    Form {
        name: "last dotted entry of a nested inline table",
        text: "ui = { x = { q = 3, p.a = 41 } }\n",
        key: "ui.x.p.a",
        value: "41",
        removed: "ui = { x = { q = 3 } }\n",
    },
    Form {
        name: "entry after a multi-line array",
        text: "[ui]\nlist = [\n  1, # a\n  2,\n]\nscale = 41 # c\n",
        key: "ui.scale",
        value: "41",
        removed: "[ui]\nlist = [\n  1, # a\n  2,\n]\n",
    },
    Form {
        name: "a multi-line array",
        text: "[ui]\nlist = [\n  1, # a\n  41,\n]\nscale = 5 # c\n",
        key: "ui.list",
        value: "[\n  1, # a\n  41,\n]",
        removed: "[ui]\nscale = 5 # c\n",
    },
    Form {
        name: "entry after a multi-line string with lines that start with #",
        text: "[ui]\nnote = \"\"\"\n# not a comment\nmore\n# y\"\"\"\nscale = 41\n",
        key: "ui.scale",
        value: "41",
        removed: "[ui]\nnote = \"\"\"\n# not a comment\nmore\n# y\"\"\"\n",
    },
    Form {
        name: "a multi-line string with lines that start with #",
        text: "[ui]\nnote = '''\n# not a comment\n41'''\nscale = 5\n",
        key: "ui.note",
        value: "'''\n# not a comment\n41'''",
        removed: "[ui]\nscale = 5\n",
    },
    Form {
        name: "comments everywhere",
        text: "# top\n\n[ui] # hdr\n# lead\nscale = 41 # tail\n# between\ntheme = 2\n\n# next\n[o]\n",
        key: "ui.scale",
        value: "41",
        removed: "# top\n\n[ui] # hdr\n# lead\n# between\ntheme = 2\n\n# next\n[o]\n",
    },
    Form {
        name: "a comment ending in a period beside the previous inline entry",
        text: "ui = {\n  theme = \"dark\", # keep theme.\n  scale = 41\n}\n",
        key: "ui.scale",
        value: "41",
        removed: "ui = {\n  theme = \"dark\" # keep theme.\n}\n",
    },
    Form {
        name: "an inline entry with its comma and comment on its own line",
        text: "ui = {\n  a = 1,\n  b = 41, # mine\n  c = 3\n}\n",
        key: "ui.b",
        value: "41",
        removed: "ui = {\n  a = 1,\n  c = 3\n}\n",
    },
];

fn set_edit(key: &str, value: Value) -> Edit {
    Edit { key: key.into(), value: Some((value, 1)) }
}

fn drop_edit(key: &str) -> Edit {
    Edit { key: key.into(), value: None }
}

fn eol_form(text: &str, eol: &str) -> String {
    text.replace('\n', eol)
}

/// Everything but the keys in `skip`, as JSON.
fn others(text: &str, skip: &[&str]) -> Value {
    let mut json = item_to_json(parse(text).unwrap().as_item());
    for key in skip {
        remove_path(&mut json, &key.split('.').collect::<Vec<_>>());
    }
    json
}

#[test]
fn every_hard_form_is_set_reset_renamed_and_removed_changing_only_its_own_entry() {
    for form in FORMS {
        for eol in ["\n", "\r\n"] {
            let ctx = format!("{} ({eol:?})", form.name);
            let (text, removed) = (eol_form(form.text, eol), eol_form(form.removed, eol));
            let value = lookup(&parse(&text).unwrap(), form.key).expect(&ctx);
            let renamed_value_has_newline = value.to_string().contains("\\n");

            // Set: the value's bytes change, nothing else.
            let set = apply(Some(&text), &[set_edit(form.key, json!(99))]).expect(&ctx);
            assert_eq!(set, text.replacen(&eol_form(form.value, eol), "99", 1), "set: {ctx}");

            // Remove and reset (the key and a retired name that is not in the file).
            let dropped = apply(Some(&text), &[drop_edit(form.key)]).expect(&ctx);
            assert_eq!(dropped, removed, "remove: {ctx}");
            let retired = BTreeMap::from([("ui.retired".to_owned(), form.key.to_owned())]);
            let reset = apply_each(Some(&text), &[drop_edit(form.key), drop_edit("ui.retired")], &retired).expect(&ctx);
            assert!(reset.refused.is_empty(), "reset: {ctx}: {:?}", reset.refused);
            assert_eq!(reset.text, removed, "reset: {ctx}");
            assert_eq!(lookup(&parse(&reset.text).unwrap(), form.key), None, "{ctx}");

            // Rename: the new name is written, the old one removed; the file is the removal plus one inserted block.
            let top = form.key.split('.').next().unwrap();
            let renamed_key = format!("{top}.renamed");
            let renamed =
                apply(Some(&text), &[set_edit(&renamed_key, value.clone()), drop_edit(form.key)]).expect(&ctx);
            let doc = parse(&renamed).expect(&ctx);
            assert_eq!(lookup(&doc, &renamed_key), Some(value), "rename: {ctx}");
            assert_eq!(lookup(&doc, form.key), None, "rename: {ctx}");
            assert_eq!(others(&text, &[form.key]), others(&renamed, &[&renamed_key]), "rename: {ctx}");
            // Taking the new entry out again (exactly checked above as a removal) gives back the removal alone, so the
            // rename changed nothing but the two entries.
            assert_eq!(apply(Some(&renamed), &[drop_edit(&renamed_key)]).expect(&ctx), removed, "rename: {ctx}");
            // (A string value that holds a newline is written with the newline it has, LF.)
            if eol == "\r\n" && !renamed_value_has_newline {
                assert_eq!(renamed.matches('\n').count(), renamed.matches("\r\n").count(), "bare LF: {ctx}");
            }
        }
    }
}

#[test]
fn a_section_before_a_multi_line_string_ending_in_hash_is_removed_whole() {
    for eol in ["\n", "\r\n"] {
        // The last line of the string starts with `#`; it is string content, not a comment of `[b]`.
        let text = eol_form("[a]\ns = \"\"\"\nx\n# y\"\"\"\n[b]\nk = 41\n", eol);
        assert_eq!(apply(Some(&text), &[drop_edit("a")]).unwrap(), eol_form("[b]\nk = 41\n", eol));
        assert_eq!(apply(Some(&text), &[set_edit("a", json!(99))]).unwrap(), eol_form("a = 99\n[b]\nk = 41\n", eol));
        // A real comment directly above the next header is that header's, so it stays; a blank line cuts the run (the
        // comment above the blank line belongs to the section being removed).
        let text = eol_form("[a]\ns = 1\n\n# gap\n\n# real\n[b]\nk = 1\n", eol);
        assert_eq!(apply(Some(&text), &[drop_edit("a")]).unwrap(), eol_form("# real\n[b]\nk = 1\n", eol));
    }
}

#[test]
fn removing_the_last_entry_of_a_nested_inline_table_leaves_no_dangling_comma() {
    for eol in ["\n", "\r\n"] {
        let text = eol_form("ui = { x = { p = 1, q = 2 } }\n", eol);
        let removed = apply(Some(&text), &[drop_edit("ui.x.q")]).unwrap();
        assert_eq!(removed, eol_form("ui = { x = { p = 1 } }\n", eol));
        let text = eol_form("ui = { x = { p.a = 1, q = 2 }, y = 3 }\n", eol);
        let removed = apply(Some(&text), &[drop_edit("ui.x.q")]).unwrap();
        assert_eq!(removed, eol_form("ui = { x = { p.a = 1 }, y = 3 }\n", eol));
    }
}

#[test]
fn a_new_inline_key_goes_after_the_last_entrys_own_comment_not_between_them() {
    for eol in ["\n", "\r\n"] {
        let text = eol_form("ui = {\n  a = 1,\n  y = 3 # about y\n}\n", eol);
        let added = apply(Some(&text), &[set_edit("ui.n", json!(5))]).unwrap();
        assert_eq!(added, eol_form("ui = {\n  a = 1,\n  y = 3, # about y\n  n = 5\n}\n", eol));
        let same_line = eol_form("ui = { a = 1, y = 3 } # about ui\n", eol);
        let added = apply(Some(&same_line), &[set_edit("ui.n", json!(5))]).unwrap();
        assert_eq!(added, eol_form("ui = { a = 1, y = 3, n = 5 } # about ui\n", eol));
    }
}
