//! The fixture items. Ids are under `test/` (compiled-in fixtures) or under the two fixture publishers `signer` and
//! `anon`, whose ids can be held only as installed items.
//!
//! [`all`] is the set every [`Library`](omuzaki_library_api::Library) must be loaded with to pass
//! [`conformance::run`](crate::conformance::run); [`MockLibrary::fixtures`](crate::MockLibrary::fixtures) loads it.
//!
//! Every constructor here panics, naming the fixture, if its literal stops parsing: that is a bug in this crate, and
//! its own tests load them all.

use std::fmt::Write as _;

use std::collections::BTreeSet;

use ed25519_dalek::{Signer, SigningKey};
use omuzaki_char_api::CharId;
use omuzaki_library_api::{BodyKind, ItemManifest, ItemScope, Publisher, PublisherKey};

use crate::CharacterFacts;

/// The seed of the key that signs the `signer` fixtures.
pub const SIGNER_SEED: u8 = 1;
/// The seed of the second key, which signs a clashing `shared/hair/bow`.
pub const RIVAL_SEED: u8 = 2;

/// A fixture item: its manifest and the scope that holds it.
pub struct Fixture {
    /// The manifest.
    pub manifest: ItemManifest,
    /// Where it lives.
    pub scope: ItemScope,
}

fn header(id: &str, version: &str, section: &str, kind: &str, rights: &str, extra: &str) -> String {
    format!(
        "schema = 1\nid = \"{id}\"\nversion = \"{version}\"\nsection = \"{section}\"\nkind = \"{kind}\"\n\
         target = \"character\"\ntitle = \"{id}\"\nsummary = \"A fixture for {id}.\"\ntags = [\"fixture\"]\n\
         origin = \"fixture\"\n{extra}\n[rights]\n{rights}\n"
    )
}

const FIRST_PARTY: &str = "author = \"Omuzaki\"\nlicence = \"CC0-1.0\"\ncommercial = \"ok\"\nowned = true";
const NON_COMMERCIAL: &str = "author = \"Anon\"\nlicence = \"CC-BY-NC-4.0\"\ncommercial = \"no\"";
const PERMISSIVE: &str = "author = \"Signer\"\nlicence = \"CC-BY-4.0\"\ncommercial = \"ok\"";

const PALETTE_STEP: &str =
    "[[step]]\nop = \"palette.set\"\n[step.args]\ncharacter = \"$character\"\nslot = \"outfit\"\n";

fn palette(colour: &str) -> String {
    format!("{PALETTE_STEP}colour = \"{colour}\"\n")
}

fn parse(toml: &str) -> ItemManifest {
    // Fixtures are literals in this file, so a failure here is a bug in the fixture: panic, naming it, in any test
    // that loads it (the crate's own tests do).
    ItemManifest::parse(toml).unwrap_or_else(|e| panic!("fixture does not parse: {e}\n{toml}"))
}

fn signed(mut m: ItemManifest, seed: u8, name: &str) -> ItemManifest {
    let key = SigningKey::from_bytes(&[seed; 32]);
    m.publisher = Some(Publisher { name: name.to_owned(), key: PublisherKey(key.verifying_key().to_bytes()).to_hex() });
    m.signature = None;
    m.signature = Some(key.sign(&m.signing_message()).to_bytes().iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    }));
    m
}

/// A part item: adds one `mock-hair` part named by `$parts.ribbon`.
pub fn part() -> ItemManifest {
    let extra = "slot = \"hairstyle\"\ncreates.ribbon = { kind = \"hair\", id = \"ribbon\", layer = 5 }\n";
    let step = "[[step]]\nop = \"part.add\"\n[step.args]\ncharacter = \"$character\"\nkind = \"mock-hair\"\n\
                id = \"$parts.ribbon\"\nlayer = 5\n";
    parse(&(header("test/hair/ribbon", "1.0.0", "hair", "part", FIRST_PARTY, extra) + step))
}

/// A setting item: recolours an existing palette slot.
pub fn setting() -> ItemManifest {
    parse(&(header("test/colourways/dusk", "1.0.0", "colourways", "setting", FIRST_PARTY, "") + &palette("#2a2346")))
}

