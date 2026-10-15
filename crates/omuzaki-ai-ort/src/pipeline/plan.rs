//! The wiring of a multi-graph model, checked from the graphs' signatures alone (no ONNX Runtime needed), so `probe`
//! and `load` share one set of rules and the rules are unit-tested without a library.

use omuzaki_ai_api::{DType, Dim, LoadError, Signature, Stage, TensorSpec};

/// Where one stage input reads from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Source {
    /// A slot input, by name.
    Slot(String),
    /// An earlier stage's output: the stage and the position of the port in its signature.
    Stage { stage: usize, port: usize },
}

/// One stage's wiring.
#[derive(Debug)]
pub(super) struct StagePlan {
    /// Where each graph input reads from, in the graph's input order.
    pub inputs: Vec<Source>,
}

/// The checked wiring of a whole pipeline.
#[derive(Debug)]
pub(super) struct Plan {
    /// One entry per stage, in run order.
    pub stages: Vec<StagePlan>,
    /// What the pipeline presents before the slot's `io` renames: the slot inputs the stages read, in the order they
    /// are first read, and every stage output as `<stage>.<port>`.
    pub signature: Signature,
}

fn wiring_error(stage: &str, input: &str, source: &str, why: impl std::fmt::Display) -> LoadError {
    LoadError::Format(format!("stage `{stage}` input `{input}` reads `{source}`: {why}"))
}

/// Whether a reader's port can take what a producer's port gives. Dimensions only conflict when both are fixed:
/// a dynamic one is checked on every run.
fn fits(have: &TensorSpec, want: &TensorSpec) -> Result<(), String> {
    if have.dtype != want.dtype {
        return Err(type_error(have.dtype, want.dtype));
    }
    if have.shape.len() != want.shape.len() {
        return Err(format!("it has {} dimensions, the input has {}", have.shape.len(), want.shape.len()));
    }
    for (axis, (h, w)) in have.shape.iter().zip(&want.shape).enumerate() {
        if let (Dim::Fixed(h), Dim::Fixed(w)) = (h, w)
            && h != w
        {
            return Err(format!("dimension {axis} is {h}, the input needs {w}"));
        }
    }
    Ok(())
}

fn type_error(have: DType, want: DType) -> String {
    format!("it is {have:?}, the input is {want:?}")
}

/// Adds the slot input a stage reads, or tightens the one already there (a fixed dimension wins over a dynamic one).
fn read_slot_input(slot_inputs: &mut Vec<TensorSpec>, name: &str, wanted: &TensorSpec) -> Result<(), String> {
    let Some(known) = slot_inputs.iter_mut().find(|s| s.name == name) else {
        slot_inputs.push(TensorSpec { name: name.to_owned(), ..wanted.clone() });
        return Ok(());
    };
    fits(known, wanted).map_err(|why| format!("another stage reads it differently: {why}"))?;
    for (kept, new) in known.shape.iter_mut().zip(&wanted.shape) {
        if matches!(kept, Dim::Any(_)) {
            *kept = *new;
        }
    }
    Ok(())
}

