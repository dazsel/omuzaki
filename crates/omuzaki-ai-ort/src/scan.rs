//! A read-only scan of an ONNX file's protobuf, before any native code sees it (ADR 0016 rule 4).
//!
//! ONNX Runtime has no call for "which operator domains does this model use" or "does it keep tensors in another
//! file", and loading a model is the thing we refuse to do for the wrong ones. So this reads just enough of
//! `ModelProto` by hand: the opset imports, every node's domain (subgraphs included), and every tensor's
//! `data_location`. It allocates only for the domain names it keeps and never trusts a length past the buffer.

use std::collections::BTreeSet;

/// Operator domains that ship inside ONNX Runtime: the standard operators (`""` and `ai.onnx`), ONNX-ML and
/// Microsoft's contrib operators. Any other domain needs a custom-op library, which we never load.
const BUILT_IN_DOMAINS: &[&str] = &["", "ai.onnx", "ai.onnx.ml", "com.microsoft"];

/// Deep enough for any real model's nested `If` / `Loop` bodies, shallow enough to stop a crafted file.
const MAX_DEPTH: usize = 32;

/// What the scan found in an acceptable model.
#[derive(Debug, PartialEq, Eq)]
pub struct Scan {
    /// The highest version of the standard operator set (`""` or `ai.onnx`) the model imports.
    pub opset: Option<u32>,
}

