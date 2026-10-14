//! VRM 0.x `meta`: reading it into the scene's 1.0 vocabulary, and writing the scene's terms back (spec §7).
//!
//! 0.x names a licence (`licenseName`) where 1.0 has flags, so both directions need a table; [`Vrm0Licence`] is that
//! table, shared, so the two directions cannot disagree about what a name means.
//!
//! **In.** [`Vrm0Meta::to_meta`] maps the fields; [`Vrm0Meta::imported_provenance`] derives the §7 record straight from
//! the 0.x meta, because the 1.0 shape has already lost `licenseName`. Both 0.x URLs (`otherLicenseUrl` and
//! `otherPermissionUrl`) are kept, joined the way `UniVRM`'s migrator joins them when they differ (`'a', 'b'`,
//! vrm-c/UniVRM#1611).
//!
//! **Out.** [`Vrm0Meta::from_meta`] picks the most restrictive 0.x licence that is still faithful (its docs have the
//! rules), warns for every term 0.x ends up saying less about, and never grants more than the scene's terms.

use omuzaki_store_api::{Commercial, Provenance};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};

use super::{
    AvatarPermission, CommercialUsage, CreditNotation, Modification, UseTerms, VRM_LICENSE_URLS, VrmMeta, VrmVersion,
    credit_line, imported, is_vrm_license_url, serialize_unshadowed, spelled,
};
use crate::{Finding, ImageIx};

/// The provenance licence of a model under "some other licence" (a 0.x `Other`, or a 1.0 file with an unrecognised
/// `licenseUrl`).
const OTHER_LICENCE: &str = "LicenseRef-VRM-Other";

/// How `UniVRM` joins two different 0.x URLs into the one 1.0 `otherLicenseUrl` field.
const URL_SEPARATOR: &str = "', '";

/// `VRM.meta` (VRM 0.x), keys spelled as in the 0.x spec including its `Ussage` typos
/// (`VRM-0.0/README.md`, "VRM extension: Model information").
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Vrm0Meta {
    /// Model title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Model version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// A single author string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Contact information.
    #[serde(rename = "contactInformation", skip_serializing_if = "Option::is_none")]
    pub contact_information: Option<String>,
    /// Original works.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// The thumbnail as a *texture* index (1.0 uses an image index; the importer resolves it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texture: Option<u32>,
    /// `OnlyAuthor`, `ExplicitlyLicensedPerson` or `Everyone`.
    #[serde(rename = "allowedUserName", skip_serializing_if = "Option::is_none")]
    pub allowed_user_name: Option<String>,
    /// `Allow` or `Disallow`.
    #[serde(rename = "violentUssageName", skip_serializing_if = "Option::is_none")]
    pub violent_ussage_name: Option<String>,
    /// `Allow` or `Disallow`.
    #[serde(rename = "sexualUssageName", skip_serializing_if = "Option::is_none")]
    pub sexual_ussage_name: Option<String>,
    /// `Allow` or `Disallow`.
    #[serde(rename = "commercialUssageName", skip_serializing_if = "Option::is_none")]
    pub commercial_ussage_name: Option<String>,
    /// URL for conditions not covered above.
    #[serde(rename = "otherPermissionUrl", skip_serializing_if = "Option::is_none")]
    pub other_permission_url: Option<String>,
    /// `Redistribution_Prohibited`, `CC0`, `CC_BY`, `CC_BY_NC`, `CC_BY_SA`, `CC_BY_NC_SA`, `CC_BY_ND`, `CC_BY_NC_ND`
    /// or `Other`.
    #[serde(rename = "licenseName", skip_serializing_if = "Option::is_none")]
    pub license_name: Option<String>,
    /// URL of the licence when `licenseName` is `Other`.
    #[serde(rename = "otherLicenseUrl", skip_serializing_if = "Option::is_none")]
    pub other_license_url: Option<String>,
    /// Keys of the file's `meta` that no field above names, verbatim (a vendor's `"noAiTraining": true`). Flattened,
    /// and never written over a field above, like [`VrmMeta::extras`]. [`Vrm0Meta::to_meta`] moves them to
    /// [`VrmMeta::extras_vrm0`] and [`Vrm0Meta::from_meta`] back.
    #[serde(flatten, serialize_with = "serialize_extras")]
    pub extras: Map<String, Value>,
}

/// Every key the typed fields of [`Vrm0Meta`] read and write, spelled as in the 0.x spec, typos included. A unit test
/// keeps this equal to the struct's own serialisation.
const VRM0_META_KEYS: [&str; 13] = [
    "title",
    "version",
    "author",
    "contactInformation",
    "reference",
    "texture",
    "allowedUserName",
    "violentUssageName",
    "sexualUssageName",
    "commercialUssageName",
    "otherPermissionUrl",
    "licenseName",
    "otherLicenseUrl",
];

