//! Typed ids of a scene. Each is a validated slug, so it is safe as a file name and a TOML key. Ids are stable: they
//! appear in shows, automation rules and library records, so renaming one is a migration (ADR 0024).

use omuzaki_core::slug_id;

use crate::error::SceneError;

slug_id!(
    /// A scene in a project (`scenes/<id>/`).
    SceneId
);
slug_id!(
    /// A character placed in a scene (`scenes/<id>/cast/<member>`). Never `stage` or `subject`: those name anchors.
    MemberId
);
slug_id!(
    /// A light of a scene's lighting (`lighting.lights.<id>`): `key`, `fill`, `rim`.
    LightId
);
slug_id!(
    /// A camera of a scene (`scenes/<id>/cameras/<camera>`): `full`, `bust`, `face`.
    CameraId
);
slug_id!(
    /// A prop of a scene (`scenes/<id>/props/<prop>`).
    PropId
);

impl MemberId {
    /// The member ids that name an anchor and so can never name a member.
    pub const RESERVED: [&'static str; 2] = ["stage", "subject"];

    /// Validates `s` as a member id: a slug that isn't [`RESERVED`](Self::RESERVED).
    ///
    /// # Errors
    /// [`SceneError`] naming the rule when `s` is reserved or not a slug.
    pub fn checked(s: &str) -> Result<Self, SceneError> {
        let id = Self::new(s).map_err(|e| SceneError::new("member", e.to_string()))?;
        if id.is_reserved() {
            Err(SceneError::new("member", format!("`{s}` names an anchor, so it can't be a member id")))
        } else {
            Ok(id)
        }
    }

    /// True for `stage` and `subject`.
    #[must_use]
    pub fn is_reserved(&self) -> bool {
        Self::RESERVED.contains(&self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_member_ids_are_refused() {
        for reserved in MemberId::RESERVED {
            let err = MemberId::checked(reserved).unwrap_err();
            assert!(err.rule.contains("anchor"), "{err}");
        }
        assert_eq!(MemberId::checked("kiri").unwrap().as_str(), "kiri");
        assert!(MemberId::checked("Not A Slug").is_err());
    }
}