/// Why the scan did not accept the file.
#[derive(Debug, PartialEq, Eq)]
pub enum ScanError {
    /// Not a readable `ModelProto`.
    Malformed(&'static str),
    /// A model that asks for something we never allow; the text is for people.
    Refused(String),
}

/// Scans `bytes` as a `ModelProto`.
///
/// # Errors
/// [`ScanError::Malformed`] when it isn't protobuf of the right shape, [`ScanError::Refused`] for a custom-op domain
/// or tensor data kept outside the file.
pub fn scan_model(bytes: &[u8]) -> Result<Scan, ScanError> {
    let mut domains = BTreeSet::new();
    let mut opset = None;
    let mut has_graph = false;
    for field in Fields::new(bytes) {
        let (number, value) = field?;
        match (number, value) {
            (7, Value::Bytes(graph)) => {
                has_graph = true;
                scan_graph(graph, &mut domains, 0)?;
            }
            (8, Value::Bytes(import)) => {
                let (domain, version) = scan_opset_import(import)?;
                if matches!(domain, "" | "ai.onnx") {
                    opset = opset.max(Some(u32::try_from(version).map_err(|_| ScanError::Malformed("opset version"))?));
                }
                domains.insert(domain.to_owned());
            }
            (25, Value::Bytes(function)) => scan_function(function, &mut domains)?,
            _ => {}
        }
    }
    if !has_graph {
        return Err(ScanError::Malformed("no graph"));
    }
    if let Some(domain) = domains.iter().find(|d| !BUILT_IN_DOMAINS.contains(&d.as_str())) {
        return Err(ScanError::Refused(format!(
            "the model uses the operator domain `{domain}`, which needs a custom-op library; models may only use ONNX Runtime's own operators"
        )));
    }
    Ok(Scan { opset })
}

fn scan_opset_import(bytes: &[u8]) -> Result<(&str, i64), ScanError> {
    let (mut domain, mut version) = ("", 0);
    for field in Fields::new(bytes) {
        match field? {
            (1, Value::Bytes(d)) => domain = utf8(d)?,
            (2, Value::Varint(v)) => version = v.cast_signed(),
            _ => {}
        }
    }
    Ok((domain, version))
}

fn scan_function(bytes: &[u8], domains: &mut BTreeSet<String>) -> Result<(), ScanError> {
    for field in Fields::new(bytes) {
        match field? {
            (7, Value::Bytes(node)) => scan_node(node, domains, 1)?,
            (9, Value::Bytes(import)) => {
                domains.insert(scan_opset_import(import)?.0.to_owned());
            }
            // `attribute_proto`: attributes with default values, which ONNX Runtime substitutes into the function's
            // nodes, so a tensor or subgraph here is as executable as one on a node.
            (11, Value::Bytes(attribute)) => scan_attribute(attribute, domains, 1)?,
            _ => {}
        }
    }
    Ok(())
}

fn scan_graph(bytes: &[u8], domains: &mut BTreeSet<String>, depth: usize) -> Result<(), ScanError> {
    if depth > MAX_DEPTH {
        return Err(ScanError::Malformed("graphs nested too deeply"));
    }
    for field in Fields::new(bytes) {
        match field? {
            (1, Value::Bytes(node)) => scan_node(node, domains, depth)?,
            (5, Value::Bytes(tensor)) => scan_tensor(tensor)?,
            (15, Value::Bytes(sparse)) => scan_sparse(sparse)?,
            _ => {}
        }
    }
    Ok(())
}

fn scan_node(bytes: &[u8], domains: &mut BTreeSet<String>, depth: usize) -> Result<(), ScanError> {
    for field in Fields::new(bytes) {
        match field? {
            (7, Value::Bytes(domain)) => {
                domains.insert(utf8(domain)?.to_owned());
            }
            (5, Value::Bytes(attribute)) => scan_attribute(attribute, domains, depth)?,
            _ => {}
        }
    }
    Ok(())
}

fn scan_attribute(bytes: &[u8], domains: &mut BTreeSet<String>, depth: usize) -> Result<(), ScanError> {
    for field in Fields::new(bytes) {
        match field? {
            (5 | 10, Value::Bytes(tensor)) => scan_tensor(tensor)?,
            (6 | 11, Value::Bytes(graph)) => scan_graph(graph, domains, depth + 1)?,
            (22 | 23, Value::Bytes(sparse)) => scan_sparse(sparse)?,
            _ => {}
        }
    }
    Ok(())
}

fn scan_sparse(bytes: &[u8]) -> Result<(), ScanError> {
    for field in Fields::new(bytes) {
        if let (1 | 2, Value::Bytes(tensor)) = field? {
            scan_tensor(tensor)?;
        }
    }
    Ok(())
}

fn scan_tensor(bytes: &[u8]) -> Result<(), ScanError> {
    for field in Fields::new(bytes) {
        // `external_data` entries (13) or `data_location` (14) = EXTERNAL (1): the weights live in another file.
        if matches!(field?, (13, Value::Bytes(_)) | (14, Value::Varint(1))) {
            return Err(ScanError::Refused(
                "the model keeps tensor data in another file (ONNX external data); models must be one self-contained file".to_owned(),
            ));
        }
    }
    Ok(())
}

fn utf8(bytes: &[u8]) -> Result<&str, ScanError> {
    std::str::from_utf8(bytes).map_err(|_| ScanError::Malformed("a name isn't UTF-8"))
}

/// One protobuf field's payload; fixed-width fields are skipped by the reader.
enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Skipped,
}

/// Walks the fields of one protobuf message.
struct Fields<'a> {
    rest: &'a [u8],
}