/// A member item: sets a face field, named by `$name`.
pub fn member() -> ItemManifest {
    let step = "[[step]]\nop = \"face.set_style\"\n[step.args]\ncharacter = \"$character\"\n\
                path = \"face/eyes/iris/width\"\nvalue = 0.6\n";
    parse(&(header("test/expressions/wink", "1.0.0", "expressions", "member", FIRST_PARTY, "name = \"wink\"\n") + step))
}

/// A bundle item: applies the part item and the setting item, then adds a step of its own.
pub fn bundle() -> ItemManifest {
    let steps = "[[step]]\nitem = \"test/hair/ribbon\"\nversion = \"^1\"\n\
                 [[step]]\nitem = \"test/colourways/dusk\"\nversion = \"^1\"\n";
    parse(&(header("test/outfits/casual", "1.0.0", "outfits", "bundle", FIRST_PARTY, "") + steps + &palette("#112233")))
}

/// A scene-target item, which `check` must refuse as reserved (#427).
pub fn scene() -> ItemManifest {
    let toml = header("test/stages/lamp", "1.0.0", "stages", "setting", FIRST_PARTY, "")
        .replace("target = \"character\"", "target = \"scene\"");
    parse(&(toml + &palette("#ffffff")))
}

/// An item whose op is not registered: available only once `ghost.missing` is.
pub fn ghost() -> ItemManifest {
    let step = "[[step]]\nop = \"ghost.missing\"\n[step.args]\ncharacter = \"$character\"\n";
    parse(&(header("test/colourways/ghost", "1.0.0", "colourways", "setting", FIRST_PARTY, "") + step))
}

/// `signer/colourways/ember` at `version`, signed with [`SIGNER_SEED`].
pub fn signed_ember(version: &str) -> ItemManifest {
    let toml = header("signer/colourways/ember", version, "colourways", "setting", PERMISSIVE, "");
    signed(parse(&(toml + &palette("#aa3311"))), SIGNER_SEED, "Signer")
}

/// An unsigned item with a non-commercial licence: an unverified publisher, and not fit to publish.
pub fn unverified() -> ItemManifest {
    parse(&(header("anon/colourways/mint", "1.0.0", "colourways", "setting", NON_COMMERCIAL, "") + &palette("#99ddbb")))
}

/// `shared/colourways/bow`, signed by `seed`; the two seeds give two publishers claiming one id and version.
pub fn shared_bow(seed: u8) -> ItemManifest {
    let colour = if seed == SIGNER_SEED { "#112211" } else { "#221122" };
    let toml = header("shared/colourways/bow", "1.0.0", "colourways", "setting", PERMISSIVE, "") + &palette(colour);
    signed(parse(&toml), seed, if seed == SIGNER_SEED { "Signer" } else { "Rival" })
}

/// A setting item with the given `compat` table body (`kinds = [...]`, `templates = ...`), for the compat fixtures.
fn compat_setting(name: &str, compat: &str) -> ItemManifest {
    let extra = format!("compat = {{ {compat} }}\n");
    parse(
        &(header(&format!("test/colourways/{name}"), "1.0.0", "colourways", "setting", FIRST_PARTY, &extra)
            + &palette("#445566")),
    )
}

/// Fits the fixture character `mira` in every way, including its style.
pub fn fits() -> ItemManifest {
    compat_setting(
        "fits",
        "kinds = [\"humanoid\"], templates = [\"soft-sd\"], styles = [\"soft\"], requires = [\"mock-hair\"]",
    )
}

/// Made for creature bodies: incompatible with `mira`.
pub fn creature_only() -> ItemManifest {
    compat_setting("creature-only", "kinds = [\"creature\"]")
}

/// Made for another template: incompatible with `mira`.
pub fn other_template() -> ItemManifest {
    compat_setting("other-template", "templates = [\"tall\"]")
}

/// Needs a part kind `mira` lacks: incompatible with `mira`.
pub fn needs_wings() -> ItemManifest {
    compat_setting("needs-wings", "requires = [\"mock-wing\"]")
}

/// Designed for another look style: it fits `mira` with a style warning.
pub fn gothic() -> ItemManifest {
    compat_setting("gothic", "styles = [\"gothic\"]")
}

