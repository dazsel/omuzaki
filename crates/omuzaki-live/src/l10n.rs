//! The sentences the engine says to people, with the `live-` prefix (ADR 0023).

omuzaki_l10n_api::messages! {
    /// A values file that couldn't be read at the start.
    pub values_unreadable = "live-values-unreadable" (which: Verbatim, reason: Verbatim)
        => "The saved { $which } values couldn't be read ({ $reason }), so they start from their defaults.";
    /// A stored value whose type no longer matches its declaration.
    pub value_wrong_type = "live-value-wrong-type" (name: Id)
        => "The saved value of { $name } has another type now, so it starts from its default.";
    /// A global another show declared with another type first.
    pub global_type_conflict = "live-global-type-conflict" (name: Id)
        => "The global { $name } already holds a value of another type from another show, so the nodes on it here are inert.";
    /// A node whose kind this build doesn't have.
    pub node_unknown_kind = "live-node-unknown-kind" (graph: Id, node: Id, kind: Id)
        => "{ $node } in { $graph } is a { $kind }, which this build doesn't have, so it is inert.";
    /// A node whose parameters no longer pass.
    pub node_bad_params = "live-node-bad-params" (graph: Id, node: Id, reason: Verbatim)
        => "{ $node } in { $graph } is inert: its parameters don't pass ({ $reason }).";
    /// An `op.run` whose op can't be run.
    pub node_op_not_allowed = "live-node-op-not-allowed" (graph: Id, node: Id, reason: Verbatim)
        => "{ $node } in { $graph } is inert: its op can't run from a show ({ $reason }).";
    /// A wire into an input the node doesn't have.
    pub wire_no_input = "live-wire-no-input" (graph: Id, node: Id, port: Id)
        => "{ $node } in { $graph } is inert: something is wired to { $port }, which it doesn't have.";
    /// A value input with more than one source.
    pub wire_two_sources = "live-wire-two-sources" (graph: Id, node: Id, port: Id)
        => "{ $node } in { $graph } is inert: its value input { $port } has more than one source.";
    /// A wire from an output the source doesn't have.
    pub wire_no_output = "live-wire-no-output" (graph: Id, node: Id, port: Id, source: Id)
        => "{ $node } in { $graph } is inert: { $port } is wired from { $source }, which isn't there.";
    /// A wire between types that never convert.
    pub wire_type = "live-wire-type" (graph: Id, node: Id, port: Id, from: Id, to: Id)
        => "{ $node } in { $graph } is inert: { $port } takes { $to } but is wired from { $from }.";
    /// A `set` constant that doesn't fit its input.
    pub set_unfit = "live-set-unfit" (graph: Id, node: Id, port: Id)
        => "{ $node } in { $graph } is inert: the value set on { $port } doesn't fit it.";
    /// A node on a value cycle.
    pub value_cycle = "live-value-cycle" (graph: Id, node: Id)
        => "{ $node } in { $graph } is inert: its values are wired in a circle.";
    /// A node expecting a variable of another type.
    pub variable_type = "live-variable-type" (graph: Id, node: Id, name: Id, declared: Id, expected: Id)
        => "{ $node } in { $graph } is inert: it expects { $name } to be { $expected }, but the show declares it { $declared }.";
    /// A node naming something the show doesn't declare.
    pub undeclared = "live-undeclared" (graph: Id, node: Id, what: Verbatim)
        => "{ $node } in { $graph } is inert: the show doesn't declare { $what }.";
    /// A trigger on an event kind nobody registered.
    pub unknown_event = "live-unknown-event" (graph: Id, node: Id, kind: Id)
        => "{ $node } in { $graph } is inert: nothing in this build sends { $kind } events.";
    /// A node waiting for approvals.
    pub not_approved = "live-not-approved" (graph: Id, node: Id, needs: Verbatim)
        => "{ $node } in { $graph } waits for your approval on this computer: { $needs }.";
    /// A trigger that listens to nothing.
    pub never_fires = "live-never-fires" (graph: Id, node: Id)
        => "{ $node } in { $graph } listens to nothing, so it never fires.";
    /// A node with no wired event input.
    pub never_runs = "live-never-runs" (graph: Id, node: Id)
        => "Nothing is wired into { $node } in { $graph }, so it never runs.";
    /// An approvals file that couldn't be read.
    pub approvals_unreadable = "live-approvals-unreadable" (reason: Verbatim)
        => "The approvals on this computer couldn't be read ({ $reason }), so nothing is approved until you approve again.";
}
