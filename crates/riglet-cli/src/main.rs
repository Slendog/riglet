//! `riglet`: inspect, validate, render and import `.riglet` packages.
//!
//! Exit codes (stable, documented in `riglet --help`):
//!   0  OK
//!   1  the file is invalid (validation issues, not a readable package, or
//!      an Inochi2D file that can't be imported)
//!   2  error: I/O, no GPU adapter, bad arguments
//!
//! Arguments are treated as untrusted input (the CLI may run behind a web
//! service): parameter values must be finite, the output size is capped,
//! and parameter lookups never guess between ambiguous matches.

mod params;

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use riglet_format::model::Node;
use riglet_format::package::PackageError;
use riglet_format::{Document, EvalOutput, Issue, Limits, ParamValues, Rig};
use riglet_render::{FrameOptions, Renderer, RendererOptions};
use serde::Serialize;

const EXIT_OK: u8 = 0;
const EXIT_INVALID: u8 = 1;
const EXIT_ERROR: u8 = 2;

#[derive(Parser)]
#[command(
    name = "riglet",
    version,
    about = "Inspect, validate, render and import .riglet rig packages",
    after_help = "Exit codes: 0 = OK, 1 = the file is invalid (validation issues, or not importable), \
                  2 = error (I/O, no GPU adapter, bad arguments)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Summarize a package: format version, canvas, parameters, nodes.
    Info {
        file: PathBuf,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Check a package against the format rules; list every issue.
    Validate {
        file: PathBuf,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Render one frame to a PNG.
    Render {
        file: PathBuf,
        /// Output PNG path.
        #[arg(short, long)]
        output: PathBuf,
        /// Parameter value, `NAME=VALUE` (or `NAME=X,Y` for 2D parameters).
        /// NAME may be the parameter's id, its name, or an export id; prefix
        /// it with `id:`, `name:`, `live2d:` or `inochi2d:` to pick one kind.
        /// Ambiguous names are an error. Repeatable.
        #[arg(short, long = "param", value_name = "NAME=VALUE")]
        params: Vec<String>,
        /// Output scale (output size = canvas size x scale).
        #[arg(long, default_value_t = 1.0)]
        scale: f32,
        /// Flatten onto an opaque background color, `RRGGBB`.
        #[arg(long, value_name = "RRGGBB")]
        background: Option<String>,
        /// Prefer a software GPU adapter (lavapipe, WARP).
        #[arg(long)]
        software: bool,
    },
    /// Convert an Inochi2D puppet (`.inp`) or project (`.inx`) to a package.
    /// Every lossy step is listed as a warning with a stable code.
    Import {
        file: PathBuf,
        /// Output `.riglet` path.
        #[arg(short, long)]
        output: PathBuf,
        /// Print the warnings as JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

/// A failure with its exit code.
struct Failure {
    code: u8,
    message: String,
}

fn error(message: impl Into<String>) -> Failure {
    Failure { code: EXIT_ERROR, message: message.into() }
}

fn invalid(message: impl Into<String>) -> Failure {
    Failure { code: EXIT_INVALID, message: message.into() }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            // --help and --version are not errors.
            let code = if e.use_stderr() { EXIT_ERROR } else { EXIT_OK };
            return ExitCode::from(code);
        }
    };
    let result = match cli.command {
        Command::Info { file, json } => info(&file, json),
        Command::Validate { file, json } => validate(&file, json),
        Command::Render { file, output, params, scale, background, software } => {
            render(&file, &output, &params, scale, background.as_deref(), software)
        }
        Command::Import { file, output, json } => import(&file, &output, json),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(f) => {
            eprintln!("riglet: {}", f.message);
            ExitCode::from(f.code)
        }
    }
}

fn read(path: &Path) -> Result<Document, Failure> {
    let file = File::open(path).map_err(|e| error(format!("cannot open {}: {e}", path.display())))?;
    riglet_format::read_package(BufReader::new(file), &Limits::default()).map_err(|e| match e {
        PackageError::Io(e) => error(format!("cannot read {}: {e}", path.display())),
        other => invalid(format!("{}: {other}", path.display())),
    })
}

fn print_issues(issues: &[Issue]) {
    for issue in issues {
        println!("{issue}");
    }
}

#[derive(Serialize)]
struct IssueJson<'a> {
    code: &'a str,
    object: Option<&'a str>,
    message: &'a str,
}

