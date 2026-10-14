//! Maps an OAuth token's `scope` claim onto Omuzaki's grants: the token asks for scopes, and the server's
//! ceiling caps what it gets. Names come from [`Scope`]'s own table, so the wire vocabulary has one definition.

use omuzaki_ops_api::{Grants, Scope};

/// Splits an OAuth `scope` claim on spaces, parses each name with [`Scope::from_str`](std::str::FromStr), drops
/// unknown names, dedupes, and keeps first-seen order. Names are exact: `Scope::from_str` decides the case rules, so
/// `READ` is unknown rather than silently mapped to `read`.
pub(crate) fn parse_scopes(claim: &str) -> Vec<Scope> {
    let mut scopes = Vec::new();
    for scope in claim.split(' ').filter_map(|name| name.parse::<Scope>().ok()) {
        if !scopes.contains(&scope) {
            scopes.push(scope);
        }
    }
    scopes
}

/// `Grants::of(token.iter().copied()).intersect(ceiling)`: never more than either side. A file root in the ceiling
/// survives only if the token lists `fs`, because the token's grants hold `fs` without roots.
pub(crate) fn grants_for(token: &[Scope], ceiling: &Grants) -> Grants {
    Grants::of(token.iter().copied()).intersect(ceiling)
}

/// The scopes in `required` that `granted` lacks, in `required`'s order.
pub(crate) fn missing(required: &[Scope], granted: &Grants) -> Vec<Scope> {
    required.iter().copied().filter(|scope| !granted.allows(*scope)).collect()
}

/// Scope names joined by single spaces, for the `scope=` parameter of a `WWW-Authenticate` challenge.
pub(crate) fn scope_param(scopes: &[Scope]) -> String {
    scopes.iter().map(|scope| scope.name()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use proptest::prelude::*;

    use super::*;

    #[test]
    fn parse_keeps_known_names_in_first_seen_order() {
        assert_eq!(parse_scopes("read edit:character openid read"), [Scope::Read, Scope::EditCharacter]);
        assert_eq!(parse_scopes("edit:scene read"), [Scope::EditScene, Scope::Read]);
    }

    #[test]
    fn parse_tolerates_empty_claims_and_extra_spaces() {
        assert_eq!(parse_scopes(""), Vec::<Scope>::new());
        assert_eq!(parse_scopes("   "), Vec::<Scope>::new());
        assert_eq!(parse_scopes("  read   edit:scene  "), [Scope::Read, Scope::EditScene]);
    }

    #[test]
    fn parse_is_case_sensitive() {
        assert_eq!(parse_scopes("READ Edit:Character"), Vec::<Scope>::new());
    }

    #[test]
    fn grants_drop_what_the_ceiling_lacks() {
        let ceiling = Grants::agent_default();
        let granted = grants_for(&[Scope::Read, Scope::Publish, Scope::Render], &ceiling);
        assert_eq!(granted.scopes().collect::<Vec<_>>(), [Scope::Read, Scope::Render]);
    }

    #[test]
    fn a_ceiling_file_root_needs_the_token_to_list_fs() {
        let ceiling = Grants::of([Scope::Read, Scope::Fs]).with_fs_root("/srv/art");
        let without = grants_for(&[Scope::Read], &ceiling);
        assert!(!without.allows(Scope::Fs) && without.fs_roots().is_empty());
        let with = grants_for(&[Scope::Read, Scope::Fs], &ceiling);
        assert!(with.allows(Scope::Fs));
        assert_eq!(with.fs_roots(), ["/srv/art"]);
    }

    #[test]
    fn missing_lists_gaps_in_required_order() {
        let granted = Grants::of([Scope::Read, Scope::EditScene]);
        let required = [Scope::Publish, Scope::Read, Scope::EditScene, Scope::Spend];
        assert_eq!(missing(&required, &granted), [Scope::Publish, Scope::Spend]);
        assert_eq!(missing(&[Scope::Read], &granted), Vec::<Scope>::new());
    }

    #[test]
    fn scope_param_joins_names_with_spaces() {
        assert_eq!(scope_param(&[Scope::Read, Scope::EditCharacter]), "read edit:character");
        assert_eq!(scope_param(&[]), "");
    }

    fn scope_list() -> impl Strategy<Value = Vec<Scope>> {
        // Repeats and any order, as a real claim may have.
        prop::collection::vec(prop::sample::select(Scope::ALL.to_vec()), 0..24)
    }

    proptest! {
        // A fixed seed, so every run checks the same inputs (the determinism standard); see changes.rs.
        #![proptest_config(ProptestConfig {
            failure_persistence: None,
            rng_seed: proptest::test_runner::RngSeed::Fixed(0x5EED),
            ..ProptestConfig::default()
        })]

        #[test]
        fn granted_scopes_are_exactly_token_intersect_ceiling(token in scope_list(), ceiling in scope_list()) {
            let ceiling_grants = Grants::of(ceiling.iter().copied());
            let got: BTreeSet<Scope> = grants_for(&token, &ceiling_grants).scopes().collect();
            let want: BTreeSet<Scope> = token.iter().copied().filter(|s| ceiling.contains(s)).collect();
            prop_assert_eq!(got, want);
        }
    }
}
