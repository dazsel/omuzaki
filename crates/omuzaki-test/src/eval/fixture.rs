//! The fixture file (design 4): the starting project of a task, as an op script rather than a committed folder.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A starting project: ops run as a person actor on an empty project.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    /// Unique across the workspace.
    pub id: String,
    /// One line for people.
    pub summary: String,
    /// The ops that build the project, in order. Arguments may refer to earlier results as `{n.pointer}`.
    pub steps: Vec<Step>,
    /// Names a task's checks can use, such as `{main}`.
    #[serde(default)]
    pub exports: BTreeMap<String, String>,
}

/// One op of the setup script.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// The op name.
    pub op: String,
    /// Its arguments.
    #[serde(default)]
    pub args: Map<String, Value>,
}

/// A reference from a step to an eval-set item: `{ set = "sheets-synthetic-1", item = "<hash>" }` (design 4, 11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetItemRef {
    /// The eval set's id.
    pub set: String,
    /// The item's content hash.
    pub item: String,
}

impl Fixture {
    /// Every eval-set item the steps refer to: steps in order, and within a step by argument name.
    #[must_use]
    pub fn set_items(&self) -> Vec<SetItemRef> {
        let mut found = Vec::new();
        for step in &self.steps {
            collect(&Value::Object(step.args.clone()), &mut found);
        }
        found
    }
}

fn collect(value: &Value, found: &mut Vec<SetItemRef>) {
    match value {
        Value::Object(map) => {
            if let (Some(Value::String(set)), Some(Value::String(item))) = (map.get("set"), map.get("item")) {
                found.push(SetItemRef { set: set.clone(), item: item.clone() });
            }
            map.values().for_each(|v| collect(v, found));
        }
        Value::Array(items) => items.iter().for_each(|v| collect(v, found)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_set_items_nested_in_step_arguments() {
        let fixture: Fixture = toml::from_str(
            r#"
id = "f"
summary = "s"
steps = [
  { op = "a.b", args = { sheet = { set = "sheets-1", item = "b3:aa" }, other = [ { set = "x", item = "b3:bb" } ] } },
  { op = "c.d" },
]
"#,
        )
        .unwrap();
        let mut items = fixture.set_items();
        // Argument keys are visited in sorted order, so compare as a set.
        items.sort_by(|a, b| a.set.cmp(&b.set));
        assert_eq!(
            items,
            vec![
                SetItemRef { set: "sheets-1".into(), item: "b3:aa".into() },
                SetItemRef { set: "x".into(), item: "b3:bb".into() }
            ]
        );
    }

    #[test]
    fn unknown_fields_are_refused() {
        assert!(toml::from_str::<Fixture>("id='f'\nsummary='s'\nsteps=[]\nbogus=1").is_err());
    }
}
