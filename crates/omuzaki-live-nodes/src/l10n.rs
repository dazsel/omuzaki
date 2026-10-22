//! The sentences the first-party nodes say to people, with the `live-nodes-` prefix (ADR 0023).

omuzaki_l10n_api::messages! {
    /// `on.event` names a kind that isn't a valid event kind.
    pub bad_event_kind = "live-nodes-bad-event-kind" (kind: Verbatim, reason: Verbatim)
        => "{ $kind } isn't an event kind: { $reason }";
    /// `on.event` names a kind nothing in this build publishes.
    pub unknown_event_kind = "live-nodes-unknown-event-kind" (kind: Id)
        => "No event kind { $kind } is known; use one that `automation.event_kinds` lists, or custom.<name>.";
    /// A `where` value that isn't text, a number or true/false.
    pub filter_not_scalar = "live-nodes-filter-not-scalar" (path: Verbatim)
        => "The filter on { $path } has to be text, a number, or true or false.";
    /// A `min` value that isn't a finite number.
    pub min_not_finite = "live-nodes-min-not-finite" (path: Verbatim)
        => "The minimum for { $path } has to be a finite number.";
    /// `op.run` names an op this build doesn't have.
    pub unknown_op = "live-nodes-unknown-op" (op: Id)
        => "No op { $op } exists in this build.";
    /// `op.run` names an op automations may not run.
    pub op_not_allowed = "live-nodes-op-not-allowed" (op: Id, reason: Verbatim)
        => "Automations can't run { $op }: { $reason }";
    /// `op.run` has no op chosen yet.
    pub no_op = "live-nodes-no-op" ()
        => "Choose the op this action runs.";
    /// An argument the op doesn't take.
    pub unknown_arg = "live-nodes-unknown-arg" (op: Id, path: Verbatim)
        => "{ $op } takes no argument { $path }.";
    /// A placeholder whose name can't be a port.
    pub bad_placeholder = "live-nodes-bad-placeholder" (path: Verbatim, name: Verbatim)
        => "The placeholder at { $path } names { $name }, which can't be an input's name.";
    /// The arguments, with the inputs filled in, don't decode for the op.
    pub invalid_args = "live-nodes-invalid-args" (op: Id, reason: Verbatim)
        => "The arguments for { $op } don't fit: { $reason }";
    /// The op ran and refused or failed: its error code, then its message.
    pub op_failed = "live-nodes-op-failed" (code: Id, reason: Verbatim)
        => "{ $code }: { $reason }";
    /// Too many op calls are waiting for their answers.
    pub op_limited = "live-nodes-op-limited" (op: Id)
        => "{ $op } wasn't run: too many actions are still waiting for their answers.";
}
