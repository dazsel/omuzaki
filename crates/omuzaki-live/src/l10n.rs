//! The sentences the engine says to people, with the `live-` prefix (ADR 0023).

omuzaki_l10n_api::messages! {
    /// A values file that couldn't be read at the start.
    pub values_unreadable = "live-values-unreadable" (which: Verbatim, reason: Verbatim)
        => "The saved { $which } values couldn't be read ({ $reason }), so they start from their defaults.";
    /// A stored value whose type no longer matches its declaration.
    pub value_wrong_type = "live-value-wrong-type" (name: Id)
        => "The saved value of { $name } has another type now, so it starts from its default.";
}