/// A bundle whose item step names an item nobody holds.
pub fn dangling_bundle() -> ItemManifest {
    let steps = "[[step]]\nitem = \"test/hair/nonexistent\"\nversion = \"^1\"\n";
    parse(&(header("test/outfits/dangling", "1.0.0", "outfits", "bundle", FIRST_PARTY, "") + steps))
}

/// A bundle whose dependency has an op that is not registered, so it is unavailable through the dependency.
pub fn ghosted_bundle() -> ItemManifest {
    let steps = "[[step]]\nitem = \"test/colourways/ghost\"\nversion = \"^1\"\n";
    parse(&(header("test/outfits/ghosted", "1.0.0", "outfits", "bundle", FIRST_PARTY, "") + steps))
}

/// `signer/colourways/tide` at `version`, signed with [`SIGNER_SEED`]: held by two teams in [`all`].
pub fn tide(version: &str) -> ItemManifest {
    let toml = header("signer/colourways/tide", version, "colourways", "setting", PERMISSIVE, "");
    signed(parse(&(toml + &palette("#224466"))), SIGNER_SEED, "Signer")
}

/// `shared/colourways/reef`, signed by `seed`: two publishers claiming one id in two different teams.
pub fn reef(seed: u8) -> ItemManifest {
    let colour = if seed == SIGNER_SEED { "#113311" } else { "#331133" };
    let toml = header("shared/colourways/reef", "1.0.0", "colourways", "setting", PERMISSIVE, "") + &palette(colour);
    signed(parse(&toml), seed, if seed == SIGNER_SEED { "Signer" } else { "Rival" })
}

/// The characters the compat fixtures are written against: `mira`, the character of `omuzaki-char-mock`. Hand them
/// to [`MockLibrary::set_characters`](crate::MockLibrary::set_characters) (or to the library under test, however it
/// learns of characters) to run [`conformance::compat`](crate::conformance::compat).
///
/// The style is the look style of `mira`'s face data, which the mock document does not carry.
///
/// # Panics
/// If the fixture character id stops parsing, a bug in this crate.
pub fn characters() -> Vec<(CharId, CharacterFacts)> {
    let part_kinds: BTreeSet<String> =
        ["mock-body", "mock-head", "mock-hair", "mock-garment"].iter().map(|k| (*k).to_owned()).collect();
    let id = CharId::new("mira").unwrap_or_else(|e| panic!("fixture character id: {e}"));
    vec![(
        id,
        CharacterFacts {
            body_kind: BodyKind::Humanoid,
            template: "soft-sd".to_owned(),
            style: Some("soft".to_owned()),
            part_kinds,
        },
    )]
}

/// The whole fixture set, in a fixed order.
pub fn all() -> Vec<Fixture> {
    let held = |manifest, scope| Fixture { manifest, scope };
    vec![
        held(part(), ItemScope::FirstParty),
        held(setting(), ItemScope::FirstParty),
        held(member(), ItemScope::FirstParty),
        held(bundle(), ItemScope::FirstParty),
        held(scene(), ItemScope::FirstParty),
        held(ghost(), ItemScope::FirstParty),
        held(signed_ember("1.0.0"), ItemScope::Mine),
        held(signed_ember("1.1.0"), ItemScope::Mine),
        held(signed_ember("1.2.0"), ItemScope::Team("studio".to_owned())),
        held(fits(), ItemScope::FirstParty),
        held(creature_only(), ItemScope::FirstParty),
        held(other_template(), ItemScope::FirstParty),
        held(needs_wings(), ItemScope::FirstParty),
        held(gothic(), ItemScope::FirstParty),
        held(dangling_bundle(), ItemScope::FirstParty),
        held(ghosted_bundle(), ItemScope::FirstParty),
        held(tide("1.0.0"), ItemScope::Team("atelier".to_owned())),
        held(tide("1.1.0"), ItemScope::Team("studio".to_owned())),
        held(reef(SIGNER_SEED), ItemScope::Team("atelier".to_owned())),
        held(reef(RIVAL_SEED), ItemScope::Team("studio".to_owned())),
        held(unverified(), ItemScope::Mine),
        held(shared_bow(SIGNER_SEED), ItemScope::Mine),
        held(shared_bow(RIVAL_SEED), ItemScope::Mine),
    ]
}