fn issues_json(issues: &[Issue]) -> Vec<IssueJson<'_>> {
    issues.iter().map(|i| IssueJson { code: i.code, object: i.object.as_deref(), message: &i.message }).collect()
}

fn print_json<T: Serialize>(value: &T) -> Result<(), Failure> {
    let text = serde_json::to_string_pretty(value).map_err(|e| error(e.to_string()))?;
    println!("{text}");
    Ok(())
}

#[derive(Serialize)]
struct ParamInfo<'a> {
    id: &'a str,
    name: &'a str,
    kind: &'a str,
    min: &'a [f32],
    max: &'a [f32],
    default: &'a [f32],
    #[serde(skip_serializing_if = "Option::is_none")]
    live2d_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inochi2d_id: Option<u32>,
}

#[derive(Serialize, Default)]
struct NodeCounts {
    group: usize,
    composite: usize,
    drawable: usize,
    warp: usize,
    rotation: usize,
    mesh_deformer: usize,
}

#[derive(Serialize)]
struct InfoJson<'a> {
    format: &'a str,
    schema_version: &'a str,
    generator: &'a str,
    canvas: [f32; 2],
    textures: usize,
    parameters: Vec<ParamInfo<'a>>,
    nodes: NodeCounts,
    motions: usize,
    physics_rigs: usize,
    provenance: usize,
    valid: bool,
    issues: Vec<IssueJson<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    binding_work: Option<u64>,
}

fn info(path: &Path, json: bool) -> Result<u8, Failure> {
    let doc = read(path)?;
    let prepared = Rig::prepare(&doc, &Limits::default());
    let issues = prepared.as_ref().err().cloned().unwrap_or_default();
    let m = &doc.model;
    let mut nodes = NodeCounts::default();
    for node in m.nodes.values() {
        match node {
            Node::Group(g) if g.composite => nodes.composite += 1,
            Node::Group(_) => nodes.group += 1,
            Node::Drawable(_) => nodes.drawable += 1,
            Node::Warp(_) => nodes.warp += 1,
            Node::Rotation(_) => nodes.rotation += 1,
            Node::MeshDeformer(_) => nodes.mesh_deformer += 1,
        }
    }
    let parameters = m
        .parameters
        .iter()
        .map(|p| ParamInfo {
            id: &p.id,
            name: &p.name,
            kind: match p.kind {
                riglet_format::model::ParamKind::OneD => "1d",
                riglet_format::model::ParamKind::TwoD => "2d",
            },
            min: &p.min,
            max: &p.max,
            default: &p.default,
            live2d_id: m.export_ids.live2d.as_ref().and_then(|e| e.get(&p.id)).map(String::as_str),
            inochi2d_id: m.export_ids.inochi2d.as_ref().and_then(|e| e.get(&p.id)).copied(),
        })
        .collect();
    let report = InfoJson {
        format: &doc.manifest.format,
        schema_version: &doc.manifest.schema_version,
        generator: &doc.manifest.generator,
        canvas: [m.canvas.width, m.canvas.height],
        textures: m.textures.len(),
        parameters,
        nodes,
        motions: m.motions.len(),
        physics_rigs: m.physics.len(),
        provenance: doc.manifest.provenance.len(),
        valid: issues.is_empty(),
        issues: issues_json(&issues),
        binding_work: prepared.as_ref().ok().map(Rig::binding_work),
    };
    if json {
        print_json(&report)?;
    } else {
        let r = &report;
        println!("format:      {} {}", r.format, r.schema_version);
        println!("generator:   {}", r.generator);
        println!("canvas:      {} x {}", r.canvas[0], r.canvas[1]);
        println!("textures:    {}", r.textures);
        let n = &r.nodes;
        println!(
            "nodes:       {} drawables, {} groups, {} composites, {} warps, {} rotations, {} mesh deformers",
            n.drawable, n.group, n.composite, n.warp, n.rotation, n.mesh_deformer
        );
        println!("motions:     {}    physics rigs: {}", r.motions, r.physics_rigs);
        println!("parameters:  {}", r.parameters.len());
        for p in &r.parameters {
            println!("  {} ({}, {}) min {:?} max {:?} default {:?}", p.name, p.id, p.kind, p.min, p.max, p.default);
        }
        if issues.is_empty() {
            println!("valid:       yes");
        } else {
            println!("valid:       no, {} issue(s); run `riglet validate` for details", issues.len());
        }
    }
    Ok(if issues.is_empty() { EXIT_OK } else { EXIT_INVALID })
}

