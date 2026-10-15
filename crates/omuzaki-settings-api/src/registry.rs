//! The registry: every [`Setting`] declared with [`register_setting!`](crate::register_setting), in one place for the
//! generic readers (the Settings page, `settings.list`, the store, the agent's schema).

use std::sync::OnceLock;

use omuzaki_doc_api::Value;
use schemars::Schema;

use crate::error::SettingError;
use crate::meta::SettingMeta;
use crate::setting::Setting;

/// One registered setting, with the typed operations erased behind function pointers.
#[derive(Debug)]
pub struct SettingRegistration {
    /// The package that registered it (`CARGO_PKG_NAME`), so a crate's own check test looks at its own keys only.
    pub package: &'static str,
    /// Static facts.
    pub meta: SettingMeta,
    /// Earlier keys this one replaces.
    pub renamed_from: &'static [&'static str],
    schema: fn() -> Schema,
    default_json: fn() -> Value,
    check: fn(&Value) -> Result<(), SettingError>,
    migrate: fn(u32, Value) -> Result<Value, SettingError>,
}

fn schema_of<S: Setting>() -> Schema {
    schemars::schema_for!(S::Value)
}

fn default_of<S: Setting>() -> Value {
    // A default that cannot serialise reads as null, which `check_crate!` reports; serialising never panics.
    serde_json::to_value(S::default()).unwrap_or(Value::Null)
}

fn check_of<S: Setting>(value: &Value) -> Result<(), SettingError> {
    let typed: S::Value =
        serde_json::from_value(value.clone()).map_err(|e| SettingError::wrong_type(S::META.key, e))?;
    S::check(&typed)
}

fn migrate_of<S: Setting>(from: u32, old: Value) -> Result<Value, SettingError> {
    S::migrate(from, old)
}

impl SettingRegistration {
    /// The registration for `S`. A `const fn` so [`register_setting!`](crate::register_setting) works at link time.
    #[must_use]
    pub const fn of<S: Setting>(package: &'static str) -> Self {
        Self {
            package,
            meta: S::META,
            renamed_from: S::RENAMED_FROM,
            schema: schema_of::<S>,
            default_json: default_of::<S>,
            check: check_of::<S>,
            migrate: migrate_of::<S>,
        }
    }

    /// The JSON Schema of the value type (with its `x-ui.*` annotations).
    #[must_use]
    pub fn schema(&self) -> Schema {
        (self.schema)()
    }

    /// The default value as JSON. A default that cannot be serialised reads as `null`, which TOML cannot write, so
    /// [`check_crate!`](crate::check_crate) fails it: it is never silently accepted.
    #[must_use]
    pub fn default_json(&self) -> Value {
        (self.default_json)()
    }

    /// Checks `value` against the type (a wrong type is rejected) and then `Setting::check` (a range is enforced).
    ///
    /// # Errors
    /// A [`SettingError`] that names this key.
    pub fn validate(&self, value: &Value) -> Result<(), SettingError> {
        (self.check)(value)
    }

    /// Runs `Setting::migrate` on a value stored at version `from`.
    ///
    /// # Errors
    /// A [`SettingError`] that names this key.
    pub fn migrate(&self, from: u32, old: Value) -> Result<Value, SettingError> {
        (self.migrate)(from, old)
    }
}

inventory::collect!(SettingRegistration);

fn sorted() -> &'static [&'static SettingRegistration] {
    static SORTED: OnceLock<Vec<&'static SettingRegistration>> = OnceLock::new();
    SORTED.get_or_init(|| {
        // Link order is not stable between builds; the key order is, so listings and snapshots are deterministic.
        let mut all: Vec<_> = inventory::iter::<SettingRegistration>.into_iter().collect();
        all.sort_by_key(|r| r.meta.key);
        all
    })
}

/// Every registered setting, sorted by key.
pub fn registrations() -> impl Iterator<Item = &'static SettingRegistration> {
    sorted().iter().copied()
}

/// The registration for `key`.
#[must_use]
pub fn find(key: &str) -> Option<&'static SettingRegistration> {
    let all = sorted();
    all.binary_search_by(|r| r.meta.key.cmp(key)).ok().map(|i| all[i])
}

/// Registers a setting type: `register_setting!(UiScale);`. Put it next to the type.
#[macro_export]
macro_rules! register_setting {
    ($ty:ty) => {
        $crate::__inventory::submit! {
            $crate::registry::SettingRegistration::of::<$ty>(::core::env!("CARGO_PKG_NAME"))
        }
        $crate::__record_registration!("setting", $ty);
    };
}