fn serialize_extras<S: Serializer>(extras: &Map<String, Value>, serializer: S) -> Result<S::Ok, S::Error> {
    serialize_unshadowed(extras, &VRM0_META_KEYS, serializer)
}

/// What a 0.x `licenseName` means, in 1.0 vocabulary plus the id Omuzaki records. 0.x says nothing beyond the name, so the
/// flags are our reading of the licence text each name links to in the 0.x spec (the table in
/// `docs/design/io-api.md`, D5).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Vrm0Licence {
    /// The id recorded in `Provenance::licence`: an SPDX id, or a `LicenseRef-` for the names that have none.
    pub spdx: &'static str,
    /// Whether the licence itself allows commercial use. `Some(false)` for the `CC_BY_NC*` names (policy: nothing
    /// non-commercial), `None` when the name settles nothing (`Other` points at a document we can't evaluate, and a
    /// missing or unknown name is the same).
    pub commercial_ok: Option<bool>,
    /// Whether the model may be redistributed.
    pub redistribution: bool,
    /// What may be done to the model.
    pub modification: Modification,
    /// Whether credit must be shown.
    pub credit: CreditNotation,
}

impl Vrm0Licence {
    /// The names that stand for one fixed licence: every 0.x `licenseName` except `Other`.
    pub const NAMES: [&'static str; 8] =
        ["CC0", "CC_BY", "CC_BY_SA", "CC_BY_ND", "CC_BY_NC", "CC_BY_NC_SA", "CC_BY_NC_ND", "Redistribution_Prohibited"];

    /// The strictest 0.x licence: nothing is granted beyond using the model.
    const STRICTEST: &'static str = "Redistribution_Prohibited";

    /// What `name` means. An unknown or missing name is [`Vrm0Licence::spdx`] `LicenseRef-Unknown`, which grants
    /// nothing.
    pub fn named(name: Option<&str>) -> Self {
        use CreditNotation::{Required, Unnecessary};
        use Modification::{AllowModificationRedistribution as Free, Prohibited};
        let l = |spdx, commercial_ok, redistribution, modification, credit| Self {
            spdx,
            commercial_ok,
            redistribution,
            modification,
            credit,
        };
        match name {
            Some("Redistribution_Prohibited") => {
                l("LicenseRef-VRM-Redistribution-Prohibited", Some(true), false, Prohibited, Required)
            }
            Some("CC0") => l("CC0-1.0", Some(true), true, Free, Unnecessary),
            Some("CC_BY") => l("CC-BY-4.0", Some(true), true, Free, Required),
            Some("CC_BY_SA") => l("CC-BY-SA-4.0", Some(true), true, Free, Required),
            Some("CC_BY_ND") => l("CC-BY-ND-4.0", Some(true), true, Prohibited, Required),
            // Non-commercial licences are `No` whatever the avatar-use flag says (policy: nothing non-commercial).
            Some("CC_BY_NC") => l("CC-BY-NC-4.0", Some(false), true, Free, Required),
            Some("CC_BY_NC_SA") => l("CC-BY-NC-SA-4.0", Some(false), true, Free, Required),
            Some("CC_BY_NC_ND") => l("CC-BY-NC-ND-4.0", Some(false), true, Prohibited, Required),
            // `Other` points at a document we can't evaluate, so it settles nothing; missing is the same.
            Some("Other") => l(OTHER_LICENCE, None, false, Prohibited, Required),
            _ => l("LicenseRef-Unknown", None, false, Prohibited, Required),
        }
    }

    /// The licence-level terms of this name in 1.0 vocabulary (redistribution, modification, credit); every other
    /// field is at its most restrictive default, because a name says nothing about them.
    pub fn terms(&self) -> UseTerms {
        UseTerms {
            allow_redistribution: self.redistribution,
            modification: self.modification,
            credit_notation: self.credit,
            ..UseTerms::default()
        }
    }

    /// The fixed name whose Omuzaki licence id is `id`, with its table row (how a provenance record points back at the
    /// 0.x name it came from).
    fn by_id(id: &str) -> Option<(&'static str, Self)> {
        Self::NAMES.iter().map(|&name| (name, Self::named(Some(name)))).find(|(_, licence)| licence.spdx == id)
    }
}

fn allow(flag: Option<&str>) -> bool {
    flag == Some("Allow")
}

/// The 1.0 `otherLicenseUrl` for a 0.x meta. 0.x has two URL fields, the licence document (`otherLicenseUrl`) and the
/// conditions the flags do not cover (`otherPermissionUrl`); 1.0 has one. When both are given and differ, both are
/// kept in the one field, as `UniVRM` does.
fn join_urls(licence: Option<&str>, permission: Option<&str>) -> Option<String> {
    let given = |u: Option<&str>| u.filter(|u| !u.trim().is_empty()).map(str::to_owned);
    match (given(licence), given(permission)) {
        (Some(a), Some(b)) if a != b => Some(format!("'{a}{URL_SEPARATOR}{b}'")),
        (a, b) => a.or(b),
    }
}

impl Vrm0Meta {
    /// Converts to the 1.0 shape. Mapping (`UniVRM`'s migrator makes the same choices for the avatar flags):
    /// `allowedUserName` `OnlyAuthor`/`ExplicitlyLicensedPerson`/`Everyone` become `onlyAuthor`/
    /// `onlySeparatelyLicensedPerson`/`everyone`; `Allow` on violent or sexual use becomes `true`; `commercial Allow`
    /// becomes `personalProfit` (0.x cannot say `corporation`) and `Disallow` `personalNonProfit`;
    /// the political and anti-social flags don't exist in 0.x and stay `false`. `licenseName` sets redistribution,
    /// modification and credit as in the table in `docs/design/io-api.md`. Both URLs are kept in `otherLicenseUrl`
    /// (joined `'a', 'b'` when they differ), and unknown keys in [`VrmMeta::extras_vrm0`]. `thumbnail` is the image the
    /// importer resolved from [`Vrm0Meta::texture`].
    pub fn to_meta(&self, thumbnail: Option<ImageIx>) -> VrmMeta {
        let lic = Vrm0Licence::named(self.license_name.as_deref());
        let authors: Vec<String> = self.author.iter().filter(|a| !a.trim().is_empty()).cloned().collect();
        let terms = UseTerms {
            avatar_permission: match self.allowed_user_name.as_deref() {
                Some("Everyone") => AvatarPermission::Everyone,
                Some("ExplicitlyLicensedPerson") => AvatarPermission::OnlySeparatelyLicensedPerson,
                _ => AvatarPermission::OnlyAuthor,
            },
            allow_excessively_violent_usage: allow(self.violent_ussage_name.as_deref()),
            allow_excessively_sexual_usage: allow(self.sexual_ussage_name.as_deref()),
            commercial_usage: if allow(self.commercial_ussage_name.as_deref()) {
                CommercialUsage::PersonalProfit
            } else {
                CommercialUsage::PersonalNonProfit
            },
            allow_political_or_religious_usage: false,
            allow_antisocial_or_hate_usage: false,
            credit_notation: lic.credit,
            allow_redistribution: lic.redistribution,
            modification: lic.modification,
        };
        VrmMeta {
            name: self.title.clone().unwrap_or_default(),
            version: self.version.clone(),
            authors,
            contact_information: self.contact_information.clone(),
            references: self.reference.iter().filter(|r| !r.trim().is_empty()).cloned().collect(),
            thumbnail_image: thumbnail,
            // 0.x has no licence document of its own; the flags above are what the file grants, and they are
            // expressed in the VRM 1.0 vocabulary, so the 1.0 Public License page is the document they refer to.
            license_url: VRM_LICENSE_URLS[0].to_owned(),
            other_license_url: join_urls(self.other_license_url.as_deref(), self.other_permission_url.as_deref()),
            extras_vrm0: self.extras.clone(),
            terms,
            ..VrmMeta::default()
        }
    }

