//! The permission summary and the update diff (#1291, design `plugins.md` §3.2, §8, §13).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used)]

use omuzaki_l10n_api::{Catalog, Locale, MessageTable, Resource, extract};
use omuzaki_ops_api::Scope;
use omuzaki_plugin_api::{LicenceClass, Manifest, Requests, Risk, diff, summarise};

/// A manifest asking for `scopes` (and `example.com` when `net` is among them), `licence` and an optional publisher.
fn manifest(scopes: &[Scope], licence: &str, signed: bool) -> Manifest {
    let mut text = format!(
        "id = \"example\"\nversion = \"0.1.0\"\napi = \"^0.1\"\nlicence = \"{licence}\"\nname = \"Example\"\ncomponent = \"p.wasm\"\n"
    );
    if signed {
        text += "publisher = { name = \"Example\", key = \"ed25519:4b1f00aa11bb22cc\" }\n";
    }
    let mut manifest = Manifest::parse(&text).unwrap();
    let net_hosts = if scopes.contains(&Scope::Net) { vec!["example.com".to_owned()] } else { Vec::new() };
    manifest.requests = Requests { scopes: scopes.to_vec(), net_hosts, data: true, ..Requests::default() };
    manifest
}

#[test]
fn lines_come_highest_risk_first_with_the_hosts_in_the_net_line() {
    let summary = summarise(&manifest(&[Scope::Read, Scope::Perform, Scope::Net], "MIT", true));
    let lines: Vec<(Scope, Risk, Option<&str>)> =
        summary.lines.iter().map(|l| (l.scope, l.risk, l.detail.as_deref())).collect();
    assert_eq!(
        lines,
        [
            (Scope::Net, Risk::High, Some("example.com")),
            (Scope::Perform, Risk::Medium, None),
            (Scope::Read, Risk::Low, None)
        ]
    );
    assert_eq!(summary.lines[0].text.id, "plugin-api-perm-net");
    assert_eq!(summary.data.as_ref().map(|d| d.quota_mb), Some(64));
    assert_eq!(summary.needs_person_each_use, []);
    assert_eq!((summary.licence, summary.unsigned), (LicenceClass::Allowed, false));
}

#[test]
fn per_use_scopes_the_licence_and_an_unsigned_plugin_are_reported() {
    let summary = summarise(&manifest(&[Scope::Read, Scope::Publish, Scope::Spend], "CC-BY-NC-4.0", false));
    assert_eq!(summary.needs_person_each_use, [Scope::Publish, Scope::Spend]);
    assert_eq!((summary.licence, summary.unsigned), (LicenceClass::NonCommercial, true));
    let order: Vec<Scope> = summary.lines.iter().map(|l| l.scope).collect();
    assert_eq!(order, [Scope::Publish, Scope::Spend, Scope::Read], "same risk: by scope name");
}

#[test]
fn the_diff_shows_what_an_update_adds_and_drops() {
    let approved = [Scope::Read, Scope::Perform];
    let adds_net = diff(&manifest(&[Scope::Read, Scope::Perform, Scope::Net], "MIT", true), &approved);
    assert_eq!(adds_net.added.iter().map(|l| l.scope).collect::<Vec<_>>(), [Scope::Net]);
    assert_eq!(adds_net.removed, []);
    let drops_perform = diff(&manifest(&[Scope::Read], "MIT", true), &approved);
    assert_eq!(drops_perform.added, []);
    assert_eq!(drops_perform.removed, [Scope::Perform]);
    assert!(diff(&manifest(&approved, "MIT", true), &approved).is_empty());
}

#[test]
fn every_text_is_declared_and_renders_in_english_and_the_pseudo_locale() {
    let table = inventory::iter::<MessageTable>
        .into_iter()
        .find(|t| t.package == "omuzaki-plugin-api")
        .expect("the crate registers its messages");
    let every_scope: Vec<Scope> = Scope::ALL.to_vec();
    let summary = summarise(&manifest(&every_scope, "MIT", true));
    let mut texts: Vec<_> = summary.lines.iter().map(|l| l.text.clone()).collect();
    texts.extend(summary.data.map(|d| d.text));
    assert_eq!(texts.len(), Scope::ALL.len() + 1);
    // The crate ships no Fluent files yet, so the catalog is built from its generated English file, as the extract
    // step writes it (`l10n-ops.md` §3.3).
    let english: &'static str = Box::leak(extract::english("omuzaki-plugin-api").into_boxed_str());
    let resources = [Resource { package: "omuzaki-plugin-api", locale: "en", source: english }];
    for locale in [Locale::EN, Locale::PSEUDO] {
        let catalog = Catalog::from_sources(locale, &[], &resources);
        for text in &texts {
            assert!(table.messages.iter().any(|m| m.id == text.id), "{} is not declared", text.id);
            let rendered = catalog.render(text);
            assert!(!rendered.contains('⟦') && !rendered.is_empty(), "{}: {rendered}", text.id);
        }
        assert_eq!(catalog.missing(), Vec::new(), "{:?}", catalog.locale());
    }
    let net_line = summary.lines.iter().find(|l| l.scope == Scope::Net).unwrap();
    let net = Catalog::from_sources(Locale::EN, &[], &resources).render(&net_line.text);
    assert!(net.contains("example.com"), "{net}");
}
