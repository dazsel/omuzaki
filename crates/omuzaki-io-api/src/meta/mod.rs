//! VRM meta and licence terms, and how they map onto Omuzaki's provenance record (spec §7).
//!
//! [`VrmMeta`] is exactly the shape of `VRMC_vrm.meta` in VRM 1.0 (`VRMC_vrm-1.0/meta.md`): it serialises to and from
//! that JSON object unchanged, so 1.0 importers and exporters are one `serde_json` call, and it is the scene's
//! canonical vocabulary for "what the file says about rights". [`Vrm0Meta`] is the 0.x object (with its spelling
//! mistakes, `…UssageName`) and converts both ways (see [`Vrm0Licence`] for the licence table and
//! [`Vrm0Meta::from_meta`] for the export rules). Keys a typed meta has no field for are kept verbatim in [`VrmMeta::extras`], so a round trip never drops a
//! creator's terms.
//!
//! The file's facts and Omuzaki's record are kept apart on purpose:
//! - **[`VrmMeta`]** says what the *file* grants (who may perform as the avatar, commercial use, redistribution…).
//! - **[`Provenance`]** says what *Omuzaki* knows and what the licence gate (`omuzaki_store_api::licence_gate`) reads:
//!   author, SPDX licence, commercial status, consent, private-test flag.
//!
//! [`VrmMeta::imported_provenance`] and [`Vrm0Meta::imported_provenance`] derive the second from the first, and they
//! are **conservative**: anything the file doesn't settle is [`Commercial::Unknown`], which the gate treats as `No`
//! (§40, "unverified is banned"). A person can then correct the record once they have read the licence.

use omuzaki_store_api::{Commercial, Provenance, Source};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::licence::no_licence_finding;
use crate::{Finding, ImageIx};

mod perform;
mod vrm0;

pub use vrm0::{Vrm0Licence, Vrm0Meta};

/// Which VRM generation a file or exporter speaks.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum VrmVersion {
    /// VRM 0.x: the `VRM` glTF extension, model faces −Z.
    V0,
    /// VRM 1.0: `VRMC_vrm` and friends, model faces +Z.
    V1,
}

/// The VRM Public License 1.0 document URLs accepted by `meta.licenseUrl` (the Japanese and English pages).
pub const VRM_LICENSE_URLS: [&str; 2] = ["https://vrm.dev/licenses/1.0/", "https://vrm.dev/licenses/1.0/en/"];

/// The SPDX-style id Omuzaki records for the VRM Public License 1.0 (it is not an SPDX id, hence `LicenseRef-`).
pub const VRM_LICENSE_REF: &str = "LicenseRef-VRM-Public-License-1.0";

/// Who may perform as the avatar (`meta.avatarPermission`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AvatarPermission {
    /// Only the author.
    OnlyAuthor,
    /// People licensed separately (paid models).
    OnlySeparatelyLicensedPerson,
    /// Anyone.
    Everyone,
}

/// Commercial use (`meta.commercialUsage`). "Corporate organization users cannot use this model unless the property
/// is `corporation`."
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CommercialUsage {
    /// Individuals, without making a profit.
    PersonalNonProfit,
    /// Individuals, profit allowed.
    PersonalProfit,
    /// Anyone, including companies.
    Corporation,
}

/// Whether credit must be shown (`meta.creditNotation`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CreditNotation {
    /// Credit is required.
    Required,
    /// Credit may be omitted.
    Unnecessary,
}

/// What may be done to the model (`meta.modification`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Modification {
    /// No modification.
    Prohibited,
    /// Modification allowed, redistribution of the result not.
    AllowModification,
    /// Modification and redistribution of the result allowed.
    AllowModificationRedistribution,
}