    /// Derives the §7 record straight from the 0.x meta, because the 1.0 shape has already lost `licenseName`
    /// (a `CC_BY_NC` model must record `CC-BY-NC-4.0` and `No`). Commercial status:
    /// `No` if the avatar flag is `Disallow` or the licence is a `CC_BY_NC*`; `Unknown` if `licenseName` is `Other`
    /// or missing; otherwise `Ok`.
    pub fn imported_provenance(&self, file: Option<&str>) -> Provenance {
        let lic = Vrm0Licence::named(self.license_name.as_deref());
        let commercial = match (lic.commercial_ok, allow(self.commercial_ussage_name.as_deref())) {
            (Some(false), _) | (_, false) => Commercial::No,
            (None, true) => Commercial::Unknown,
            (Some(true), true) => Commercial::Ok,
        };
        let meta = self.to_meta(None);
        Provenance {
            source: imported(file),
            author: (!meta.authors.is_empty()).then(|| meta.authors.join(", ")),
            licence: lic.spdx.to_owned(),
            attribution: (lic.credit == CreditNotation::Required).then(|| credit_line(&meta.name, None, &meta.authors)),
            commercial,
            consent: None,
            private_test: false,
        }
    }

    /// Converts a scene's 1.0 meta to 0.x for export, with a finding for everything 0.x cannot say exactly. `provenance`
    /// is the scene's record. `texture` is left for the caller, which knows the document's textures.
    ///
    /// 1.0 says more than 0.x can, so the scene is mapped to the *most restrictive 0.x licence that is still faithful*,
    /// and every term the file ends up saying less about is a `vrm0.meta_licence` warning that names the field and its
    /// value. The rules, in order:
    /// 1. The provenance says the model came from a known 0.x licence: keep that `licenseName` (`CC0`, `CC_BY`, ...
    ///    `Redistribution_Prohibited`) as long as it does not grant more than the 1.0 terms now say. If the terms were
    ///    narrowed below it, write `Redistribution_Prohibited`, the strictest 0.x name.
    /// 2. The licence is `Other` (a 0.x `Other`, or a 1.0 file pointing at a licence document that is not the VRM
    ///    Public License): write `Other` with that document.
    /// 3. Otherwise (the VRM Public License 1.0, or a model made in Omuzaki): redistribution off is
    ///    `Redistribution_Prohibited`; redistribution on has no 0.x name, so it is `Other` pointing at the licence
    ///    document, and the settings 0.x cannot hold are listed.
    ///
    /// A creator's licence is never widened: no rule picks a name that grants something the terms do not, and a model
    /// whose terms are `personalNonProfit` never becomes a name that is not already its own (never `CC0` or `CC_BY` out
    /// of a 1.0 file).
    ///
    /// Findings, each naming the field and its value in its location and message: `vrm0.meta_licence` (a term the
    /// chosen licence name cannot state), `vrm0.meta_commercial` (`corporation`), `vrm0.meta_content_flags` (the
    /// political and anti-social flags), `vrm0.meta_third_party_licenses` (appended to `reference`, which has no such
    /// field) and `vrm.meta_unknown_keys` (extras of the other version).
    pub fn from_meta(meta: &VrmMeta, provenance: Option<&Provenance>) -> (Self, Vec<Finding>) {
        let t = &meta.terms;
        let mut findings = Vec::new();
        flag_findings(t, &mut findings);
        let raw_urls = meta.other_license_url.as_deref();
        let urls = Urls::parse(raw_urls);
        let licence = provenance.map_or("", |p| p.licence.as_str());
        let chosen = choose(meta, licence, &urls, &mut findings);
        let (other_license_url, other_permission_url) = url_fields(&chosen, &urls, raw_urls);
        let reference = reference(meta, &mut findings);
        findings.extend(meta.unwritable_extras(VrmVersion::V0));
        let yes_no = |b: bool| Some(if b { "Allow" } else { "Disallow" }.to_owned());
        let v0 = Self {
            title: Some(meta.name.clone()),
            version: meta.version.clone(),
            author: Some(meta.authors.join(", ")),
            contact_information: meta.contact_information.clone(),
            reference,
            texture: None,
            allowed_user_name: Some(
                match t.avatar_permission {
                    AvatarPermission::OnlyAuthor => "OnlyAuthor",
                    AvatarPermission::OnlySeparatelyLicensedPerson => "ExplicitlyLicensedPerson",
                    AvatarPermission::Everyone => "Everyone",
                }
                .to_owned(),
            ),
            violent_ussage_name: yes_no(t.allow_excessively_violent_usage),
            sexual_ussage_name: yes_no(t.allow_excessively_sexual_usage),
            commercial_ussage_name: yes_no(t.commercial_usage != CommercialUsage::PersonalNonProfit),
            other_permission_url,
            license_name: Some(chosen.name.to_owned()),
            other_license_url,
            extras: meta.extras_vrm0.clone(),
        };
        (v0, findings)
    }
}

/// The URLs held by the scene's `otherLicenseUrl`: none, one, or the pair [`join_urls`] made.
enum Urls<'a> {
    None,
    One(&'a str),
    Two(&'a str, &'a str),
}

impl<'a> Urls<'a> {
    fn parse(url: Option<&'a str>) -> Self {
        let Some(url) = url.map(str::trim).filter(|u| !u.is_empty()) else { return Self::None };
        let pair = url
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .filter(|s| s.matches(URL_SEPARATOR).count() == 1)
            .and_then(|s| s.split_once(URL_SEPARATOR));
        pair.map_or(Self::One(url), |(a, b)| Self::Two(a, b))
    }

