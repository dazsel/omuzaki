//! The sentences `omuzaki-live-api` says to people: structural findings, node errors and parameter errors, each a
//! [`Text`](omuzaki_l10n_api::Text) with the `live-api-` prefix (ADR 0023). The node and event registries' own text
//! (`node-`, `event-`) is derived from the registrations, not declared here.

omuzaki_l10n_api::messages! {
    // ── Structural findings (design D18, first column) ──────────────────────────────────────────────────────────

    /// A wire names a node its graph doesn't have.
    pub wire_missing_node = "live-api-wire-missing-node" (graph: Id, node: Id, port: Id, source: Id)
        => "In { $graph }, { $node }.{ $port } is wired from { $source }, but there is no node by that name.";
    /// A wire's source list is empty.
    pub wire_empty = "live-api-wire-empty" (graph: Id, node: Id, port: Id)
        => "In { $graph }, { $node }.{ $port } has an empty list of sources.";
    /// An input is both wired and given a constant.
    pub port_wired_and_set = "live-api-port-wired-and-set" (graph: Id, node: Id, port: Id)
        => "In { $graph }, { $node }.{ $port } is both wired and set to a constant; keep one.";
    /// A graph is in a group the show doesn't declare.
    pub graph_unknown_group = "live-api-graph-unknown-group" (graph: Id, group: Id)
        => "Graph { $graph } is in group { $group }, which the show doesn't declare.";
    /// A variable's default doesn't have its declared type.
    pub variable_default_type = "live-api-variable-default-type" (variable: Id, expected: Id)
        => "The default of variable { $variable } isn't a { $expected }.";
    /// The show has more graphs than allowed.
    pub too_many_graphs = "live-api-too-many-graphs" (count: Int, max: Int)
        => "The show has { $count } graphs; at most { $max } are allowed.";
    /// A graph has more nodes than allowed.
    pub too_many_nodes = "live-api-too-many-nodes" (graph: Id, count: Int, max: Int)
        => "Graph { $graph } has { $count } nodes; at most { $max } are allowed.";
    /// A node names more ports than allowed.
    pub too_many_ports = "live-api-too-many-ports" (graph: Id, node: Id, count: Int, max: Int)
        => "In { $graph }, { $node } names { $count } ports; at most { $max } are allowed.";
    /// The show declares more variables than allowed.
    pub too_many_variables = "live-api-too-many-variables" (count: Int, max: Int)
        => "The show declares { $count } variables; at most { $max } are allowed.";
    /// A node's parameters are larger than allowed.
    pub params_too_large = "live-api-params-too-large" (graph: Id, node: Id, bytes: Int, max: Int)
        => "In { $graph }, the parameters of { $node } take { $bytes } bytes; at most { $max } are allowed.";
    /// A show key whose owner this build doesn't include.
    pub key_kept_unchecked = "live-api-key-kept-unchecked" (key: Id)
        => "{ $key } belongs to a feature this build doesn't include: it is kept as it is, not checked.";
    /// A key under a show that can't be used.
    pub key_refused = "live-api-key-refused" (key: Id, reason: Verbatim)
        => "{ $key } can't be kept under a show: { $reason }";

    // ── Node errors (design D4) ─────────────────────────────────────────────────────────────────────────────────

    /// A node named a port it doesn't have.
    pub unknown_port = "live-api-unknown-port" (port: Id)
        => "This node has no port { $port }.";
    /// A value of the wrong type for a port.
    pub type_mismatch = "live-api-type-mismatch" (port: Id, expected: Id, found: Id)
        => "{ $port } takes { $expected }, not { $found }.";
    /// A value breaks a rule every value keeps (finite numbers, text and list lengths).
    pub value_invalid = "live-api-value-invalid" (port: Id, reason: Verbatim)
        => "The value for { $port } can't be used: { $reason }";
    /// A number that has to be finite isn't.
    pub not_finite = "live-api-not-finite" (what: Id)
        => "{ $what } must be a finite number.";
    /// An engine limit was hit.
    pub limit = "live-api-limit" (limit: Id, max: Int)
        => "The limit { $limit } of { $max } was reached.";
    /// A port was read as a live output but isn't one.
    pub not_live = "live-api-not-live" (port: Id)
        => "{ $port } isn't an output computed when read.";
    /// A node got an input it doesn't handle.
    pub unhandled = "live-api-unhandled" (input: Id)
        => "This node doesn't handle the { $input } input; update the node.";
    /// A node named a variable or group the show doesn't declare.
    pub undeclared = "live-api-undeclared" (name: Id)
        => "The show doesn't declare { $name }.";
    /// A node emitted an event whose payload doesn't fit its kind.
    pub payload_invalid = "live-api-payload-invalid" (kind: Id, reason: Verbatim)
        => "The payload for { $kind } can't be used: { $reason }";
    /// A fault inside a node or its host.
    pub internal = "live-api-internal" (reason: Verbatim)
        => "Something went wrong inside the node: { $reason }";

    // ── Parameters and ports (design D4) ────────────────────────────────────────────────────────────────────────

    /// Parameters that don't match the kind's schema.
    pub params_invalid = "live-api-params-invalid" (reason: Verbatim)
        => "The parameters don't fit this kind: { $reason }";
    /// Parameters that aren't a table.
    pub params_not_object = "live-api-params-not-object" ()
        => "The parameters must be a table.";
    /// A node kind nobody registered.
    pub unknown_node_kind = "live-api-unknown-node-kind" (kind: Id)
        => "No node kind { $kind } is installed.";
    /// Two ports in one direction share a name.
    pub port_duplicate = "live-api-port-duplicate" (port: Id)
        => "Two ports are called { $port }.";
    /// A kind declares more ports than allowed.
    pub port_too_many = "live-api-port-too-many" (count: Int, max: Int)
        => "The kind declares { $count } ports; at most { $max } are allowed.";
    /// A port name isn't valid.
    pub port_bad_name = "live-api-port-bad-name" (name: Verbatim)
        => "{ $name } isn't a valid port name.";
    /// An event port declares a default or is live.
    pub port_event_value = "live-api-port-event-value" (port: Id)
        => "Event port { $port } can't have a default or be computed when read.";
    /// A live port that isn't a value output.
    pub port_live_input = "live-api-port-live-input" (port: Id)
        => "Only value outputs can be computed when read, and { $port } is an input.";
    /// A default on an output.
    pub port_output_default = "live-api-port-output-default" (port: Id)
        => "Output { $port } can't have a default.";
    /// A default of the wrong type.
    pub port_default_type = "live-api-port-default-type" (port: Id, expected: Id)
        => "The default of { $port } isn't a { $expected }.";
    /// A pure kind with an event port.
    pub port_pure_event = "live-api-port-pure-event" (port: Id)
        => "A pure node has no event ports, and { $port } is one.";
    /// A pure kind with an output that isn't live.
    pub port_pure_not_live = "live-api-port-pure-not-live" (port: Id)
        => "Every output of a pure node is computed when read, and { $port } isn't.";
    /// A trigger with an event input.
    pub port_trigger_event_in = "live-api-port-trigger-event-in" (port: Id)
        => "A trigger starts on events it listens to, so it has no event input like { $port }.";
}