/// The permission flags of `VRMC_vrm.meta`, with the spec's defaults (all the most restrictive value).
///
/// The bools mirror the spec one for one; folding them into a bitset would only hide the file format.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct UseTerms {
    /// Who may be the avatar.
    pub avatar_permission: AvatarPermission,
    /// Excessively violent content.
    pub allow_excessively_violent_usage: bool,
    /// Excessively sexual content.
    pub allow_excessively_sexual_usage: bool,
    /// Commercial use.
    pub commercial_usage: CommercialUsage,
    /// Political or religious content.
    pub allow_political_or_religious_usage: bool,
    /// Anti-social or hateful content.
    pub allow_antisocial_or_hate_usage: bool,
    /// Credit requirement.
    pub credit_notation: CreditNotation,
    /// Redistribution of the model.
    pub allow_redistribution: bool,
    /// Modification of the model.
    pub modification: Modification,
}

impl Default for UseTerms {
    /// The spec defaults: `onlyAuthor`, personal non-profit, credit required, no redistribution, no modification,
    /// every content flag `false`. This is also the most restrictive value, which export uses as a safe cap.
    fn default() -> Self {
        Self {
            avatar_permission: AvatarPermission::OnlyAuthor,
            allow_excessively_violent_usage: false,
            allow_excessively_sexual_usage: false,
            commercial_usage: CommercialUsage::PersonalNonProfit,
            allow_political_or_religious_usage: false,
            allow_antisocial_or_hate_usage: false,
            credit_notation: CreditNotation::Required,
            allow_redistribution: false,
            modification: Modification::Prohibited,
        }
    }
}

impl UseTerms {
    /// The fields of `self` that grant more than the same field of `cap`, spelled as `VRMC_vrm.meta` spells them, in a
    /// fixed order (so reports are deterministic). Ordered fields (who may be the avatar, commercial use, modification)
    /// exceed when their value is higher in the spec's order; the flags exceed when `self` allows what `cap` does not;
    /// `creditNotation` exceeds when `self` drops a credit requirement that `cap` keeps.
    pub fn exceeding(&self, cap: &UseTerms) -> Vec<&'static str> {
        let permission = |t: &UseTerms| match t.avatar_permission {
            AvatarPermission::OnlyAuthor => 0,
            AvatarPermission::OnlySeparatelyLicensedPerson => 1,
            AvatarPermission::Everyone => 2,
        };
        let commercial = |t: &UseTerms| match t.commercial_usage {
            CommercialUsage::PersonalNonProfit => 0,
            CommercialUsage::PersonalProfit => 1,
            CommercialUsage::Corporation => 2,
        };
        let modification = |t: &UseTerms| match t.modification {
            Modification::Prohibited => 0,
            Modification::AllowModification => 1,
            Modification::AllowModificationRedistribution => 2,
        };
        let credit_omittable = |t: &UseTerms| t.credit_notation == CreditNotation::Unnecessary;
        let flag = |own: bool, allowed: bool| own && !allowed;
        [
            ("avatarPermission", permission(self) > permission(cap)),
            (
                "allowExcessivelyViolentUsage",
                flag(self.allow_excessively_violent_usage, cap.allow_excessively_violent_usage),
            ),
            (
                "allowExcessivelySexualUsage",
                flag(self.allow_excessively_sexual_usage, cap.allow_excessively_sexual_usage),
            ),
            ("commercialUsage", commercial(self) > commercial(cap)),
            (
                "allowPoliticalOrReligiousUsage",
                flag(self.allow_political_or_religious_usage, cap.allow_political_or_religious_usage),
            ),
            (
                "allowAntisocialOrHateUsage",
                flag(self.allow_antisocial_or_hate_usage, cap.allow_antisocial_or_hate_usage),
            ),
            ("creditNotation", credit_omittable(self) && !credit_omittable(cap)),
            ("allowRedistribution", flag(self.allow_redistribution, cap.allow_redistribution)),
            ("modification", modification(self) > modification(cap)),
        ]
        .into_iter()
        .filter_map(|(field, exceeds)| exceeds.then_some(field))
        .collect()
    }

    /// True if no field of `self` grants more than the same field of `cap` (see [`UseTerms::exceeding`]). Used to stop
    /// an export from granting more than Omuzaki holds (`rights.terms_exceed_provenance`).
    pub fn is_within(&self, cap: &UseTerms) -> bool {
        self.exceeding(cap).is_empty()
    }
}

