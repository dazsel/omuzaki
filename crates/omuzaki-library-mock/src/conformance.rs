//! What every [`Library`] must do on the fixture set (design section 6). The library under test is loaded with
//! [`fixtures::all`](crate::fixtures::all), then handed to [`run`].

use omuzaki_char_api::CharId;
use omuzaki_library_api::{
    CompatStatus, CompatTarget, ItemId, ItemScope, Library, LibraryError, Query, Section, Usage, Version, VersionReq,
};

use crate::fixtures::{RIVAL_SEED, SIGNER_SEED, shared_bow};

/// A suite fails by panicking, naming the check. This is the one place a failed call becomes that panic.
fn must<T, E: std::fmt::Debug>(what: &str, result: Result<T, E>) -> T {
    result.unwrap_or_else(|e| panic!("conformance: {what}: {e:?}"))
}

fn id(s: &str) -> ItemId {
    must(s, ItemId::new(s))
}

fn v(major: u32, minor: u32, patch: u32) -> Version {
    Version::new(major, minor, patch)
}

/// Runs every check against `library`, which must hold the fixture set.
///
/// # Panics
/// With the failed check's name when the library disagrees with the contract.
pub fn run(library: &dyn Library) {
    resolution_order(library);
    version_selection(library);
    hash_clash_and_publishers(library);
    deterministic_search(library);
    team_scopes_are_one_tier(library);
    unavailable_with_reasons(library);
    refusals(library);
}

/// The compatibility filter (design D1, D13) on the fixture character from [`fixtures::characters`], which the
/// library must be able to answer for. Incompatible items are filtered out, a style mismatch stays with its warning
/// and ranks below an equally good match that fits, and a character the library doesn't know is an error rather than
/// an unfiltered result.
///
/// # Panics
/// With the failed check's name when the library disagrees with the contract.
pub fn compat(library: &dyn Library, character: &CharId) {
    let query =
        Query { compatible_with: Some(CompatTarget::Character(character.clone())), limit: 100, ..Query::default() };
    let page = must("compatible search", library.search(&query));
    let status = |item: &str| page.items.iter().find(|i| i.id.as_str() == item).map(|i| i.compat.clone());
    assert_eq!(status("test/colourways/fits"), Some(CompatStatus::Compatible), "an item that fits stays");
    assert_eq!(status("test/colourways/dusk"), Some(CompatStatus::Compatible), "an item with no compat stays");
    for gone in ["creature-only", "other-template", "needs-wings"] {
        assert_eq!(status(&format!("test/colourways/{gone}")), None, "{gone}: incompatible items drop out");
    }
    assert!(
        matches!(status("test/colourways/gothic"), Some(CompatStatus::StyleMismatch { .. })),
        "a style mismatch stays, with its warning"
    );
    let all = must("unfiltered search", library.search(&Query { limit: 100, ..Query::default() }));
    assert_eq!(page.total, page.items.len());
    assert_eq!(page.total + 3, all.total, "exactly the three incompatible items are filtered");
    assert!(all.items.iter().all(|i| i.compat == CompatStatus::Compatible), "with no target everything is compatible");
    assert_eq!(page.facets.sections.values().sum::<usize>(), page.total, "facets count the filtered result set");
    let styled = Query { text: Some("colourways".into()), ..query.clone() };
    let order: Vec<_> = must("ranked search", library.search(&styled)).items.into_iter().map(|i| i.compat).collect();
    let first_warning = order.iter().position(|c| matches!(c, CompatStatus::StyleMismatch { .. }));
    assert!(
        first_warning.is_none_or(|at| order[at..].iter().all(|c| matches!(c, CompatStatus::StyleMismatch { .. }))),
        "an equally good match that fits ranks above a style mismatch: {order:?}"
    );
    let stranger = Query { compatible_with: Some(CompatTarget::Character(id_of("nobody"))), ..Query::default() };
    assert!(matches!(library.search(&stranger), Err(LibraryError::NotFound(_))), "an unknown character is refused");
}

fn id_of(name: &str) -> CharId {
    must(name, CharId::new(name))
}

/// All teams are one precedence tier: the newest matching version wins across them whatever the teams are called, and
/// two publishers in different teams are ambiguous, in `resolve` and in `versions`.
fn team_scopes_are_one_tier(library: &dyn Library) {
    let tide = id("signer/colourways/tide");
    let got = must("tide resolves", library.resolve(&tide, &VersionReq::Any));
    assert_eq!(
        (got.scope, got.manifest.version.as_str()),
        (ItemScope::Team("studio".to_owned()), "1.1.0"),
        "the newest across teams, not the first team by name"
    );
    assert_eq!(must("tide versions", library.versions(&tide)), vec![v(1, 0, 0), v(1, 1, 0)]);
    let reef = id("shared/colourways/reef");
    assert!(matches!(library.resolve(&reef, &VersionReq::Any), Err(LibraryError::Ambiguous { .. })));
    assert!(matches!(library.versions(&reef), Err(LibraryError::Ambiguous { .. })));
}

