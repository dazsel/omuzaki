//! Synthetic TFLite models, built in the test with the official FlatBuffers builder: no model file is read or
//! committed. A model is a tree of [`Table`]s whose fields are set by slot (the order of the fields in
//! `tflite/converter/schema/schema.fbs` at LiteRT v2.3.0), so a test can set any field, including one the schema
//! doesn't have yet. Depends on `flatbuffers` only, so the scan's fuzz target can include this file to seed its corpus.
#![allow(dead_code)] // each test binary uses some of it

use flatbuffers::{FlatBufferBuilder, TableFinishedWIPOffset, UnionWIPOffset, WIPOffset};

/// One field's value.
#[derive(Clone, Debug)]
pub enum Value {
    /// `byte` (and the `TensorType` enum).
    I8(i8),
    /// `bool`.
    Bool(bool),
    /// `int`.
    I32(i32),
    /// `uint`.
    U32(u32),
    /// `ulong`.
    U64(u64),
    /// `string`.
    Str(&'static str),
    /// `[int]`.
    Ints(Vec<i32>),
    /// `[ubyte]`.
    Bytes(Vec<u8>),
    /// `[bool]`.
    Bools(Vec<bool>),
    /// A table.
    Table(Table),
    /// `[Table]`.
    Tables(Vec<Table>),
}

/// A table: its fields by slot.
#[derive(Clone, Debug, Default)]
pub struct Table(pub Vec<(u16, Value)>);

impl Table {
    /// Sets field `slot`, replacing it if set.
    pub fn set(&mut self, slot: u16, value: Value) -> &mut Self {
        self.0.retain(|(s, _)| *s != slot);
        self.0.push((slot, value));
        self
    }

    /// The `index`th table of the vector at `slot`.
    pub fn at(&mut self, slot: u16, index: usize) -> &mut Table {
        match self.0.iter_mut().find(|(s, _)| *s == slot) {
            Some((_, Value::Tables(tables))) => &mut tables[index],
            _ => panic!("no table vector in slot {slot}"),
        }
    }
}

/// The model's slots this file builds with (schema order, from 0).
pub mod slot {
    /// `Model.operator_codes`.
    pub const CODES: u16 = 1;
    /// `Model.subgraphs`.
    pub const SUBGRAPHS: u16 = 2;
    /// `Model.buffers`.
    pub const BUFFERS: u16 = 4;
    /// `SubGraph.tensors`.
    pub const TENSORS: u16 = 0;
    /// `SubGraph.operators`.
    pub const OPERATORS: u16 = 3;
}

/// A tensor of `FLOAT32` named `name` with `shape`, on the empty buffer 0.
pub fn tensor(name: &'static str, shape: &[i32]) -> Table {
    Table(vec![(0, Value::Ints(shape.to_vec())), (1, Value::I8(0)), (2, Value::U32(0)), (3, Value::Str(name))])
}

/// An operator code for the builtin `code`, version 1.
pub fn builtin(code: i32) -> Table {
    let deprecated = i8::try_from(code).unwrap_or(127);
    Table(vec![(0, Value::I8(deprecated)), (2, Value::I32(1)), (3, Value::I32(code))])
}

/// The fixed-shape `ADD` graph: `out = a + b`, each `[1, 4]` `FLOAT32`, one subgraph, one operator.
pub fn add_graph() -> Table {
    let operator = Table(vec![(0, Value::U32(0)), (1, Value::Ints(vec![0, 1])), (2, Value::Ints(vec![2]))]);
    let subgraph = Table(vec![
        (0, Value::Tables(vec![tensor("a", &[1, 4]), tensor("b", &[1, 4]), tensor("out", &[1, 4])])),
        (1, Value::Ints(vec![0, 1])),
        (2, Value::Ints(vec![2])),
        (3, Value::Tables(vec![operator])),
    ]);
    Table(vec![
        (0, Value::U32(3)),
        (1, Value::Tables(vec![builtin(0)])),
        (2, Value::Tables(vec![subgraph])),
        (4, Value::Tables(vec![Table::default()])),
    ])
}

/// The subgraph's `index`th tensor of `model`.
pub fn tensor_of(model: &mut Table, index: usize) -> &mut Table {
    model.at(slot::SUBGRAPHS, 0).at(slot::TENSORS, index)
}

/// The subgraph's first operator.
pub fn operator_of(model: &mut Table) -> &mut Table {
    model.at(slot::SUBGRAPHS, 0).at(slot::OPERATORS, 0)
}

/// The model's bytes, with the `TFL3` file identifier.
pub fn bytes(model: &Table) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let root = write(&mut builder, model);
    builder.finish(root, Some("TFL3"));
    builder.finished_data().to_vec()
}

fn write(builder: &mut FlatBufferBuilder<'_>, table: &Table) -> WIPOffset<TableFinishedWIPOffset> {
    // Children before the table: a FlatBuffer table can't be open while its children are written.
    let children: Vec<Option<WIPOffset<UnionWIPOffset>>> = table
        .0
        .iter()
        .map(|(_, value)| match value {
            Value::Str(s) => Some(builder.create_string(s).as_union_value()),
            Value::Ints(v) => Some(builder.create_vector(v).as_union_value()),
            Value::Bytes(v) => Some(builder.create_vector(v).as_union_value()),
            Value::Bools(v) => Some(builder.create_vector(v).as_union_value()),
            Value::Table(t) => Some(write(builder, t).as_union_value()),
            Value::Tables(ts) => {
                let offsets: Vec<_> = ts.iter().map(|t| write(builder, t)).collect();
                Some(builder.create_vector(&offsets).as_union_value())
            }
            _ => None,
        })
        .collect();
    let start = builder.start_table();
    for ((slot, value), child) in table.0.iter().zip(children) {
        let at = 4 + 2 * slot;
        match (value, child) {
            (_, Some(offset)) => builder.push_slot_always(at, offset),
            (Value::I8(x), None) => builder.push_slot_always(at, *x),
            (Value::Bool(x), None) => builder.push_slot_always(at, *x),
            (Value::I32(x), None) => builder.push_slot_always(at, *x),
            (Value::U32(x), None) => builder.push_slot_always(at, *x),
            (Value::U64(x), None) => builder.push_slot_always(at, *x),
            _ => unreachable!("every vector, string and table has an offset"),
        }
    }
    builder.end_table(start)
}