impl<'a> Fields<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    fn varint(&mut self) -> Result<u64, ScanError> {
        let mut value = 0u64;
        for (i, &byte) in self.rest.iter().enumerate().take(10) {
            value |= u64::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                self.rest = &self.rest[i + 1..];
                return Ok(value);
            }
        }
        Err(ScanError::Malformed("bad varint"))
    }

    fn take(&mut self, len: u64) -> Result<&'a [u8], ScanError> {
        let len = usize::try_from(len).map_err(|_| ScanError::Malformed("length past the end"))?;
        if len > self.rest.len() {
            return Err(ScanError::Malformed("length past the end"));
        }
        let (head, tail) = self.rest.split_at(len);
        self.rest = tail;
        Ok(head)
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u64, Value<'a>), ScanError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let item = (|| {
            let key = self.varint()?;
            let value = match key & 7 {
                0 => Value::Varint(self.varint()?),
                1 => {
                    self.take(8)?;
                    Value::Skipped
                }
                2 => {
                    let len = self.varint()?;
                    Value::Bytes(self.take(len)?)
                }
                5 => {
                    self.take(4)?;
                    Value::Skipped
                }
                _ => return Err(ScanError::Malformed("unsupported protobuf wire type")),
            };
            Ok((key >> 3, value))
        })();
        if item.is_err() {
            self.rest = &[]; // one error ends the walk
        }
        Some(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: &[u8] = include_bytes!("../tests/fixtures/identity.onnx");
    const ADD: &[u8] = include_bytes!("../tests/fixtures/add.onnx");
    const CUSTOM_OP: &[u8] = include_bytes!("../tests/fixtures/custom_op.onnx");
    const EXTERNAL: &[u8] = include_bytes!("../tests/fixtures/external_data.onnx");

    #[test]
    fn plain_models_pass_and_report_their_opset() {
        assert_eq!(scan_model(IDENTITY), Ok(Scan { opset: Some(13) }));
        assert_eq!(scan_model(ADD), Ok(Scan { opset: Some(13) }));
    }

    #[test]
    fn a_custom_op_domain_is_refused_by_name() {
        match scan_model(CUSTOM_OP) {
            Err(ScanError::Refused(why)) => assert!(why.contains("com.example.custom"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn external_data_is_refused() {
        match scan_model(EXTERNAL) {
            Err(ScanError::Refused(why)) => assert!(why.contains("external data"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn garbage_and_truncated_files_are_malformed_not_panics() {
        assert!(matches!(scan_model(b""), Err(ScanError::Malformed(_))));
        assert!(matches!(scan_model(b"not an onnx file at all"), Err(ScanError::Malformed(_))));
        for cut in 1..IDENTITY.len() {
            // Every prefix either fails cleanly or (when the cut falls on a field boundary) still parses.
            let _ = scan_model(&IDENTITY[..cut]);
        }
        assert!(matches!(scan_model(&IDENTITY[..IDENTITY.len() - 1]), Err(ScanError::Malformed(_))));
    }

    /// A length-delimited field `number` holding `payload`.
    fn field(number: u64, payload: &[u8]) -> Vec<u8> {
        let varint = |mut v: u64| {
            let mut out = Vec::new();
            loop {
                let byte = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    out.push(byte);
                    return out;
                }
                out.push(byte | 0x80);
            }
        };
        [varint(number << 3 | 2), varint(payload.len() as u64), payload.to_vec()].concat()
    }

    /// A model whose only content is an empty graph and a function carrying `function_field` (a `FunctionProto` field).
    fn model_with_function(function_field: &[u8]) -> Vec<u8> {
        [field(7, &[]), field(25, function_field)].concat()
    }

    #[test]
    fn external_data_in_a_function_default_attribute_is_refused() {
        let tensor = [0x70u8, 1]; // TensorProto.data_location (14) = EXTERNAL (1)
        let model = model_with_function(&field(11, &field(5, &tensor)));
        match scan_model(&model) {
            Err(ScanError::Refused(why)) => assert!(why.contains("external data"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_custom_domain_node_in_a_function_default_attribute_subgraph_is_refused() {
        let node = field(7, b"com.example.custom");
        let model = model_with_function(&field(11, &field(6, &field(1, &node))));
        match scan_model(&model) {
            Err(ScanError::Refused(why)) => assert!(why.contains("com.example.custom"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_function_with_a_plain_default_attribute_is_fine() {
        let model = model_with_function(&field(11, &[0x18, 5])); // AttributeProto.i (3) = 5
        assert_eq!(scan_model(&model), Ok(Scan { opset: None }));
    }

    #[test]
    fn deeply_nested_graphs_are_stopped() {
        // graph (1) -> node -> attribute (5) -> graph (6) -> ..., a model whose graph nests 60 levels.
        let mut graph = Vec::new();
        for _ in 0..60 {
            graph = field(1, &field(5, &field(6, &graph)));
        }
        let model = field(7, &graph);
        assert_eq!(scan_model(&model), Err(ScanError::Malformed("graphs nested too deeply")));
    }
}
