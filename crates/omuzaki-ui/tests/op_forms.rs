//! Every operation the studio links can be run from its generated form: no field of any registered op's argument
//! schema falls back to a read-only row (#153), so the palette and the inspector reach everything MCP reaches.

use omuzaki_ops_api::Services;
use omuzaki_ui::ds::schema_form::Form;
use omuzaki_ui::wiring::{Document, runner_on};

#[test]
fn every_registered_op_has_a_form_with_no_read_only_field() {
    omuzaki_ui::link();
    let runner = runner_on(Document::empty(), Services::default()).unwrap_or_else(|e| panic!("{e}"));
    let ops = runner.ops();
    assert!(ops.len() > 50, "the studio links the kit, the face and the scene ops: {}", ops.len());
    let read_only: Vec<String> = ops
        .iter()
        .flat_map(|op| {
            Form::from_schema(&op.schema).read_only().into_iter().map(|(path, ty)| format!("{} {path} ({ty})", op.name))
        })
        .collect();
    assert!(read_only.is_empty(), "fields no control edits:\n{}", read_only.join("\n"));
}