/// `VRMC_vrm.meta` (VRM 1.0). Required by the spec: `name`, `authors` (at least one non-empty entry) and
/// `licenseUrl`; a [`Default`] value has none of them, so a fresh scene fails the VRM checks until they are set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
#[derive(Default)]
pub struct VrmMeta {
    /// The model's name.
    pub name: String,
    /// The model's version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Authors, creator first.
    pub authors: Vec<String>,
    /// Who holds the copyright, shown with the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copyright_information: Option<String>,
    /// How to reach the author (never private details, the spec warns).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_information: Option<String>,
    /// Original works this model is based on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    /// Licences of third-party parts, free text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub third_party_licenses: Option<String>,
    /// The thumbnail, an index into the scene's images (square, PNG or JPEG).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_image: Option<ImageIx>,
    /// The licence document the flags refer to; one of [`VRM_LICENSE_URLS`] for VRM 1.0.
    pub license_url: String,
    /// The permission flags (flattened, so the JSON is flat like the spec's).
    #[serde(flatten)]
    pub terms: UseTerms,
    /// Another licence document, when the model has terms beyond the flags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other_license_url: Option<String>,
    /// Keys of a VRM 1.0 `meta` that no field above names, verbatim: the glTF `extras` and `extensions` the 1.0 schema
    /// allows on `meta`, and vendor keys such as `"noAiTraining": true`. A licence term that vanished silently would
    /// be the worst failure an importer could have, so they ride along: a 1.0 exporter writes them back unchanged and
    /// an exporter of the other version lists them in a warning ([`VrmMeta::unwritable_extras`]).
    ///
    /// Flattened, so the JSON stays flat. A key that is one of the fields above is never written from here: the typed
    /// field wins, so a hand-edited scene cannot use this map to override the terms.
    #[serde(flatten, serialize_with = "serialize_extras")]
    pub extras: Map<String, Value>,
    /// The same for a VRM 0.x file: keys of its `meta` that [`Vrm0Meta`] has no field for, in 0.x vocabulary. Not part
    /// of the 1.0 JSON, so never serialised with the meta; a 0.x exporter writes them back.
    #[serde(skip)]
    pub extras_vrm0: Map<String, Value>,
}

/// Every key the typed fields of [`VrmMeta`] read and write, spelled as in `VRMC_vrm.meta` (`VRMC_vrm-1.0/meta.md`). A
/// unit test keeps this equal to the struct's own serialisation.
const VRM1_META_KEYS: [&str; 19] = [
    "name",
    "version",
    "authors",
    "copyrightInformation",
    "contactInformation",
    "references",
    "thirdPartyLicenses",
    "thumbnailImage",
    "licenseUrl",
    "avatarPermission",
    "allowExcessivelyViolentUsage",
    "allowExcessivelySexualUsage",
    "commercialUsage",
    "allowPoliticalOrReligiousUsage",
    "allowAntisocialOrHateUsage",
    "creditNotation",
    "allowRedistribution",
    "modification",
    "otherLicenseUrl",
];

fn serialize_extras<S: Serializer>(extras: &Map<String, Value>, serializer: S) -> Result<S::Ok, S::Error> {
    serialize_unshadowed(extras, &VRM1_META_KEYS, serializer)
}

/// Writes the members of `extras` that are not one of the `typed` keys. Both meta types serialise their extras through
/// this, so unknown data can never shadow (and so never widen) a term the typed fields hold.
fn serialize_unshadowed<S: Serializer>(
    extras: &Map<String, Value>,
    typed: &[&str],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_map(extras.iter().filter(|(key, _)| !typed.contains(&key.as_str())))
}

impl VrmMeta {
    /// A meta with the three fields the spec requires and every other field at its spec default.
    pub fn new(name: impl Into<String>, authors: Vec<String>, license_url: impl Into<String>) -> Self {
        Self { name: name.into(), authors, license_url: license_url.into(), ..Self::default() }
    }
}

