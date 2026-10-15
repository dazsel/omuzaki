//! The fixture items. Ids are under `test/` (compiled-in fixtures) or under the two fixture publishers `signer` and
//! `anon`, whose ids can be held only as installed items.
//!
//! [`all`] is the set every [`Library`](omuzaki_library_api::Library) must be loaded with to pass
//! [`conformance::run`](crate::conformance::run); [`MockLibrary::fixtures`](crate::MockLibrary::fixtures) loads it.
//!
//! Every constructor here panics, naming the fixture, if its literal stops parsing: that is a bug in this crate, and
//! its own tests load them all.

use std::fmt::Write as _;

use ed25519_dalek::{Signer, SigningKey};
use omuzaki_library_api::{ItemManifest, ItemScope, Publisher, PublisherKey};

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
        held(unverified(), ItemScope::Mine),
        held(shared_bow(SIGNER_SEED), ItemScope::Mine),
        held(shared_bow(RIVAL_SEED), ItemScope::Mine),
    ]
}