    /// The first URL, which is the licence document when the file has several.
    fn first(&self) -> Option<&'a str> {
        match *self {
            Self::None => None,
            Self::One(a) | Self::Two(a, _) => Some(a),
        }
    }
}

/// A fixed 0.x licence name with its table row, as the export rules compare them.
struct Named {
    name: &'static str,
    terms: UseTerms,
}

impl Named {
    fn new(name: &'static str) -> Self {
        Self { name, terms: Vrm0Licence::named(Some(name)).terms() }
    }
}

/// The licence the 0.x file will name: `Other` carries `document` (a licence URL taken from `licenseUrl`); an `Other`
/// without one takes its URLs from `otherLicenseUrl`.
struct Chosen {
    name: &'static str,
    document: Option<String>,
}

impl Chosen {
    fn name(name: &'static str) -> Self {
        Self { name, document: None }
    }
}

/// A term that one side grants and the other does not.
struct Excess {
    field: &'static str,
    /// The value on the granting side, spelled as the file spells it.
    value: String,
}

fn modification_rank(m: Modification) -> u8 {
    match m {
        Modification::Prohibited => 0,
        Modification::AllowModification => 1,
        Modification::AllowModificationRedistribution => 2,
    }
}

/// The licence-level terms (the ones `licenseName` stands for) in which `a` grants more than `b`.
fn excess(a: &UseTerms, b: &UseTerms) -> Vec<Excess> {
    let mut out = Vec::new();
    if a.allow_redistribution && !b.allow_redistribution {
        out.push(Excess { field: "allowRedistribution", value: spelled(&a.allow_redistribution) });
    }
    if modification_rank(a.modification) > modification_rank(b.modification) {
        out.push(Excess { field: "modification", value: spelled(&a.modification) });
    }
    if a.credit_notation == CreditNotation::Unnecessary && b.credit_notation == CreditNotation::Required {
        out.push(Excess { field: "creditNotation", value: spelled(&a.credit_notation) });
    }
    out
}