fn is_vrm_license_url(url: &str) -> bool {
    VRM_LICENSE_URLS.iter().any(|known| url.trim() == *known)
}

/// The credit line a model asks for: its copyright holder, or else its authors.
fn credit_line(name: &str, copyright: Option<&str>, authors: &[String]) -> String {
    let holder = copyright.filter(|c| !c.trim().is_empty()).map_or_else(|| authors.join(", "), str::to_owned);
    if holder.is_empty() { name.to_owned() } else { format!("{name} - {holder}") }
}

impl VrmMeta {
    /// Derives the §7 provenance record for a freshly imported file. Conservative by design:
    ///
    /// | File says | Record |
    /// |---|---|
    /// | `licenseUrl` is a VRM Public License 1.0 page | licence `LicenseRef-VRM-Public-License-1.0`; commercial `No` for `personalNonProfit`, `Ok` for `personalProfit` and `corporation` |
    /// | any other or empty `licenseUrl` | licence `LicenseRef-VRM-Other` (or `LicenseRef-Unknown` if empty); commercial `Unknown` |
    /// | `creditNotation: required` | `attribution` = name and copyright holder (or authors) |
    /// | `authors` | `author` = authors joined with `, ` |
    ///
    /// `private_test` is never set here: only the user marks private material. `personalProfit` counts as `Ok`
    /// because Omuzaki's users are individual creators first; a team or company should re-check the record
    /// (`corporation` is the only value that explicitly covers them).
    pub fn imported_provenance(&self, file: Option<&str>) -> Provenance {
        let known = is_vrm_license_url(&self.license_url);
        let (licence, commercial) = if known {
            let commercial = match self.terms.commercial_usage {
                CommercialUsage::PersonalNonProfit => Commercial::No,
                CommercialUsage::PersonalProfit | CommercialUsage::Corporation => Commercial::Ok,
            };
            (VRM_LICENSE_REF, commercial)
        } else if self.license_url.trim().is_empty() {
            ("LicenseRef-Unknown", Commercial::Unknown)
        } else {
            ("LicenseRef-VRM-Other", Commercial::Unknown)
        };
        Provenance {
            source: imported(file),
            author: (!self.authors.is_empty()).then(|| self.authors.join(", ")),
            licence: licence.to_owned(),
            attribution: (self.terms.credit_notation == CreditNotation::Required)
                .then(|| credit_line(&self.name, self.copyright_information.as_deref(), &self.authors)),
            commercial,
            consent: None,
            private_test: false,
        }
    }
}

impl VrmMeta {
    /// Notes for the import report about what the file's terms allow. They don't block anything; they tell the user
    /// (and agents) what the model may be used for before it ends up in a project.
    ///
    /// A blank `licenseUrl` is reported as "no licence information" and the terms are not described (there is no
    /// document they refer to). [`run_import`](crate::run_import) calls this for
    /// [`LicenceEvidence::Imported`](crate::LicenceEvidence::Imported) only; the other two kinds of evidence have
    /// their own notes.
    pub fn import_findings(&self) -> Vec<Finding> {
        let mut out = Vec::new();
        if self.license_url.trim().is_empty() {
            out.push(no_licence_finding());
        } else {
            out.extend(self.terms_notes());
        }
        let kept = |keys: &Map<String, Value>| {
            (!keys.is_empty()).then(|| {
                let msg = format!(
                    "the meta has keys this importer does not interpret; they are kept verbatim: {}",
                    describe(keys)
                );
                Finding::info("vrm.meta_unknown_keys", msg).at("meta")
            })
        };
        out.extend(kept(&self.extras));
        out.extend(kept(&self.extras_vrm0));
        out
    }

