//! Golden test of the MCP mapping (llm-api D3): a `tools/list` body of three tools (one with an unsafe name) and one
//! `tools/call` result for each rule, serialised and compared with the blessed files in `tests/goldens/mcp/`.
//! Bless after an intended change with `UPDATE_GOLDENS=1 cargo test -p omuzaki-llm --test mcp_golden`.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use omuzaki_llm::mcp::{skipped_tools, tool_defs, tool_result};
use serde_json::{Value, json};

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/mcp").join(name)
}

fn check(name: &str, got: &impl serde::Serialize) {
    let text = serde_json::to_string_pretty(got).unwrap() + "\n";
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(want == text, "{name} differs from its golden; bless with UPDATE_GOLDENS=1\n{text}");
}

#[test]
fn tools_list_maps_to_tool_defs() {
    let list = json!({"tools": [
        {"name": "render_preview", "title": "Render a preview", "description": "Renders the character.",
         "inputSchema": {"type": "object", "properties": {"camera": {"type": "string"}}, "required": ["camera"]},
         "outputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}},
        {"name": "bad name!", "description": "Skipped: outside ^[a-zA-Z0-9_-]{1,64}$.", "inputSchema": {"type": "object"}},
        {"name": "list_parts", "description": "Lists the parts.", "inputSchema": {"type": "object"}}
    ]});
    check("tool_defs.json", &tool_defs(&list));
    assert_eq!(skipped_tools(&list), vec!["bad name!".to_owned()]);
}

fn results() -> Vec<(&'static str, Value)> {
    vec![
        ("result_text.json", json!({"content": [{"type": "text", "text": "Rendered main."}]})),
        ("result_error.json", json!({"isError": true, "content": [{"type": "text", "text": "no such part"}]})),
        (
            "result_image_render.json",
            json!({"content": [{"type": "image", "mimeType": "image/png", "data": "iVBORw==",
                                "uri": "omuzaki://render/abc.png"}]}),
        ),
        (
            "result_image_file.json",
            json!({"content": [{"type": "image", "mimeType": "image/jpeg", "data": "/9j/4A=="}]}),
        ),
        (
            "result_resource_link.json",
            json!({"content": [{"type": "resource_link", "uri": "omuzaki://doc/main", "name": "main"}]}),
        ),
        (
            "result_resource_text.json",
            json!({"content": [{"type": "resource", "resource": {"uri": "omuzaki://doc/main", "text": "hello"}}]}),
        ),
        (
            "result_resource_image.json",
            json!({"content": [{"type": "resource", "resource": {
                "uri": "omuzaki://render/def.png", "mimeType": "image/png", "blob": "iVBORw=="}}]}),
        ),
        (
            "result_audio_and_blob.json",
            json!({"content": [
                {"type": "audio", "mimeType": "audio/wav", "data": "UklGRg=="},
                {"type": "resource", "resource": {"uri": "file:///x.bin", "mimeType": "application/octet-stream",
                                                  "blob": "AAEC"}}
            ]}),
        ),
        ("result_structured.json", json!({"content": [], "structuredContent": {"parts": ["hair", "eyes"], "n": 2}})),
    ]
}

#[test]
fn tools_call_results_map_to_blocks() {
    let all = results();
    assert_eq!(all.len(), 9, "one case for each rule of llm-api D3");
    for (name, result) in &all {
        check(name, &tool_result(result));
    }
}
