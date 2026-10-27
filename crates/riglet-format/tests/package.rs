//! Package read/write tests (spec "On disk", V1, Limits).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::io::{Cursor, Write};

use common::*;
use riglet_format::package::PackageError;
use riglet_format::{Document, Limits, read_package_bytes, write_package_bytes};

fn sample() -> Document {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    b.warp("w", "root", 1, 1, &[[0.0, 0.0], [100.0, 0.0], [0.0, 100.0], [100.0, 100.0]]);
    b.drawable("d", "w", &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
    let kfs = vec![b.positions(&[[0.0, 0.0]; 3]), b.positions(&[[0.1, 0.0]; 3])];
    b.set("d", &[linear("p", &[0.0, 1.0])], kfs);
    let mut ext = serde_json::Map::new();
    ext.insert("live2d".into(), serde_json::json!({"unknownThing": [1, 2, 3]}));
    b.model.extensions = Some(ext);
    let mut doc = b.document();
    doc.files.insert("editor.json".into(), b"{\"zoom\": 2}\n".to_vec());
    doc.files.insert("source/art.psd".into(), vec![1, 2, 3, 4]);
    doc
}

fn read(bytes: &[u8]) -> Result<Document, PackageError> {
    read_package_bytes(bytes, &Limits::default())
}

/// Writes a raw ZIP with the given entries (stored or deflated).
fn raw_zip(entries: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
    let method = if deflate { zip::CompressionMethod::Deflated } else { zip::CompressionMethod::Stored };
    let options = zip::write::SimpleFileOptions::default().compression_method(method);
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn json_entries(doc: &Document) -> (Vec<u8>, Vec<u8>) {
    (serde_json::to_vec(&doc.manifest).unwrap(), serde_json::to_vec(&doc.model).unwrap())
}

#[test]
fn round_trip_preserves_everything() {
    let doc = sample();
    let bytes = write_package_bytes(&doc).unwrap();
    let back = read(&bytes).unwrap();
    assert_eq!(back, doc);
    // Unknown entries and extensions survive.
    assert_eq!(back.files["source/art.psd"], vec![1, 2, 3, 4]);
    assert!(back.model.extensions.unwrap().contains_key("live2d"));
}

#[test]
fn output_is_deterministic() {
    let doc = sample();
    assert_eq!(write_package_bytes(&doc).unwrap(), write_package_bytes(&doc).unwrap());
}

#[test]
fn json_uses_lf_and_trailing_newline() {
    let bytes = write_package_bytes(&sample()).unwrap();
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut model = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("model.json").unwrap(), &mut model).unwrap();
    assert!(model.ends_with("}\n"));
    assert!(!model.contains('\r'));
}

#[test]
fn reads_and_migrates_v0_1() {
    let doc = sample();
    let mut manifest = serde_json::to_value(&doc.manifest).unwrap();
    manifest["format"] = "rigproj".into();
    manifest["schemaVersion"] = "0.1.0".into();
    let manifest = serde_json::to_vec(&manifest).unwrap();
    let model = serde_json::to_vec(&doc.model).unwrap();
    let buffers = &doc.files["buffers/data.bin"];
    let png = &doc.files["textures/t.png"];
    let bytes = raw_zip(
        &[("manifest.json", &manifest), ("model.json", &model), ("buffers/data.bin", buffers), ("textures/t.png", png)],
        true,
    );
    let back = read(&bytes).unwrap();
    assert_eq!(back.manifest.format, "riglet");
    assert_eq!(back.manifest.schema_version, riglet_format::migrate::CURRENT_SCHEMA_VERSION);
    assert_eq!(back.model, doc.model);
}

#[test]
fn rejects_missing_or_bad_json() {
    let doc = sample();
    let (manifest, model) = json_entries(&doc);
    assert!(matches!(read(&raw_zip(&[("model.json", &model)], true)), Err(PackageError::Missing("manifest.json"))));
    assert!(matches!(read(&raw_zip(&[("manifest.json", &manifest)], true)), Err(PackageError::Missing("model.json"))));
    let r = read(&raw_zip(&[("manifest.json", &manifest), ("model.json", b"{not json")], true));
    assert!(matches!(r, Err(PackageError::Json { file: "model.json", .. })));
    assert!(matches!(read(b"not a zip"), Err(PackageError::Zip(_))));
}

#[test]
fn rejects_newer_schema() {
    let doc = sample();
    let mut manifest = serde_json::to_value(&doc.manifest).unwrap();
    manifest["schemaVersion"] = "0.5.0".into();
    let manifest = serde_json::to_vec(&manifest).unwrap();
    let model = serde_json::to_vec(&doc.model).unwrap();
    let r = read(&raw_zip(&[("manifest.json", &manifest), ("model.json", &model)], true));
    assert!(matches!(r, Err(PackageError::Migrate(_))));
}

#[test]
fn rejects_unsafe_paths() {
    for name in ["../evil.bin", "/abs.bin", "buffers\\a.bin", "C:/a.bin", "a//b.bin", "./a.bin", "a/../b"] {
        let r = read(&raw_zip(&[(name, b"x")], false));
        assert!(matches!(r, Err(PackageError::BadPath { .. })), "{name}: {r:?}");
    }
}

#[test]
fn rejects_case_collisions() {
    let r = read(&raw_zip(&[("buffers/A.bin", b"x"), ("buffers/a.bin", b"y")], false));
    assert!(matches!(r, Err(PackageError::CaseCollision { .. })), "{r:?}");
}

#[test]
fn enforces_zip_limits() {
    let entries: Vec<(String, Vec<u8>)> = (0..5).map(|i| (format!("f{i}"), vec![0u8; 100])).collect();
    let refs: Vec<(&str, &[u8])> = entries.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    let bytes = raw_zip(&refs, true);

    let limits = Limits { zip_max_entries: 4, ..Limits::default() };
    assert!(matches!(read_package_bytes(&bytes, &limits), Err(PackageError::TooManyEntries { .. })));

    let limits = Limits { zip_max_entry_uncompressed: 99, ..Limits::default() };
    assert!(matches!(read_package_bytes(&bytes, &limits), Err(PackageError::EntryTooLarge { .. })));

    let limits = Limits { zip_max_total_uncompressed: 450, ..Limits::default() };
    assert!(matches!(read_package_bytes(&bytes, &limits), Err(PackageError::TotalTooLarge { .. })));
}

#[test]
fn rejects_zip_bomb_compression_ratio() {
    // 4 MiB of zeros compresses far beyond 1000:1.
    let zeros = vec![0u8; 4 * 1024 * 1024];
    let r = read(&raw_zip(&[("buffers/zeros.bin", &zeros)], true));
    assert!(matches!(r, Err(PackageError::CompressionRatio { .. })), "{r:?}");
}

/// Patches every declared uncompressed size of the first entry.
fn lie_about_size(bytes: &mut [u8], declared: u32) {
    let find = |bytes: &[u8], sig: [u8; 4]| bytes.windows(4).position(|w| w == sig).unwrap();
    let local = find(bytes, [0x50, 0x4b, 0x03, 0x04]);
    bytes[local + 22..local + 26].copy_from_slice(&declared.to_le_bytes());
    let central = find(bytes, [0x50, 0x4b, 0x01, 0x02]);
    bytes[central + 24..central + 28].copy_from_slice(&declared.to_le_bytes());
}

#[test]
fn enforces_declared_size_while_decompressing() {
    let data = vec![7u8; 10_000];
    let mut bytes = raw_zip(&[("buffers/a.bin", &data)], true);
    lie_about_size(&mut bytes, 10);
    assert!(read(&bytes).is_err());
}

#[test]
fn writer_rejects_reserved_and_unsafe_names() {
    let mut doc = sample();
    doc.files.insert("model.json".into(), Vec::new());
    assert!(matches!(write_package_bytes(&doc), Err(PackageError::BadPath { .. })));
    let mut doc = sample();
    doc.files.insert("../x".into(), Vec::new());
    assert!(matches!(write_package_bytes(&doc), Err(PackageError::BadPath { .. })));
}

#[test]
fn reads_v0_2_composites_with_migrated_draw_order() {
    let mut b = Builder::new();
    b.drawable("x", "root", &[[0.0, 0.0]]).set_draw_order("x", 9.0);
    b.composite("c", "root", 1.0, 0.0);
    b.drawable("a", "c", &[[0.0, 0.0]]).set_draw_order("a", 4.0);
    b.drawable("b2", "c", &[[0.0, 0.0]]).set_draw_order("b2", 6.0);
    let doc = b.document();
    // Write it as a v0.2.1 file: no composite drawOrder yet.
    let mut model = serde_json::to_value(&doc.model).unwrap();
    model["nodes"]["c"]["rest"].as_object_mut().unwrap().remove("drawOrder");
    let mut manifest = serde_json::to_value(&doc.manifest).unwrap();
    manifest["schemaVersion"] = "0.2.1".into();
    let (manifest, model) = (serde_json::to_vec(&manifest).unwrap(), serde_json::to_vec(&model).unwrap());
    let bytes = raw_zip(
        &[
            ("manifest.json", &manifest),
            ("model.json", &model),
            ("buffers/data.bin", &doc.files["buffers/data.bin"]),
            ("textures/t.png", &doc.files["textures/t.png"]),
        ],
        true,
    );
    let back = read(&bytes).unwrap();
    let riglet_format::model::Node::Group(c) = &back.model.nodes["c"] else { panic!() };
    assert_eq!(c.rest.draw_order, Some(4.0));
    assert!(riglet_format::Rig::prepare(&back, &Limits::default()).is_ok());
}