    fn terms_notes(&self) -> Vec<Finding> {
        let mut out = Vec::new();
        if !is_vrm_license_url(&self.license_url) {
            out.push(
                Finding::warning(
                    "rights.unrecognised_license_url",
                    format!("the licence document {:?} isn't a VRM Public License page", self.license_url),
                )
                .at("meta/licenseUrl")
                .hint("read it, then set the provenance record by hand"),
            );
        }
        if !self.terms.allow_redistribution || self.terms.modification == Modification::Prohibited {
            out.push(
                Finding::info("rights.import_restricted", "the terms forbid redistribution or modification")
                    .at("meta")
                    .hint("use it as is to perform; don't include it in exports or edit it"),
            );
        }
        if self.other_license_url.is_some() {
            out.push(
                Finding::info("rights.other_license", "the model points to further licence terms")
                    .at("meta/otherLicenseUrl"),
            );
        }
        out
    }

    /// The warning an exporter of `target` owes for the extras it cannot write: a VRM 1.0 exporter cannot place the
    /// 0.x keys of [`VrmMeta::extras_vrm0`], and a 0.x exporter cannot place the 1.0 keys of [`VrmMeta::extras`] (the
    /// two versions share no vocabulary for them). `None` when nothing is left out. Same-version extras are written
    /// back verbatim and need no note.
    pub fn unwritable_extras(&self, target: VrmVersion) -> Option<Finding> {
        let (keys, version) = match target {
            VrmVersion::V1 => (&self.extras_vrm0, "VRM 1.0"),
            VrmVersion::V0 => (&self.extras, "VRM 0.x"),
        };
        (!keys.is_empty()).then(|| {
            let msg = format!(
                "{version} has no place for these meta keys of the source file, so they are not written: {}",
                describe(keys)
            );
            Finding::warning("vrm.meta_unknown_keys", msg)
                .at("meta")
                .hint("they may be licence terms: read them in the original file before sharing this one")
        })
    }
}

/// The longest value shown in a finding; the scene keeps the whole value.
const SHOWN: usize = 80;