/// Checks `stages` against the signatures of their graphs (same order) and returns the wiring.
///
/// # Errors
/// [`LoadError::Format`] naming the stage, its input and the source: a duplicate stage name, an input name the graph
/// lacks, a graph input nothing feeds, a source that is not an earlier stage's output, or a type that does not fit.
pub(super) fn plan(stages: &[Stage], graphs: &[Signature]) -> Result<Plan, LoadError> {
    let mut slot_inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut plans: Vec<StagePlan> = Vec::with_capacity(stages.len());
    for (index, (stage, graph)) in stages.iter().zip(graphs).enumerate() {
        if stages[..index].iter().any(|earlier| earlier.name == stage.name) {
            return Err(LoadError::Format(format!("two stages are called `{}`", stage.name)));
        }
        if let Some(name) = stage.inputs.keys().find(|name| !graph.inputs.iter().any(|port| &port.name == *name)) {
            let source = &stage.inputs[name];
            return Err(wiring_error(&stage.name, name, source, "the graph has no such input"));
        }
        let mut sources = Vec::with_capacity(graph.inputs.len());
        for port in &graph.inputs {
            let source = stage.inputs.get(&port.name).ok_or_else(|| {
                LoadError::Format(format!("stage `{}` input `{}` is not wired to anything", stage.name, port.name))
            })?;
            let err = |why: String| wiring_error(&stage.name, &port.name, source, why);
            sources.push(match Stage::split_source(source) {
                None => {
                    read_slot_input(&mut slot_inputs, source, port).map_err(err)?;
                    Source::Slot(source.clone())
                }
                Some((from, output)) => {
                    let from_stage = stages[..index]
                        .iter()
                        .position(|earlier| earlier.name == from)
                        .ok_or_else(|| err(format!("`{from}` is not an earlier stage")))?;
                    let position = graphs[from_stage]
                        .outputs
                        .iter()
                        .position(|o| o.name == output)
                        .ok_or_else(|| err(format!("stage `{from}` has no output `{output}`")))?;
                    fits(&graphs[from_stage].outputs[position], port).map_err(err)?;
                    Source::Stage { stage: from_stage, port: position }
                }
            });
        }
        outputs.extend(
            graph.outputs.iter().map(|o| TensorSpec { name: format!("{}.{}", stage.name, o.name), ..o.clone() }),
        );
        plans.push(StagePlan { inputs: sources });
    }
    Ok(Plan { stages: plans, signature: Signature { inputs: slot_inputs, outputs } })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // a panic is a test failure
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn spec(name: &str, dtype: DType, dims: &[usize]) -> TensorSpec {
        TensorSpec::new(name, dtype, dims.iter().map(|&d| Dim::Fixed(d)).collect::<Vec<_>>())
    }

    fn stage(name: &str, inputs: &[(&str, &str)]) -> Stage {
        Stage {
            name: name.to_owned(),
            file: format!("{name}.onnx"),
            inputs: inputs.iter().map(|&(k, v)| (k.to_owned(), v.to_owned())).collect::<BTreeMap<_, _>>(),
        }
    }

    fn graph(inputs: &[TensorSpec], outputs: &[TensorSpec]) -> Signature {
        Signature { inputs: inputs.to_vec(), outputs: outputs.to_vec() }
    }

    fn two() -> (Vec<Stage>, Vec<Signature>) {
        let f = |n: &str| spec(n, DType::F32, &[1, 4]);
        (
            vec![stage("a", &[("x", "x")]), stage("b", &[("p", "a.y"), ("q", "x")])],
            vec![graph(&[f("x")], &[f("y")]), graph(&[f("p"), f("q")], &[f("z")])],
        )
    }

    fn message(result: Result<Plan, LoadError>) -> String {
        result.expect_err("the wiring should be refused").to_string()
    }

    #[test]
    fn a_valid_pipeline_reports_its_slot_inputs_and_prefixed_outputs() {
        let (stages, graphs) = two();
        let plan = plan(&stages, &graphs).unwrap();
        assert_eq!(plan.signature.inputs, [spec("x", DType::F32, &[1, 4])]);
        let names: Vec<_> = plan.signature.outputs.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["a.y", "b.z"]);
        assert_eq!(plan.stages[1].inputs, [Source::Stage { stage: 0, port: 0 }, Source::Slot("x".to_owned())]);
    }

    #[test]
    fn a_missing_port_names_the_stage_input_and_source() {
        let (mut stages, graphs) = two();
        stages[1].inputs.insert("p".to_owned(), "a.nope".to_owned());
        let text = message(plan(&stages, &graphs));
        assert!(text.contains("stage `b`") && text.contains("input `p`") && text.contains("`a.nope`"), "{text}");
    }

    #[test]
    fn a_later_or_unknown_stage_is_not_a_source() {
        let (mut stages, graphs) = two();
        stages[0].inputs.insert("x".to_owned(), "b.z".to_owned());
        assert!(message(plan(&stages, &graphs)).contains("`b` is not an earlier stage"));
    }

    #[test]
    fn a_type_mismatch_is_refused() {
        let (stages, mut graphs) = two();
        graphs[0].outputs[0].dtype = DType::I64;
        let text = message(plan(&stages, &graphs));
        assert!(text.contains("stage `b`") && text.contains("I64"), "{text}");
    }

    #[test]
    fn two_fixed_dimensions_that_differ_are_refused_but_a_dynamic_one_fits() {
        let (stages, mut graphs) = two();
        graphs[0].outputs[0].shape = vec![Dim::Fixed(1), Dim::Fixed(5)];
        assert!(message(plan(&stages, &graphs)).contains("dimension 1"));
        graphs[0].outputs[0].shape = vec![Dim::Fixed(1), Dim::ANY];
        assert!(plan(&stages, &graphs).is_ok());
    }

    #[test]
    fn a_slot_input_read_with_two_types_is_refused() {
        let (stages, mut graphs) = two();
        graphs[1].inputs[1].dtype = DType::I64;
        let text = message(plan(&stages, &graphs));
        assert!(text.contains("stage `b`") && text.contains("`x`") && text.contains("another stage"), "{text}");
    }

    #[test]
    fn a_slot_input_read_twice_keeps_the_fixed_dimensions() {
        let (stages, mut graphs) = two();
        graphs[0].inputs[0].shape = vec![Dim::ANY, Dim::Fixed(4)];
        let plan = plan(&stages, &graphs).unwrap();
        assert_eq!(plan.signature.inputs[0].shape, [Dim::Fixed(1), Dim::Fixed(4)]);
    }

    #[test]
    fn an_unwired_input_and_an_unknown_input_name_are_refused() {
        let (mut stages, graphs) = two();
        stages[1].inputs.remove("q");
        assert!(message(plan(&stages, &graphs)).contains("input `q` is not wired"));
        stages[1].inputs.insert("q".to_owned(), "x".to_owned());
        stages[1].inputs.insert("r".to_owned(), "x".to_owned());
        assert!(message(plan(&stages, &graphs)).contains("no such input"));
    }

    #[test]
    fn two_stages_with_one_name_are_refused() {
        let (mut stages, graphs) = two();
        stages[1].name = "a".to_owned();
        assert!(message(plan(&stages, &graphs)).contains("two stages are called `a`"));
    }
}
