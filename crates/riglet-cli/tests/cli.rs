//! End-to-end tests of the `riglet` binary: exit codes, output formats,
//! parameter lookup, and untrusted-argument handling.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use riglet_fixtures::Builder;

fn riglet(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_riglet")).args(args).output().unwrap()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A per-test scratch directory under the target dir.
fn scratch(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli").join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, name: &str, b: &Builder) -> String {
    let path = dir.join(name);
    std::fs::write(&path, riglet_format::write_package_bytes(&b.document()).unwrap()).unwrap();
    path.to_str().unwrap().to_owned()
}

fn sample() -> Builder {
    let mut b = Builder::new();
    b.model.canvas = riglet_format::model::Canvas { width: 32.0, height: 24.0 };
    b.texture("red", 1, 1, &[255, 0, 0, 255], false);
    b.param("head_x", -30.0, 30.0, 0.0);
    b.param2d("eye", [-1.0, -1.0], [1.0, 1.0]);
    // A parameter whose NAME equals another's id: an ambiguity trap.
    b.param("p3", 0.0, 1.0, 0.0);
    b.model.parameters[2].name = "head_x".into();
    b.model.export_ids.live2d = Some([("eye".to_owned(), "ParamEyeBallX".to_owned())].into());
    b.model.export_ids.inochi2d = Some([("p3".to_owned(), 4242)].into());
    b.quad("q", "root", "red", [-8.0, -8.0], [8.0, 8.0]);
    b
}

#[test]
fn help_and_version_exit_zero() {
    let out = riglet(&["--help"]);
    assert_eq!(code(&out), 0);
    assert!(text(&out.stdout).contains("Exit codes: 0 = OK, 1 = the file is invalid"));
    assert_eq!(code(&riglet(&["--version"])), 0);
}

#[test]
fn bad_arguments_exit_two() {
    assert_eq!(code(&riglet(&[])), 2);
    assert_eq!(code(&riglet(&["frobnicate"])), 2);
    assert_eq!(code(&riglet(&["info"])), 2);
}

#[test]
fn missing_file_is_an_error() {
    let out = riglet(&["info", "/nonexistent/riglet/file.riglet"]);
    assert_eq!(code(&out), 2);
    assert!(text(&out.stderr).contains("cannot open"));
}

#[test]
fn info_text_and_json() {
    let dir = scratch("info");
    let file = write(&dir, "m.riglet", &sample());
    let out = riglet(&["info", &file]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    let format_line = format!("format:      riglet {}", riglet_format::migrate::CURRENT_SCHEMA_VERSION);
    assert!(stdout.contains(&format_line), "{stdout}");
    assert!(stdout.contains("canvas:      32 x 24"));
    assert!(stdout.contains("valid:       yes"));

    let out = riglet(&["info", &file, "--json"]);
    assert_eq!(code(&out), 0);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["valid"], true);
    assert_eq!(json["nodes"]["drawable"], 1);
    assert_eq!(json["parameters"][1]["kind"], "2d");
    assert_eq!(json["parameters"][1]["live2d_id"], "ParamEyeBallX");
    assert_eq!(json["parameters"][2]["inochi2d_id"], 4242);
}

#[test]
fn validate_exit_codes() {
    let dir = scratch("validate");
    let good = write(&dir, "good.riglet", &sample());
    let out = riglet(&["validate", &good]);
    assert_eq!(code(&out), 0);
    assert!(text(&out.stdout).contains("valid"));

    let mut b = sample();
    b.model.parameters[0].max = vec![-100.0];
    let bad = write(&dir, "bad.riglet", &b);
    let out = riglet(&["validate", &bad]);
    assert_eq!(code(&out), 1);
    assert!(text(&out.stdout).contains("V4_PARAM_RANGE"));
    let out = riglet(&["validate", &bad, "--json"]);
    assert_eq!(code(&out), 1);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["valid"], false);
    assert_eq!(json["issues"][0]["code"], "V4_PARAM_RANGE");

    // Not a package at all: the file is invalid (1), not an I/O error.
    let junk = dir.join("junk.riglet");
    std::fs::write(&junk, b"not a zip").unwrap();
    assert_eq!(code(&riglet(&["validate", junk.to_str().unwrap()])), 1);
}