/// Warns, per field, that 0.x ends up saying less than the scene's terms.
fn warn(findings: &mut Vec<Finding>, fields: Vec<Excess>, because: &str) {
    for Excess { field, value } in fields {
        findings.push(
            Finding::warning(
                "vrm0.meta_licence",
                format!("{field} = {value} cannot be written exactly in 0.x: {because}"),
            )
            .at(format!("meta/{field}")),
        );
    }
}

/// Picks the 0.x licence for the scene's terms and the licence id of its provenance (the rules are in the module
/// docs).
fn choose(meta: &VrmMeta, licence: &str, urls: &Urls<'_>, findings: &mut Vec<Finding>) -> Chosen {
    let terms = &meta.terms;
    let document = Some(meta.license_url.trim()).filter(|u| !u.is_empty());
    let own_document = document.filter(|u| !is_vrm_license_url(u));

    if licence == OTHER_LICENCE {
        if let Some(doc) = own_document {
            return other(doc, terms, findings);
        }
        if urls.first().is_some() {
            return other_by_urls(terms, findings);
        }
        findings.push(
            Finding::warning(
                "vrm0.meta_licence",
                "licenseName Other has no licence document to point at, so the strictest 0.x licence is written instead",
            )
            .at("meta/licenseUrl"),
        );
        return strictest(terms, findings, "the strictest 0.x licence is written instead of Other");
    }
    if let Some(known) = Vrm0Licence::by_id(licence).map(|(name, _)| Named::new(name)) {
        let strictest_terms = Named::new(Vrm0Licence::STRICTEST).terms;
        if excess(&known.terms, terms).is_empty() {
            let because = format!("licenseName {} grants less than this", known.name);
            warn(findings, excess(terms, &known.terms), &because);
            return Chosen::name(known.name);
        }
        let because = format!("the terms were narrowed below {}, so the strictest 0.x licence is used", known.name);
        warn(findings, excess(terms, &strictest_terms), &because);
        return Chosen::name(Vrm0Licence::STRICTEST);
    }
    // The VRM Public License 1.0, or a model made in Omuzaki: the scene's own flags are the licence.
    if let Some(doc) = own_document {
        return other(doc, terms, findings);
    }
    match document {
        Some(doc) if terms.allow_redistribution => other(doc, terms, findings),
        _ => {
            strictest(terms, findings, "0.x has no licence name for these terms, so the strictest 0.x licence is used")
        }
    }
}

/// `Other`, pointing at `document`; the settings 0.x cannot hold are the ones that grant anything beyond the defaults.
fn other(document: &str, terms: &UseTerms, findings: &mut Vec<Finding>) -> Chosen {
    let because =
        format!("licenseName Other only points at {document}; 0.x readers get the document, not this setting");
    warn(findings, excess(terms, &UseTerms::default()), &because);
    Chosen { name: "Other", document: Some(document.to_owned()) }
}

/// `Other` whose document is in `otherLicenseUrl`.
fn other_by_urls(terms: &UseTerms, findings: &mut Vec<Finding>) -> Chosen {
    let because =
        "licenseName Other only points at the licence document; 0.x readers get the document, not this setting";
    warn(findings, excess(terms, &UseTerms::default()), because);
    Chosen::name("Other")
}

fn strictest(terms: &UseTerms, findings: &mut Vec<Finding>, because: &str) -> Chosen {
    warn(findings, excess(terms, &Named::new(Vrm0Licence::STRICTEST).terms), because);
    Chosen::name(Vrm0Licence::STRICTEST)
}

/// `(otherLicenseUrl, otherPermissionUrl)` of the 0.x meta. Every URL of the scene lands in one of them: the licence
/// document (when the scene has one of its own) goes to the first and all of `otherLicenseUrl` to the second; a pair
/// goes to one field each; a single URL goes to both, which is also what the 0.x spec's wording allows and what a
/// round trip needs, because 1.0 cannot say which field it came from.
fn url_fields(chosen: &Chosen, urls: &Urls<'_>, raw: Option<&str>) -> (Option<String>, Option<String>) {
    let own = |s: &str| Some(s.to_owned());
    match (&chosen.document, urls) {
        (Some(doc), _) => (Some(doc.clone()), raw.map(|r| r.trim().to_owned()).filter(|r| !r.is_empty())),
        (None, Urls::Two(a, b)) => (own(a), own(b)),
        (None, Urls::One(a)) => (own(a), own(a)),
        (None, Urls::None) => (None, None),
    }
}

