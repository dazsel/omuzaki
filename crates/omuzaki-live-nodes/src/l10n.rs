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
    /// `event.emit` names a kind that isn't `custom.<name>`.
    pub emit_not_custom = "live-nodes-emit-not-custom" (kind: Verbatim)
        => "An automation can only send custom events: use custom.<name>, not { $kind }.";
    /// `event.emit`'s payload isn't an object.
    pub emit_not_object = "live-nodes-emit-not-object" (found: Verbatim)
        => "The payload has to be an object, not { $found }.";
    /// The engine refused the event (too many hops, too large).
    pub emit_refused = "live-nodes-emit-refused" (reason: Verbatim)
        => "The event wasn't sent: { $reason }";
    /// A list or count outside its allowed range.
    pub count_out_of_range = "live-nodes-count-out-of-range" (count: Int, min: Int, max: Int)
        => "Give between { $min } and { $max }, not { $count }.";
    /// A weight that is negative or not a finite number.
    pub bad_weight = "live-nodes-bad-weight" ()
        => "A weight has to be a number of at least 0.";
    /// Weights that add up to nothing.
    pub weights_sum_zero = "live-nodes-weights-sum-zero" ()
        => "At least one weight has to be above 0.";
    /// `math.compare` on text with an ordering.
    pub text_compare_op = "live-nodes-text-compare-op" ()
        => "Text compares only as equal (==) or not equal (!=).";
    /// A negative or non-finite epsilon.
    pub bad_epsilon = "live-nodes-bad-epsilon" ()
        => "The tolerance has to be a number of at least 0.";
    /// A random range whose minimum is above its maximum.
    pub range_backwards = "live-nodes-range-backwards" (min: Num, max: Num)
        => "The minimum { $min } is above the maximum { $max }.";
    /// A template with more placeholders than allowed.
    pub too_many_placeholders = "live-nodes-too-many-placeholders" (max: Int)
        => "A template takes at most { $max } different placeholders.";
    /// A template with `{` and no `}`.
    pub template_unclosed = "live-nodes-template-unclosed" ()
        => "A placeholder starts with {, but no } ends it; write {{ for a brace.";
    /// A template with a lone `}`.
    pub template_stray_close = "live-nodes-template-stray-close" ()
        => "There is a } with no { before it; write }} for a brace.";
    /// A placeholder whose name can't be an input's name.
    pub template_bad_name = "live-nodes-template-bad-name" (name: Verbatim)
        => "The placeholder {{{ $name }}} has to be lowercase letters, digits and _, starting with a letter.";
    /// `text.to_number` on empty text.
    pub not_a_number_empty = "live-nodes-not-a-number-empty" ()
        => "The text is empty.";
    /// `text.to_number` on text that isn't a number.
    pub not_a_number = "live-nodes-not-a-number" (text: Verbatim)
        => "{ $text } isn't a number.";
    /// A variable node whose kind needs another variable type.
    pub var_type_needed = "live-nodes-var-type-needed" (want: Id, found: Id)
        => "This node needs a { $want } variable, not { $found }.";
    /// A variable write the show refused.
    pub var_refused = "live-nodes-var-refused" (variable: Id, reason: Verbatim)
        => "{ $variable } wasn't changed: { $reason }";
    /// A variable node with no variable chosen.
    pub var_missing = "live-nodes-var-missing" ()
        => "Choose a variable.";
    /// `var.add` whose sum overflowed.
    pub sum_not_finite = "live-nodes-sum-not-finite" ()
        => "The sum is too large to be a number.";
    /// A group node with no group chosen.
    pub group_missing = "live-nodes-group-missing" ()
        => "Choose a group.";
    /// A group switch the show refused.
    pub group_refused = "live-nodes-group-refused" (group: Id, reason: Verbatim)
        => "{ $group } wasn't switched: { $reason }";
    /// A chat command's name, alias or prefix with a space in it, or empty.
    pub command_not_one_word = "live-nodes-command-not-one-word" ()
        => "This has to be one word, with no spaces.";
    /// A cooldown that isn't a number of seconds from 0 up.
    pub bad_seconds = "live-nodes-bad-seconds" ()
        => "This has to be a number of seconds, 0 or more.";
}
