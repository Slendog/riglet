//! Schema migration hook. Readers migrate older files to
//! [`CURRENT_SCHEMA_VERSION`] before deserializing them into typed structs.

use serde_json::Value;

pub const FORMAT_NAME: &str = "riglet";
pub const CURRENT_SCHEMA_VERSION: &str = "0.3.0";
const CURRENT: Version = Version { major: 0, minor: 3, patch: 0 };

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
    if version.minor < 3 {
        v0_2_to_v0_3(model);
    }
    // Patch versions only clarify rules, so any older version just takes
    // the current version number.
    if let Some(obj) = manifest.as_object_mut() {
        obj.insert("schemaVersion".into(), Value::from(CURRENT_SCHEMA_VERSION));
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

/// v0.3 gave composite groups their own draw order. Each composite gets
/// the lowest rest drawOrder of the drawables in its subtree (0 if none), so
/// it sorts where its first drawable used to. Works on untrusted JSON: any
/// missing or malformed piece is skipped (validation reports it later), and
/// cycles can't loop because every node is visited at most once per walk.
fn v0_2_to_v0_3(model: &mut Value) {
    let Some(nodes) = model.get("nodes").and_then(Value::as_object) else { return };
    let mut composites = Vec::new();
    for (id, node) in nodes {
        let is_group = node.get("type").and_then(Value::as_str) == Some("group");
        let composite = node.get("composite").and_then(Value::as_bool) == Some(true);
        let has_order = node.get("rest").and_then(|r| r.get("drawOrder")).is_some();
        if is_group && composite && !has_order {
            composites.push(id.clone());
        }
    }
    let mut orders = Vec::with_capacity(composites.len());
    for id in &composites {
        let mut lowest: Option<f64> = None;
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![id.as_str()];
        while let Some(current) = stack.pop() {
            if !seen.insert(current) {
                continue;
            }
            let Some(node) = nodes.get(current) else { continue };
            if node.get("type").and_then(Value::as_str) == Some("drawable")
                && let Some(order) = node.get("rest").and_then(|r| r.get("drawOrder")).and_then(Value::as_f64)
            {
                lowest = Some(lowest.map_or(order, |l| l.min(order)));
            }
            if let Some(children) = node.get("children").and_then(Value::as_array) {
                stack.extend(children.iter().filter_map(Value::as_str));
            }
        }
        orders.push(lowest.unwrap_or(0.0));
    }
    let Some(nodes) = model.get_mut("nodes").and_then(Value::as_object_mut) else { return };
    for (id, order) in composites.iter().zip(orders) {
        if let Some(rest) = nodes.get_mut(id).and_then(|n| n.get_mut("rest")).and_then(Value::as_object_mut) {
            rest.insert("drawOrder".into(), Value::from(order));
        }
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
        let mut manifest = json!({"format": "riglet", "schemaVersion": CURRENT_SCHEMA_VERSION});
        let before = manifest.clone();
        assert!(migrate(&mut manifest, &mut json!({})).is_ok());
        assert_eq!(manifest, before);
    }

    #[test]
    fn older_patch_reads_without_migration() {
        let mut manifest = json!({"format": "riglet", "schemaVersion": "0.2.0"});
        assert_eq!(migrate(&mut manifest, &mut json!({})), Ok("0.2.0".to_owned()));
        assert_eq!(manifest["schemaVersion"], CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn newer_patch_of_current_minor_is_readable() {
        let mut manifest = json!({"format": "riglet", "schemaVersion": "0.3.9"});
        assert!(migrate(&mut manifest, &mut json!({})).is_ok());
    }

    #[test]
    fn v0_2_composites_get_their_lowest_drawable_order() {
        let mut manifest = json!({"format": "riglet", "schemaVersion": "0.2.1"});
        let mut model = json!({"nodes": {
            "root": {"type": "group", "composite": false, "children": ["c", "x"], "rest": {"opacity": 1}},
            "c": {"type": "group", "composite": true, "children": ["a", "inner"], "rest": {"opacity": 1}},
            "inner": {"type": "group", "composite": true, "children": ["b"], "rest": {"opacity": 1}},
            "empty": {"type": "group", "composite": true, "children": [], "rest": {"opacity": 1}},
            "loop": {"type": "group", "composite": true, "children": ["loop"], "rest": {"opacity": 1}},
            "a": {"type": "drawable", "children": [], "rest": {"drawOrder": 7.0}},
            "b": {"type": "drawable", "children": [], "rest": {"drawOrder": 3.5}},
            "x": {"type": "drawable", "children": [], "rest": {"drawOrder": -100.0}}
        }});
        assert!(migrate(&mut manifest, &mut model).is_ok());
        let order = |id: &str| model["nodes"][id]["rest"]["drawOrder"].as_f64();
        assert_eq!(order("c"), Some(3.5));
        assert_eq!(order("inner"), Some(3.5));
        assert_eq!(order("empty"), Some(0.0));
        assert_eq!(order("loop"), Some(0.0));
        // Non-composites are untouched.
        assert_eq!(order("root"), None);
    }

    #[test]
    fn v0_2_migration_survives_malformed_models() {
        for model in [json!(null), json!({"nodes": 5}), json!({"nodes": {"g": {"type": "group", "composite": true}}})] {
            let mut model = model;
            let mut manifest = json!({"format": "riglet", "schemaVersion": "0.2.0"});
            assert!(migrate(&mut manifest, &mut model).is_ok());
        }
    }

    #[test]
    fn rejects_newer_and_malformed_versions() {
        for v in ["0.4.0", "1.0.0", "0.2", "0.2.0.1", "a.b.c", "0.2.-1", ""] {
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