/// What the 0.x flags cannot say about commercial use and content.
fn flag_findings(terms: &UseTerms, findings: &mut Vec<Finding>) {
    if terms.commercial_usage == CommercialUsage::Corporation {
        findings.push(
            Finding::warning(
                "vrm0.meta_commercial",
                "commercialUsage = \"corporation\" is written as commercialUssageName Allow: 0.x only says whether \
                 commercial use is allowed, not whether companies are covered",
            )
            .at("meta/commercialUsage"),
        );
    }
    for (field, granted) in [
        ("allowPoliticalOrReligiousUsage", terms.allow_political_or_religious_usage),
        ("allowAntisocialOrHateUsage", terms.allow_antisocial_or_hate_usage),
    ] {
        if granted {
            let msg = format!(
                "{field} = true cannot be written in 0.x, which has no such flag; readers will not see the permission"
            );
            findings.push(Finding::warning("vrm0.meta_content_flags", msg).at(format!("meta/{field}")));
        }
    }
}

/// 0.x `reference`, which also takes `thirdPartyLicenses` because 0.x has no field for it.
fn reference(meta: &VrmMeta, findings: &mut Vec<Finding>) -> Option<String> {
    let mut parts = Vec::new();
    if !meta.references.is_empty() {
        parts.push(meta.references.join(", "));
    }
    if let Some(text) = meta.third_party_licenses.as_deref().filter(|t| !t.trim().is_empty()) {
        parts.push(format!("Third-party licenses: {text}"));
        findings.push(
            Finding::warning(
                "vrm0.meta_third_party_licenses",
                "0.x has no thirdPartyLicenses field; its text was appended to `reference`",
            )
            .at("meta/thirdPartyLicenses"),
        );
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn v0(license: &str, commercial: &str) -> Vrm0Meta {
        Vrm0Meta {
            title: Some("T".into()),
            author: Some("Au".into()),
            license_name: Some(license.into()),
            commercial_ussage_name: Some(commercial.into()),
            allowed_user_name: Some("Everyone".into()),
            ..Vrm0Meta::default()
        }
    }

    fn keys_of<T: Serialize>(v: &T) -> Vec<String> {
        let mut keys: Vec<String> = serde_json::to_value(v).unwrap().as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }

    fn sorted(list: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = list.iter().map(|s| (*s).to_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn the_typed_keys_are_exactly_what_vrm0_meta_writes() {
        let s = || Some("x".to_owned());
        let full = Vrm0Meta {
            title: s(),
            version: s(),
            author: s(),
            contact_information: s(),
            reference: s(),
            texture: Some(0),
            allowed_user_name: s(),
            violent_ussage_name: s(),
            sexual_ussage_name: s(),
            commercial_ussage_name: s(),
            other_permission_url: s(),
            license_name: s(),
            other_license_url: s(),
            extras: Map::new(),
        };
        assert_eq!(keys_of(&full), sorted(&VRM0_META_KEYS), "a field was added to Vrm0Meta: list it in VRM0_META_KEYS");
    }

    #[test]
    fn unknown_keys_are_collected_and_written_back_but_never_over_a_typed_key() {
        let meta: Vrm0Meta =
            serde_json::from_value(json!({"title": "T", "vendorTerms": {"nested": [1, null]}, "noAi": true})).unwrap();
        assert_eq!(meta.title.as_deref(), Some("T"));
        assert_eq!(meta.extras.keys().collect::<Vec<_>>(), ["noAi", "vendorTerms"]);
        let back = serde_json::to_value(&meta).unwrap();
        assert_eq!(back["vendorTerms"], json!({"nested": [1, null]}), "values are verbatim, null included");
        // A hand-edited scene must not be able to override a term through the extras.
        let mut forged = meta;
        forged.extras.insert("licenseName".into(), json!("CC0"));
        forged.license_name = Some("Redistribution_Prohibited".into());
        assert_eq!(serde_json::to_value(&forged).unwrap()["licenseName"], "Redistribution_Prohibited");
    }

    #[test]
    fn vrm0_licences_map_to_spdx_and_commercial_status() {
        let status = |l, c| v0(l, c).imported_provenance(None);
        assert_eq!(
            (status("CC0", "Allow").commercial, status("CC0", "Allow").licence),
            (Commercial::Ok, "CC0-1.0".into())
        );
        assert_eq!(status("CC_BY", "Allow").licence, "CC-BY-4.0");
        assert_eq!(status("CC_BY_NC", "Allow").commercial, Commercial::No, "non-commercial licence wins over the flag");
        assert_eq!(status("CC_BY_NC_ND", "Allow").licence, "CC-BY-NC-ND-4.0");
        assert_eq!(status("CC_BY", "Disallow").commercial, Commercial::No, "the avatar-use flag can still forbid");
        assert_eq!(status("Other", "Allow").commercial, Commercial::Unknown);
        assert_eq!(status("Redistribution_Prohibited", "Allow").commercial, Commercial::Ok);
        assert_eq!(
            Vrm0Meta::default().imported_provenance(None).commercial,
            Commercial::No,
            "missing flag is not permission"
        );
    }

    #[test]
    fn the_licence_table_is_public_and_every_name_has_its_own_id() {
        let ids: Vec<&str> = Vrm0Licence::NAMES.iter().map(|n| Vrm0Licence::named(Some(n)).spdx).collect();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "{ids:?}");
        assert_eq!(Vrm0Licence::named(Some("CC_BY_NC")).commercial_ok, Some(false));
        assert_eq!(Vrm0Licence::named(Some("Other")).commercial_ok, None);
        assert_eq!(Vrm0Licence::named(None).spdx, "LicenseRef-Unknown");
        let nd = Vrm0Licence::named(Some("CC_BY_ND")).terms();
        assert!(nd.allow_redistribution && nd.modification == Modification::Prohibited);
    }

    #[test]
    fn vrm0_meta_converts_to_1_0_vocabulary() {
        let m = Vrm0Meta {
            violent_ussage_name: Some("Allow".into()),
            allowed_user_name: Some("ExplicitlyLicensedPerson".into()),
            reference: Some("orig".into()),
            other_permission_url: Some("https://p".into()),
            extras: serde_json::from_value(json!({"vendorTerms": 1})).unwrap(),
            ..v0("CC_BY_ND", "Allow")
        }
        .to_meta(Some(ImageIx(1)));
        assert_eq!(m.name, "T");
        assert_eq!(m.terms.avatar_permission, AvatarPermission::OnlySeparatelyLicensedPerson);
        assert!(m.terms.allow_excessively_violent_usage && !m.terms.allow_excessively_sexual_usage);
        assert_eq!(m.terms.commercial_usage, CommercialUsage::PersonalProfit);
        assert_eq!((m.terms.allow_redistribution, m.terms.modification), (true, Modification::Prohibited));
        assert_eq!((m.references.as_slice(), m.thumbnail_image), (&["orig".to_owned()][..], Some(ImageIx(1))));
        assert_eq!(m.other_license_url.as_deref(), Some("https://p"));
        assert_eq!(m.extras_vrm0["vendorTerms"], 1, "unknown 0.x keys are kept, in the 0.x slot");
        assert!(m.extras.is_empty());
    }

    #[test]
    fn both_urls_are_joined_when_they_differ_and_split_again_on_export() {
        let with = |license: Option<&str>, permission: Option<&str>| {
            Vrm0Meta {
                other_license_url: license.map(str::to_owned),
                other_permission_url: permission.map(str::to_owned),
                ..v0("Other", "Disallow")
            }
            .to_meta(None)
        };
        let both = with(Some("https://a/licence"), Some("https://b/permission"));
        assert_eq!(both.other_license_url.as_deref(), Some("'https://a/licence', 'https://b/permission'"));
        let (back, _) = Vrm0Meta::from_meta(&both, Some(&v0("Other", "Disallow").imported_provenance(None)));
        assert_eq!(back.license_name.as_deref(), Some("Other"));
        assert_eq!(
            (back.other_license_url.as_deref(), back.other_permission_url.as_deref()),
            (Some("https://a/licence"), Some("https://b/permission"))
        );
        // The same URL twice, one of them blank, or only one: just that URL.
        assert_eq!(with(Some("https://a/x"), Some("https://a/x")).other_license_url.as_deref(), Some("https://a/x"));
        assert_eq!(with(Some(" "), Some("https://p")).other_license_url.as_deref(), Some("https://p"));
        assert_eq!(with(None, Some("https://p")).other_license_url.as_deref(), Some("https://p"));
        assert_eq!(with(None, None).other_license_url, None);
    }

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
    fn exporting_to_vrm0_reports_what_it_loses() {
        let mut m = meta_with(CommercialUsage::Corporation, VRM_LICENSE_URLS[0]);
        m.terms.allow_political_or_religious_usage = true;
        let (v0, lost) = Vrm0Meta::from_meta(&m, None);
        assert_eq!(v0.commercial_ussage_name.as_deref(), Some("Allow"));
        assert_eq!(v0.license_name.as_deref(), Some("Redistribution_Prohibited"));
        let at = |rule: &str| lost.iter().find(|f| f.rule == rule).and_then(|f| f.location.clone());
        assert_eq!(at("vrm0.meta_commercial").as_deref(), Some("meta/commercialUsage"));
        assert_eq!(at("vrm0.meta_content_flags").as_deref(), Some("meta/allowPoliticalOrReligiousUsage"));
        let (clean, none_lost) =
            Vrm0Meta::from_meta(&meta_with(CommercialUsage::PersonalNonProfit, VRM_LICENSE_URLS[0]), None);
        assert_eq!(clean.commercial_ussage_name.as_deref(), Some("Disallow"));
        assert_eq!(none_lost, vec![]);
    }

    #[test]
    fn a_known_licence_name_survives_the_trip_and_is_never_widened() {
        for name in Vrm0Licence::NAMES {
            let src = v0(name, "Disallow");
            let provenance = src.imported_provenance(None);
            let (back, lost) = Vrm0Meta::from_meta(&src.to_meta(None), Some(&provenance));
            assert_eq!(back.license_name.as_deref(), Some(name), "{name}");
            assert_eq!(lost, vec![], "{name}: nothing is lost, so nothing is warned");
            // Terms narrowed since the import: a name that would grant more than they say is not kept.
            let mut narrowed = src.to_meta(None);
            narrowed.terms.allow_redistribution = false;
            narrowed.terms.modification = Modification::Prohibited;
            narrowed.terms.credit_notation = CreditNotation::Required;
            let (back, _) = Vrm0Meta::from_meta(&narrowed, Some(&provenance));
            assert_eq!(back.license_name.as_deref(), Some("Redistribution_Prohibited"), "{name}: narrowed");
        }
    }

    /// The restrictiveness contract: whatever the source, a 0.x export never names a licence that grants more than the
    /// source did, never turns a non-commercial source into a commercial-friendly name, and keeps commercial use off.
    #[test]
    fn an_export_is_never_more_permissive_than_its_source() {
        let permissive = ["CC0", "CC_BY", "CC_BY_SA", "CC_BY_ND"];
        // 1.0 sources that forbid commercial use, under every document and every sharing setting.
        for url in [VRM_LICENSE_URLS[0], VRM_LICENSE_URLS[1], "https://example.org/terms", ""] {
            for redistribution in [false, true] {
                for modification in [
                    Modification::Prohibited,
                    Modification::AllowModification,
                    Modification::AllowModificationRedistribution,
                ] {
                    for credit in [CreditNotation::Required, CreditNotation::Unnecessary] {
                        let mut m = meta_with(CommercialUsage::PersonalNonProfit, url);
                        m.terms.allow_redistribution = redistribution;
                        m.terms.modification = modification;
                        m.terms.credit_notation = credit;
                        let provenance = m.imported_provenance(None);
                        let (out, _) = Vrm0Meta::from_meta(&m, Some(&provenance));
                        let name = out.license_name.as_deref().unwrap_or_default();
                        assert!(!permissive.contains(&name), "{url:?} {redistribution} {modification:?}: {name}");
                        assert_eq!(out.commercial_ussage_name.as_deref(), Some("Disallow"), "{url:?}");
                        let back =
                            Vrm0Meta { license_name: Some(name.into()), ..Vrm0Meta::default() }.to_meta(None).terms;
                        assert!(
                            !back.allow_redistribution || redistribution,
                            "{url:?}: {name} redistributes what the source did not"
                        );
                        assert!(modification_rank(back.modification) <= modification_rank(modification));
                    }
                }
            }
        }
        // 0.x sources under a non-commercial licence stay non-commercial names, with commercial use off.
        for name in ["CC_BY_NC", "CC_BY_NC_SA", "CC_BY_NC_ND"] {
            let src = v0(name, "Disallow");
            let (out, _) = Vrm0Meta::from_meta(&src.to_meta(None), Some(&src.imported_provenance(None)));
            assert_eq!(out.license_name.as_deref(), Some(name));
            assert_eq!(out.commercial_ussage_name.as_deref(), Some("Disallow"));
            assert_eq!(src.imported_provenance(None).commercial, Commercial::No, "{name}");
        }
    }

    #[test]
    fn extras_of_the_other_version_are_reported_and_same_version_extras_come_back() {
        let from_0x: Vrm0Meta = serde_json::from_value(json!({"title": "T", "vendorTerms": "no resale"})).unwrap();
        let scene_meta = from_0x.to_meta(None);
        let (back, lost) = Vrm0Meta::from_meta(&scene_meta, None);
        assert_eq!(back.extras["vendorTerms"], "no resale");
        assert!(!lost.iter().any(|f| f.rule == "vrm.meta_unknown_keys"));
        assert!(scene_meta.unwritable_extras(VrmVersion::V1).is_some(), "1.0 cannot place a 0.x key");
        assert!(scene_meta.unwritable_extras(VrmVersion::V0).is_none());

        let mut one_zero = meta_with(CommercialUsage::PersonalNonProfit, VRM_LICENSE_URLS[0]);
        one_zero.extras.insert("vendorTerms".into(), json!("no resale"));
        let (back, lost) = Vrm0Meta::from_meta(&one_zero, None);
        assert!(back.extras.is_empty());
        let f = lost.iter().find(|f| f.rule == "vrm.meta_unknown_keys").expect("a warning");
        assert!(f.message.contains("vendorTerms") && f.hint.is_some());
    }
}