/// `key = value` for each entry, values cut to a readable length.
fn describe(keys: &Map<String, Value>) -> String {
    keys.iter()
        .map(|(key, value)| {
            let text = value.to_string();
            match text.char_indices().nth(SHOWN) {
                Some((cut, _)) => format!("{key} = {}...", &text[..cut]),
                None => format!("{key} = {text}"),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// A term as the file spells it (`false`, `"prohibited"`), so a message quotes what the creator wrote.
pub(crate) fn spelled<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn imported(file: Option<&str>) -> Source {
    Source::Imported { file: file.unwrap_or("(unnamed)").to_owned(), origin: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_with(commercial: CommercialUsage, url: &str) -> VrmMeta {
        VrmMeta {
            name: "Test".into(),
            authors: vec!["A".into(), "B".into()],
            license_url: url.into(),
            terms: UseTerms { commercial_usage: commercial, ..UseTerms::default() },
            ..VrmMeta::default()
        }
    }

    #[test]
    fn defaults_are_the_spec_defaults_and_serialise_flat() {
        let json = serde_json::to_value(VrmMeta::default()).unwrap();
        assert_eq!(json["avatarPermission"], "onlyAuthor");
        assert_eq!(json["commercialUsage"], "personalNonProfit");
        assert_eq!(json["creditNotation"], "required");
        assert_eq!(json["modification"], "prohibited");
        assert_eq!(json["allowRedistribution"], false);
        assert!(json.get("version").is_none(), "absent optionals are omitted");
        assert!(json.get("terms").is_none(), "the terms are flattened into the meta object");
    }

    #[test]
    fn spec_json_round_trips_and_missing_flags_take_defaults() {
        let src = serde_json::json!({
            "name": "M", "authors": ["x"], "licenseUrl": "https://vrm.dev/licenses/1.0/",
            "commercialUsage": "corporation", "allowRedistribution": true, "thumbnailImage": 3
        });
        let m: VrmMeta = serde_json::from_value(src).unwrap();
        assert_eq!(m.terms.commercial_usage, CommercialUsage::Corporation);
        assert_eq!(m.terms.avatar_permission, AvatarPermission::OnlyAuthor, "absent -> spec default");
        assert_eq!(m.thumbnail_image, Some(ImageIx(3)));
        let back: VrmMeta = serde_json::from_value(serde_json::to_value(&m).unwrap()).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn imported_provenance_is_conservative() {
        let known = "https://vrm.dev/licenses/1.0/";
        let p = meta_with(CommercialUsage::PersonalNonProfit, known).imported_provenance(Some("a.vrm"));
        assert_eq!((p.commercial, p.licence.as_str()), (Commercial::No, VRM_LICENSE_REF));
        assert_eq!(p.author.as_deref(), Some("A, B"));
        assert_eq!(p.attribution.as_deref(), Some("Test - A, B"));
        assert!(!p.private_test && p.consent.is_none());
        assert_eq!(
            meta_with(CommercialUsage::PersonalProfit, known).imported_provenance(None).commercial,
            Commercial::Ok
        );
        assert_eq!(
            meta_with(CommercialUsage::Corporation, "https://vrm.dev/licenses/1.0/en/")
                .imported_provenance(None)
                .commercial,
            Commercial::Ok
        );
        let other = meta_with(CommercialUsage::Corporation, "https://example.com/terms").imported_provenance(None);
        assert_eq!((other.commercial, other.licence.as_str()), (Commercial::Unknown, "LicenseRef-VRM-Other"));
        let none = meta_with(CommercialUsage::Corporation, "").imported_provenance(None);
        assert_eq!((none.commercial, none.licence.as_str()), (Commercial::Unknown, "LicenseRef-Unknown"));
    }

    #[test]
    fn the_typed_keys_are_exactly_what_vrm_meta_writes() {
        let mut m = VrmMeta::new("n", vec!["a".into()], "u");
        m.version = Some("1".into());
        m.copyright_information = Some("c".into());
        m.contact_information = Some("c".into());
        m.references = vec!["r".into()];
        m.third_party_licenses = Some("t".into());
        m.thumbnail_image = Some(ImageIx(0));
        m.other_license_url = Some("o".into());
        let mut written: Vec<String> = serde_json::to_value(&m).unwrap().as_object().unwrap().keys().cloned().collect();
        written.sort();
        let mut listed: Vec<String> = VRM1_META_KEYS.iter().map(|k| (*k).to_owned()).collect();
        listed.sort();
        assert_eq!(written, listed, "a field was added to VrmMeta: list it in VRM1_META_KEYS");
    }

    #[test]
    fn unknown_keys_ride_in_the_extras_and_come_back_verbatim() {
        let src = serde_json::json!({
            "name": "M", "authors": ["x"], "licenseUrl": "https://vrm.dev/licenses/1.0/",
            "extras": {"studio": "ACME", "gap": null}, "extensions": {"EXT_terms": {"noAiTraining": true}},
            "vendorTerms": "no resale"
        });
        let m: VrmMeta = serde_json::from_value(src.clone()).unwrap();
        assert_eq!(m.extras.keys().collect::<Vec<_>>(), ["extensions", "extras", "vendorTerms"]);
        assert!(m.extras_vrm0.is_empty());
        let back = serde_json::to_value(&m).unwrap();
        for key in ["extras", "extensions", "vendorTerms"] {
            assert_eq!(back[key], src[key], "{key}");
        }
        assert_eq!(back["name"], "M", "the typed fields are still typed");
        // They are described on import, so the user sees what the file carried.
        let note = m.import_findings().into_iter().find(|f| f.rule == "vrm.meta_unknown_keys").expect("a note");
        assert!(note.message.contains("vendorTerms") && note.message.contains("extensions"), "{}", note.message);
    }

    #[test]
    fn an_extra_can_never_override_a_typed_term() {
        let mut m = meta_with(CommercialUsage::PersonalNonProfit, VRM_LICENSE_URLS[0]);
        m.extras.insert("allowRedistribution".into(), serde_json::json!(true));
        m.extras.insert("commercialUsage".into(), serde_json::json!("corporation"));
        m.extras.insert("vendorTerms".into(), serde_json::json!(1));
        let written = serde_json::to_value(&m).unwrap();
        assert_eq!(written["allowRedistribution"], false);
        assert_eq!(written["commercialUsage"], "personalNonProfit");
        assert_eq!(written["vendorTerms"], 1);
    }

    #[test]
    fn the_other_versions_extras_are_listed_by_the_exporter_that_cannot_write_them() {
        let mut m = meta_with(CommercialUsage::PersonalNonProfit, VRM_LICENSE_URLS[0]);
        assert!(m.unwritable_extras(VrmVersion::V0).is_none() && m.unwritable_extras(VrmVersion::V1).is_none());
        m.extras.insert("vendorTerms".into(), serde_json::json!("no resale"));
        assert!(m.unwritable_extras(VrmVersion::V1).is_none(), "1.0 keys are written back by a 1.0 exporter");
        let f = m.unwritable_extras(VrmVersion::V0).expect("a 0.x exporter cannot place them");
        assert_eq!((f.rule.as_str(), f.severity), ("vrm.meta_unknown_keys", crate::Severity::Warning));
        assert!(f.message.contains("vendorTerms = \"no resale\""), "{}", f.message);
        let long = "x".repeat(300);
        m.extras.insert("long".into(), serde_json::json!(long));
        let f = m.unwritable_extras(VrmVersion::V0).unwrap();
        assert!(f.message.contains("...") && f.message.len() < 400, "values are cut in findings: {}", f.message);
    }

    #[test]
    fn a_blank_licence_url_is_no_licence_information_and_extras_are_still_noted() {
        let mut m = meta_with(CommercialUsage::PersonalNonProfit, "");
        m.extras.insert("vendorTerms".into(), serde_json::json!(1));
        let rules: Vec<String> = m.import_findings().into_iter().map(|f| f.rule).collect();
        assert_eq!(rules, ["rights.no_license_info", "vrm.meta_unknown_keys"]);
    }

    type Open = fn(&mut UseTerms);

    #[test]
    fn exceeding_names_each_permission_that_grants_more_than_the_cap() {
        let cap = UseTerms::default();
        assert_eq!(cap.exceeding(&cap).len(), 0);
        // One open permission at a time, each named as the spec spells it.
        let open: [(&str, Open); 9] = [
            ("avatarPermission", |t| t.avatar_permission = AvatarPermission::Everyone),
            ("allowExcessivelyViolentUsage", |t| t.allow_excessively_violent_usage = true),
            ("allowExcessivelySexualUsage", |t| t.allow_excessively_sexual_usage = true),
            ("commercialUsage", |t| t.commercial_usage = CommercialUsage::PersonalProfit),
            ("allowPoliticalOrReligiousUsage", |t| t.allow_political_or_religious_usage = true),
            ("allowAntisocialOrHateUsage", |t| t.allow_antisocial_or_hate_usage = true),
            ("creditNotation", |t| t.credit_notation = CreditNotation::Unnecessary),
            ("allowRedistribution", |t| t.allow_redistribution = true),
            ("modification", |t| t.modification = Modification::AllowModification),
        ];
        for (field, set) in open {
            let mut t = UseTerms::default();
            set(&mut t);
            assert_eq!(t.exceeding(&cap), [field]);
            assert!(!t.is_within(&cap) && cap.is_within(&t), "{field}");
        }
        let mut all = UseTerms::default();
        for (_, set) in open {
            set(&mut all);
        }
        assert_eq!(all.exceeding(&cap).len(), 9, "every field is checked");
    }

    #[test]
    fn is_within_compares_every_field() {
        let cap = UseTerms::default();
        assert!(cap.is_within(&cap));
        let open = UseTerms { commercial_usage: CommercialUsage::Corporation, ..UseTerms::default() };
        assert!(cap.is_within(&open) && !open.is_within(&cap));
        let no_credit = UseTerms { credit_notation: CreditNotation::Unnecessary, ..UseTerms::default() };
        assert!(!no_credit.is_within(&cap), "dropping the credit requirement grants more");
        let redistributable = UseTerms { allow_redistribution: true, ..UseTerms::default() };
        assert!(!redistributable.is_within(&cap) && cap.is_within(&redistributable));
    }
}