fn validate(path: &Path, json: bool) -> Result<u8, Failure> {
    let doc = read(path)?;
    let issues = riglet_format::validate(&doc, &Limits::default());
    if json {
        print_json(&serde_json::json!({ "valid": issues.is_empty(), "issues": issues_json(&issues) }))?;
    } else if issues.is_empty() {
        println!("{}: valid", path.display());
    } else {
        print_issues(&issues);
        eprintln!("{}: {} issue(s)", path.display(), issues.len());
    }
    Ok(if issues.is_empty() { EXIT_OK } else { EXIT_INVALID })
}

fn parse_background(text: &str) -> Result<[u8; 3], Failure> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    let bad = || error(format!("--background must be RRGGBB hex, got `{text}`"));
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let channel = |i: usize| hex.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).ok_or_else(bad);
    Ok([channel(0)?, channel(2)?, channel(4)?])
}

fn render(
    path: &Path,
    output: &Path,
    params: &[String],
    scale: f32,
    background: Option<&str>,
    software: bool,
) -> Result<u8, Failure> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err(error("--scale must be a finite number greater than 0"));
    }
    let background = background.map(parse_background).transpose()?;
    let doc = read(path)?;
    let rig = match Rig::prepare(&doc, &Limits::default()) {
        Ok(rig) => rig,
        Err(issues) => {
            print_issues(&issues);
            return Err(invalid(format!("{} is invalid ({} issue(s)); not rendering", path.display(), issues.len())));
        }
    };
    let mut values = ParamValues::defaults(&rig);
    for arg in params {
        let (index, value) = params::parse(arg, &doc, &rig).map_err(error)?;
        if let Some(slot) = values.values.get_mut(index) {
            *slot = value;
        }
    }
    let mut frame = EvalOutput::new();
    rig.evaluate(&values, &mut frame);

    let renderer = Renderer::new(RendererOptions { force_software: software, ..Default::default() })
        .map_err(|e| error(e.to_string()))?;
    let image = riglet_render::render_document(&renderer, &doc, &rig, &frame, &FrameOptions { scale, background })
        .map_err(|e| error(e.to_string()))?;
    let png = image.to_png().map_err(|e| error(e.to_string()))?;
    let mut file = File::create(output).map_err(|e| error(format!("cannot write {}: {e}", output.display())))?;
    file.write_all(&png).map_err(|e| error(format!("cannot write {}: {e}", output.display())))?;
    eprintln!(
        "riglet: wrote {} ({}x{}, adapter `{}`)",
        output.display(),
        image.width,
        image.height,
        renderer.adapter().name
    );
    Ok(EXIT_OK)
}

fn import(path: &Path, output: &Path, json: bool) -> Result<u8, Failure> {
    let options = riglet_inochi2d::Options {
        inx: path.extension().is_some_and(|e| e.eq_ignore_ascii_case("inx")),
        ..Default::default()
    };
    // Read at most one byte past the cap, so an oversized file is rejected
    // by the importer without being read whole.
    let cap = options.limits.zip_max_total_uncompressed;
    let file = File::open(path).map_err(|e| error(format!("cannot open {}: {e}", path.display())))?;
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| error(format!("cannot read {}: {e}", path.display())))?;
    let imported = riglet_inochi2d::import(&bytes, &options)
        .map_err(|e| invalid(format!("{}: {} ({})", path.display(), e, e.code())))?;
    let package = riglet_format::write_package_bytes(&imported.document)
        .map_err(|e| error(format!("cannot write {}: {e}", output.display())))?;
    std::fs::write(output, package).map_err(|e| error(format!("cannot write {}: {e}", output.display())))?;
    let warnings = &imported.warnings;
    if json {
        print_json(&serde_json::json!({ "output": output.display().to_string(), "warnings": warnings }))?;
    } else {
        for w in warnings {
            match &w.object_id {
                Some(id) => println!("warning {} [{id}]: {}", w.code, w.message),
                None => println!("warning {}: {}", w.code, w.message),
            }
        }
        eprintln!("{} -> {}: {} warning(s)", path.display(), output.display(), warnings.len());
    }
    Ok(EXIT_OK)
}