/// Renders, or returns None when this machine has no GPU adapter.
fn render(args: &[&str]) -> Option<Output> {
    let out = riglet(args);
    if code(&out) == 2 && text(&out.stderr).contains("no GPU adapter") {
        if std::env::var("RIGLET_REQUIRE_GPU").as_deref() == Ok("1") {
            panic!("RIGLET_REQUIRE_GPU=1 but no adapter: {}", text(&out.stderr));
        }
        eprintln!("cli render tests: SKIPPED, no GPU adapter");
        return None;
    }
    Some(out)
}

#[test]
fn render_writes_a_png() {
    let dir = scratch("render");
    let file = write(&dir, "m.riglet", &sample());
    let png = dir.join("out.png");
    let png = png.to_str().unwrap();
    let Some(out) = render(&["render", &file, "-o", png, "--param", "eye=0.5,-0.5", "--scale", "2"]) else {
        return;
    };
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let image = riglet_render::Image::from_png(&std::fs::read(png).unwrap()).unwrap();
    assert_eq!((image.width, image.height), (64, 48));
    // Center pixel is the red quad.
    let center = ((24 * 64 + 32) * 4) as usize;
    assert_eq!(&image.rgba[center..center + 4], &[255, 0, 0, 255]);
    // Corner is transparent, or the background when one is given.
    assert_eq!(image.rgba[3], 0);
    let out = render(&["render", &file, "-o", png, "--background", "#102030"]).unwrap();
    assert_eq!(code(&out), 0);
    let image = riglet_render::Image::from_png(&std::fs::read(png).unwrap()).unwrap();
    assert_eq!(&image.rgba[0..4], &[0x10, 0x20, 0x30, 255]);
}

#[test]
fn param_lookup_by_id_name_and_export_id() {
    let dir = scratch("params");
    let file = write(&dir, "m.riglet", &sample());
    let png = dir.join("out.png");
    let png = png.to_str().unwrap();
    // `eye` (id), `ParamEyeBallX` (Live2D id) and `4242` (Inochi2D id) are
    // unique.
    for p in ["eye=1,1", "ParamEyeBallX=0,0", "4242=0.5"] {
        let Some(out) = render(&["render", &file, "-o", png, "-p", p]) else { return };
        assert_eq!(code(&out), 0, "{p}: {}", text(&out.stderr));
    }
}

#[test]
fn param_errors_are_exit_two_and_list_candidates() {
    let dir = scratch("param_errors");
    let file = write(&dir, "m.riglet", &sample());
    let png = dir.join("out.png");
    let png = png.to_str().unwrap();
    // `head_x` is one parameter's id and another's name: ambiguous.
    let out = riglet(&["render", &file, "-o", png, "-p", "head_x=1"]);
    assert_eq!(code(&out), 2);
    let err = text(&out.stderr);
    assert!(err.contains("ambiguous") && err.contains("id head_x") && err.contains("id p3"), "{err}");

    for bad in ["nope=1", "eye=1", "eye=1,2,3", "p3=abc", "p3=NaN", "p3=inf", "p3=1e40", "p3", "eye=1,nan"] {
        let out = riglet(&["render", &file, "-o", png, "-p", bad]);
        assert_eq!(code(&out), 2, "{bad}: {}", text(&out.stderr));
        assert!(!dir.join("out.png").exists(), "{bad}: wrote an image");
    }
    // A prefix resolves it, and only matches that kind.
    for p in ["id:head_x=1", "id:p3=1", "live2d:ParamEyeBallX=0,0", "inochi2d:4242=1"] {
        let Some(out) = render(&["render", &file, "-o", png, "-p", p]) else { return };
        assert_eq!(code(&out), 0, "{p}: {}", text(&out.stderr));
        let _ = std::fs::remove_file(png);
    }
    // Both parameters are NAMED head_x (the builder names p0 after its id),
    // so `name:` stays ambiguous; `live2d:` finds nothing.
    for p in ["name:head_x=1", "live2d:head_x=1"] {
        assert_eq!(code(&riglet(&["render", &file, "-o", png, "-p", p])), 2, "{p}");
    }
    let long = format!("p3={}", "1".repeat(5000));
    assert_eq!(code(&riglet(&["render", &file, "-o", png, "-p", &long])), 2);
}

