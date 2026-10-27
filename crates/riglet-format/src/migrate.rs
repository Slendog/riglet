//! Schema migration hook. Readers migrate older files to
//! [`CURRENT_SCHEMA_VERSION`] before deserializing them into typed structs.

use serde_json::Value;

pub const FORMAT_NAME: &str = "riglet";
pub const CURRENT_SCHEMA_VERSION: &str = "0.2.0";
const CURRENT: Version = Version { major: 0, minor: 2, patch: 0 };

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MigrateError {
    #[error("manifest.json is not a JSON object")]
    ManifestNotObject,
    #[error("manifest field `{0}` is missing or not a string")]
    MissingField(&'static str),
    #[error("manifest format `{0}` is not a Riglet package")]
    WrongFormat(String),
    #[error("schemaVersion `{0}` is not a valid semver version")]
    BadVersion(String),
    #[error("schemaVersion `{found}` is newer than this reader supports ({supported})")]
    TooNew { found: String, supported: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

fn parse_version(s: &str) -> Option<Version> {
    // Pre-release and build suffixes are not used by this format.
    let mut parts = s.split('.');
    let mut next = || -> Option<u64> {
        let p = parts.next()?;
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    let v = Version { major: next()?, minor: next()?, patch: next()? };
    parts.next().is_none().then_some(v)
}

/// Upgrades `manifest` and `model` in place to the current schema version.
/// Returns the version the file was written with.
pub fn migrate(manifest: &mut Value, model: &mut Value) -> Result<String, MigrateError> {
    let obj = manifest.as_object_mut().ok_or(MigrateError::ManifestNotObject)?;
    let format = obj.get("format").and_then(Value::as_str).ok_or(MigrateError::MissingField("format"))?.to_owned();
    let version_str =
        obj.get("schemaVersion").and_then(Value::as_str).ok_or(MigrateError::MissingField("schemaVersion"))?.to_owned();
    let version = parse_version(&version_str).ok_or_else(|| MigrateError::BadVersion(version_str.clone()))?;

    // While major is 0, every minor version can break compatibility, so
    // only same-or-older minors are readable.
    if version.major != CURRENT.major || version.minor > CURRENT.minor {
        return Err(MigrateError::TooNew { found: version_str, supported: CURRENT_SCHEMA_VERSION });
    }

    let expected_format = if version.minor < 2 { "rigproj" } else { FORMAT_NAME };
    if format != expected_format {
        return Err(MigrateError::WrongFormat(format));
    }

    if version.minor < 2 {
        v0_1_to_v0_2(manifest, model);
    }
    Ok(version_str)
}

/// v0.2 renamed the package format and only added evaluation rules, so no
/// model data changes.
fn v0_1_to_v0_2(manifest: &mut Value, _model: &mut Value) {
    if let Some(obj) = manifest.as_object_mut() {
        obj.insert("format".into(), Value::from(FORMAT_NAME));
        obj.insert("schemaVersion".into(), Value::from("0.2.0"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn migrates_v0_1_format_name() {
        let mut manifest = json!({"format": "rigproj", "schemaVersion": "0.1.0"});
        let mut model = json!({});
        assert_eq!(migrate(&mut manifest, &mut model), Ok("0.1.0".to_owned()));
        assert_eq!(manifest["format"], "riglet");
        assert_eq!(manifest["schemaVersion"], CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn current_version_is_unchanged() {
        let mut manifest = json!({"format": "riglet", "schemaVersion": "0.2.0"});
        let before = manifest.clone();
        assert!(migrate(&mut manifest, &mut json!({})).is_ok());
        assert_eq!(manifest, before);
    }

    #[test]
    fn rejects_newer_and_malformed_versions() {
        for v in ["0.3.0", "1.0.0", "0.2", "0.2.0.1", "a.b.c", "0.2.-1", ""] {
            let mut manifest = json!({"format": "riglet", "schemaVersion": v});
            assert!(migrate(&mut manifest, &mut json!({})).is_err(), "{v}");
        }
    }

    #[test]
    fn rejects_wrong_format() {
        let mut manifest = json!({"format": "riglet", "schemaVersion": "0.1.0"});
        assert!(matches!(migrate(&mut manifest, &mut json!({})), Err(MigrateError::WrongFormat(_))));
        let mut manifest = json!({"format": "other", "schemaVersion": "0.2.0"});
        assert!(matches!(migrate(&mut manifest, &mut json!({})), Err(MigrateError::WrongFormat(_))));
    }
}