/// First-party before the person's library before team scopes, and the newest version within the first scope that
/// has a match.
fn resolution_order(library: &dyn Library) {
    let ember = id("signer/colourways/ember");
    let got = must("ember resolves", library.resolve(&ember, &VersionReq::Any));
    assert_eq!(
        (got.scope, got.manifest.version.as_str()),
        (ItemScope::Mine, "1.1.0"),
        "Mine beats Team even when Team is newer"
    );
    let team = must("only the team holds 1.2.0", library.resolve(&ember, &VersionReq::Exact(v(1, 2, 0))));
    assert_eq!(team.scope, ItemScope::Team("studio".to_owned()));
}

fn version_selection(library: &dyn Library) {
    let ember = id("signer/colourways/ember");
    assert_eq!(must("versions", library.versions(&ember)), vec![v(1, 0, 0), v(1, 1, 0), v(1, 2, 0)], "oldest first");
    let compatible = must("req ^1.0", VersionReq::parse("^1.0"));
    assert_eq!(must("^1.0", library.resolve(&ember, &compatible)).manifest.version, "1.1.0");
    let exact = must("describe 1.0.0", library.describe(&ember, Some(&v(1, 0, 0))));
    assert_eq!(exact.info.version, v(1, 0, 0));
    assert_eq!(must("describe newest", library.describe(&ember, None)).info.version, v(1, 1, 0));
    match library.resolve(&ember, &VersionReq::Exact(v(9, 0, 0))) {
        Err(LibraryError::NoMatchingVersion { installed, .. }) => {
            assert_eq!(installed, vec![v(1, 0, 0), v(1, 1, 0), v(1, 2, 0)]);
        }
        other => panic!("an absent version must say what is installed, got {other:?}"),
    }
}

/// Two publishers holding one id and version are never silently merged: the call is ambiguous, both manifests stay
/// reachable by hash and their hashes differ.
fn hash_clash_and_publishers(library: &dyn Library) {
    let bow = id("shared/colourways/bow");
    match library.resolve(&bow, &VersionReq::Any) {
        Err(LibraryError::Ambiguous { candidates, .. }) => assert_eq!(candidates.len(), 2, "{candidates:?}"),
        other => panic!("two publishers must be ambiguous, got {other:?}"),
    }
    assert!(matches!(library.versions(&bow), Err(LibraryError::Ambiguous { .. })));
    let (a, b) = (shared_bow(SIGNER_SEED), shared_bow(RIVAL_SEED));
    assert_ne!(a.hash(), b.hash());
    for m in [&a, &b] {
        assert_eq!(must("by hash", library.by_hash(&m.hash())).manifest, *m);
    }
}

fn deterministic_search(library: &dyn Library) {
    let all = Query { limit: 100, ..Query::default() };
    let first = must("search", library.search(&all));
    assert_eq!(first, must("search again", library.search(&all)), "same query, same page");
    assert_eq!(first.total, first.items.len());
    assert_eq!(first.facets.sections.values().sum::<usize>(), first.total, "facets count the whole result set");
    let order: Vec<_> = first.items.iter().map(|i| (i.section, i.id.clone())).collect();
    assert!(order.windows(2).all(|w| w[0] <= w[1]), "ties go by section order, then id: {order:?}");
    let page = must("page", library.search(&Query { offset: 2, limit: 3, ..Query::default() }));
    assert_eq!((page.total, page.items), (first.total, first.items[2..5].to_vec()), "pages slice the same ranking");
    let text = must("text", library.search(&Query { text: Some("RIBBON".into()), limit: 10, ..Query::default() }));
    assert_eq!(text.items.first().map(|i| i.id.as_str()), Some("test/hair/ribbon"), "case-insensitive text");
    let section = must("list", library.list(Some(Section::Colourways), None, 0, 100));
    assert!(section.items.iter().all(|i| i.section == Section::Colourways) && section.total > 0);
    let publishable =
        must("usable", library.search(&Query { usable_for: Some(Usage::Publish), limit: 100, ..Query::default() }));
    assert!(
        publishable.items.iter().all(|i| i.id.as_str() != "anon/colourways/mint"),
        "non-commercial items are hidden"
    );
    assert!(first.items.iter().any(|i| i.id.as_str() == "anon/colourways/mint"));
}

fn unavailable_with_reasons(library: &dyn Library) {
    let reason = |item: &str| must(item, library.describe(&id(item), None)).info.unavailable;
    assert!(reason("test/stages/lamp").is_some_and(|r| r.contains("scene")), "the scene target is reserved");
    assert!(reason("test/colourways/ghost").is_some_and(|r| r.contains("ghost.missing")), "names the missing op");
    assert!(
        reason("test/outfits/dangling").is_some_and(|r| r.contains("step/0") && r.contains("nonexistent")),
        "a bundle whose item is missing is unavailable, as applying it would fail"
    );
    assert!(
        reason("test/outfits/ghosted").is_some_and(|r| r.contains("ghost.missing")),
        "a bundle is unavailable through its dependency's missing op"
    );
    assert_eq!(reason("test/outfits/casual"), None, "a bundle whose items are all there is available");
    assert_eq!(reason("test/hair/ribbon"), None);
}

fn refusals(library: &dyn Library) {
    assert!(matches!(library.resolve(&id("nobody/hair/none"), &VersionReq::Any), Err(LibraryError::NotFound(_))));
    let like = Query { like: Some(id("test/hair/ribbon")), ..Query::default() };
    assert!(matches!(library.search(&like), Err(LibraryError::Unsupported { .. })));
}