#[test]
fn output_size_and_scale_are_capped() {
    let dir = scratch("caps");
    let file = write(&dir, "m.riglet", &sample());
    let png = dir.join("out.png");
    let png = png.to_str().unwrap();
    for scale in ["0", "-1", "nan", "inf"] {
        let out = riglet(&["render", &file, "-o", png, "--scale", scale]);
        assert_eq!(code(&out), 2, "scale {scale}");
    }
    // 32 x 1000 = 32000 px wide: over the 8192 cap.
    let Some(out) = render(&["render", &file, "-o", png, "--scale", "1000"]) else { return };
    assert_eq!(code(&out), 2);
    assert!(text(&out.stderr).contains("output size"), "{}", text(&out.stderr));
    for bg in ["red", "#12345", "zzzzzz", "#1234567"] {
        assert_eq!(code(&riglet(&["render", &file, "-o", png, "--background", bg])), 2, "{bg}");
    }
}

#[test]
fn rendering_an_invalid_file_exits_one() {
    let dir = scratch("render_invalid");
    let mut b = sample();
    b.model.parameters[0].max = vec![-100.0];
    let file = write(&dir, "bad.riglet", &b);
    let out = riglet(&["render", &file, "-o", dir.join("x.png").to_str().unwrap()]);
    assert_eq!(code(&out), 1);
    assert!(text(&out.stdout).contains("V4_PARAM_RANGE"));
}

/// The committed fuzz seed: a small puppet using most importer features.
const INOCHI_SEED: &[u8] = include_bytes!("../../../fuzz/seeds/inp_container/rich.inp");

#[test]
fn import_writes_a_valid_package() {
    let dir = scratch("import");
    let input = dir.join("puppet.inp");
    std::fs::write(&input, INOCHI_SEED).unwrap();
    let output = dir.join("puppet.riglet");
    let out = riglet(&["import", input.to_str().unwrap(), "-o", output.to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("warning(s)"));
    for line in text(&out.stdout).lines() {
        assert!(line.starts_with("warning "), "unexpected output line: {line}");
    }
    let validated = riglet(&["validate", output.to_str().unwrap()]);
    assert_eq!(code(&validated), 0, "{}", text(&validated.stdout));

    let out = riglet(&["import", input.to_str().unwrap(), "-o", output.to_str().unwrap(), "--json"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let warnings = v["warnings"].as_array().unwrap();
    assert!(warnings.iter().all(|w| w["code"].is_string() && w["message"].is_string()));
}

#[test]
fn import_rejects_non_inochi_files() {
    let dir = scratch("import_invalid");
    let input = dir.join("not.inp");
    std::fs::write(&input, b"definitely not a puppet").unwrap();
    let output = dir.join("out.riglet");
    let out = riglet(&["import", input.to_str().unwrap(), "-o", output.to_str().unwrap()]);
    assert_eq!(code(&out), 1);
    assert!(text(&out.stderr).contains("INP_NOT_INOCHI"), "{}", text(&out.stderr));
    assert!(!output.exists());

    std::fs::write(&input, b"TRNSRTS2").unwrap();
    let out = riglet(&["import", input.to_str().unwrap(), "-o", output.to_str().unwrap()]);
    assert_eq!(code(&out), 1);
    assert!(text(&out.stderr).contains("INP2_UNSUPPORTED"), "{}", text(&out.stderr));
}

#[test]
fn import_io_errors_exit_two() {
    let dir = scratch("import_io");
    let out = riglet(&["import", "/nonexistent/riglet/puppet.inp", "-o", "out.riglet"]);
    assert_eq!(code(&out), 2);
    assert!(text(&out.stderr).contains("cannot open"));

    let input = dir.join("puppet.inp");
    std::fs::write(&input, INOCHI_SEED).unwrap();
    let output = dir.join("missing-dir").join("out.riglet");
    let out = riglet(&["import", input.to_str().unwrap(), "-o", output.to_str().unwrap()]);
    assert_eq!(code(&out), 2);
    assert!(text(&out.stderr).contains("cannot write"));
}
