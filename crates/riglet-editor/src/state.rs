//! The editor's document state, independent of any UI toolkit.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use riglet_format::model::{ConversionWarning, Id};
use riglet_format::{Document, EvalOutput, Issue, Limits, PackageError, ParamValues, Rig};

use crate::command::{Command, CommandError};
use crate::history::{History, HistoryLimits};
use crate::save::write_atomic;

/// What a model was opened from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Riglet,
    Inp,
    Inx,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: PathBuf,
    pub kind: SourceKind,
}

/// A file read and converted, ready for [`EditorState::open`]. Produced by
/// [`load`], which the UI runs on a background thread.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub document: Document,
    pub rig: Rig,
    pub source: Source,
    /// Import warnings (empty for `.riglet` files, whose earlier import
    /// warnings are in `manifest.provenance`).
    pub import_warnings: Vec<ConversionWarning>,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{path} is neither a .riglet package nor an Inochi2D file")]
    UnknownFormat { path: PathBuf },
    #[error("{path}: {source}")]
    Package { path: PathBuf, source: PackageError },
    #[error("{path}: {source} ({})", source.code())]
    Import { path: PathBuf, source: riglet_inochi2d::InochiError },
    #[error("{path} is not a valid model ({} issue(s))", issues.len())]
    Invalid { path: PathBuf, issues: Vec<Issue> },
}

const ZIP_MAGIC: &[u8] = b"PK\x03\x04";
const INOCHI_MAGIC: &[u8] = b"TRNSRTS";

/// Reads a `.riglet`, `.inp` or `.inx` file (recognized by content; `.inx`
/// by extension, as in the CLI) through the same parsers, limits and
/// validation as the CLI. The file is read at most one byte past
/// `limits.zip_max_total_uncompressed`, so an oversized file is rejected
/// without being read whole.
pub fn load(path: &Path, limits: &Limits) -> Result<Loaded, LoadError> {
    let io = |source| LoadError::Io { path: path.to_owned(), source };
    let file = std::fs::File::open(path).map_err(io)?;
    let mut bytes = Vec::new();
    file.take(limits.zip_max_total_uncompressed.saturating_add(1)).read_to_end(&mut bytes).map_err(io)?;
    let (document, kind, import_warnings) = if bytes.starts_with(ZIP_MAGIC) {
        let doc = riglet_format::read_package_bytes(&bytes, limits)
            .map_err(|source| LoadError::Package { path: path.to_owned(), source })?;
        (doc, SourceKind::Riglet, Vec::new())
    } else if bytes.starts_with(INOCHI_MAGIC) {
        let inx = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("inx"));
        let options = riglet_inochi2d::Options { limits: limits.clone(), inx };
        let imported = riglet_inochi2d::import(&bytes, &options)
            .map_err(|source| LoadError::Import { path: path.to_owned(), source })?;
        (imported.document, if inx { SourceKind::Inx } else { SourceKind::Inp }, imported.warnings)
    } else {
        return Err(LoadError::UnknownFormat { path: path.to_owned() });
    };
    let rig = Rig::prepare(&document, limits).map_err(|issues| LoadError::Invalid { path: path.to_owned(), issues })?;
    Ok(Loaded { document, rig, source: Source { path: path.to_owned(), kind }, import_warnings })
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("this model has no .riglet file yet; choose where to save it")]
    NoPath,
    #[error("models are saved as .riglet files; use Export for Inochi2D")]
    NotRiglet,
    #[error("{0} is the file this model was imported from; save the .riglet elsewhere")]
    WouldOverwriteImport(PathBuf),
    #[error("cannot encode the package: {0}")]
    Encode(PackageError),
    #[error("cannot write {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
}

/// View-only state: lives in memory only, is never saved, and never
/// changes the [`Document`] (so it never marks it dirty).
#[derive(Debug, Clone, PartialEq)]
pub struct ViewState {
    pub zoom: f32,
    pub pan: [f32; 2],
    /// Solid straight-RGB background, or `None` for the checkerboard.
    pub background: Option<[u8; 3]>,
    /// Nodes hidden in the viewport only (not the model's `visible`).
    pub hidden: BTreeSet<Id>,
    pub selection: Option<Id>,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState { zoom: 1.0, pan: [0.0, 0.0], background: None, hidden: BTreeSet::new(), selection: None }
    }
}

/// An open model: the document, its prepared rig, parameter values, the
/// last evaluation, the undo history and the view state.
pub struct EditorState {
    document: Document,
    rig: Rig,
    limits: Limits,
    source: Source,
    save_path: Option<PathBuf>,
    history: History,
    params: ParamValues,
    eval: EvalOutput,
    eval_stale: bool,
    import_warnings: Vec<ConversionWarning>,
    pub view: ViewState,
}

impl EditorState {
    /// Opens a loaded file. A `.riglet` starts clean with itself as the save
    /// path; an import starts dirty with no save path, so Save asks where
    /// to write the `.riglet` and never writes over the imported file.
    pub fn open(loaded: Loaded, limits: Limits) -> Self {
        Self::open_with(loaded, limits, HistoryLimits::default())
    }

    pub fn open_with(loaded: Loaded, limits: Limits, history: HistoryLimits) -> Self {
        let Loaded { document, rig, source, import_warnings } = loaded;
        let is_riglet = source.kind == SourceKind::Riglet;
        let params = ParamValues::defaults(&rig);
        EditorState {
            document,
            rig,
            limits,
            save_path: is_riglet.then(|| source.path.clone()),
            source,
            history: History::new(history, is_riglet),
            params,
            eval: EvalOutput::new(),
            eval_stale: true,
            import_warnings,
            view: ViewState::default(),
        }
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn rig(&self) -> &Rig {
        &self.rig
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    pub fn save_path(&self) -> Option<&Path> {
        self.save_path.as_deref()
    }

    pub fn import_warnings(&self) -> &[ConversionWarning] {
        &self.import_warnings
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    pub fn is_dirty(&self) -> bool {
        self.history.is_dirty()
    }

    // ---------- Commands ----------

    /// Applies `command` as one undo step (merged into the previous step
    /// when `coalesce` is set and it edits the same field, see
    /// [`History::seal`]). The model is validated afterwards; a command
    /// that would make it invalid is reverted and returns an error.
    pub fn apply(&mut self, command: Command, coalesce: bool) -> Result<(), CommandError> {
        let inverse = self.apply_checked(&command)?;
        self.history.push(command, inverse, coalesce);
        Ok(())
    }

    pub fn seal(&mut self) {
        self.history.seal();
    }

    /// Undoes the last step. `Ok(false)` when there is nothing to undo.
    pub fn undo(&mut self) -> Result<bool, CommandError> {
        let Some(step) = self.history.take_undo() else { return Ok(false) };
        if let Err(e) = self.apply_checked(&step.undo) {
            self.history.restore_undo();
            return Err(e);
        }
        Ok(true)
    }

    /// Redoes the last undone step. `Ok(false)` when there is none.
    pub fn redo(&mut self) -> Result<bool, CommandError> {
        let Some(step) = self.history.take_redo() else { return Ok(false) };
        if let Err(e) = self.apply_checked(&step.redo) {
            self.history.restore_redo();
            return Err(e);
        }
        Ok(true)
    }

    /// Applies a command, validates and re-prepares the rig, and reverts on
    /// failure. Returns the inverse.
    fn apply_checked(&mut self, command: &Command) -> Result<Command, CommandError> {
        let inverse = command.apply(&mut self.document)?;
        match Rig::prepare(&self.document, &self.limits) {
            Ok(rig) => {
                self.replace_rig(rig);
                Ok(inverse)
            }
            Err(issues) => {
                // Reverting restores the state that was valid before.
                let _ = inverse.apply(&mut self.document);
                let text = issues.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
                Err(CommandError::Invalid(text))
            }
        }
    }

    /// Installs a new rig, keeping parameter values by id.
    fn replace_rig(&mut self, rig: Rig) {
        let mut params = ParamValues::defaults(&rig);
        for (p, v) in self.rig.params.iter().zip(&self.params.values) {
            params.set(&rig, &p.id, *v);
        }
        self.rig = rig;
        self.params = params;
        self.eval_stale = true;
    }

    // ---------- Parameters (view state: never dirty the document) ----------

    pub fn param_values(&self) -> &ParamValues {
        &self.params
    }

    /// Sets a parameter, clamped to its range. False for an unknown id or
    /// a non-finite value.
    pub fn set_param(&mut self, id: &str, value: [f32; 2]) -> bool {
        let Some(i) = self.rig.param_index(id) else { return false };
        let Some(p) = self.rig.params.get(i) else { return false };
        if !value.iter().all(|v| v.is_finite()) {
            return false;
        }
        let clamped = [value[0].clamp(p.min[0], p.max[0]), value[1].clamp(p.min[1], p.max[1])];
        let Some(slot) = self.params.values.get_mut(i) else { return false };
        if *slot != clamped {
            *slot = clamped;
            self.eval_stale = true;
        }
        true
    }

    pub fn reset_param(&mut self, id: &str) -> bool {
        match self.rig.param_index(id).and_then(|i| self.rig.params.get(i)) {
            Some(p) => {
                let default = p.default;
                self.set_param(id, default)
            }
            None => false,
        }
    }

    pub fn reset_params(&mut self) {
        self.params = ParamValues::defaults(&self.rig);
        self.eval_stale = true;
    }

    /// The model evaluated at the current parameter values (re-evaluated
    /// only after a change).
    pub fn evaluate(&mut self) -> &EvalOutput {
        if self.eval_stale {
            self.rig.evaluate(&self.params, &mut self.eval);
            self.eval_stale = false;
        }
        &self.eval
    }

    // ---------- Saving ----------

    /// Saves to the current save path.
    pub fn save(&mut self) -> Result<(), SaveError> {
        let path = self.save_path.clone().ok_or(SaveError::NoPath)?;
        self.save_as(&path)
    }

    /// Saves as a `.riglet` at `path` (atomically) and makes it the save
    /// path. Refuses other extensions and the file the model was imported
    /// from. Every package entry the editor doesn't own (another tool's
    /// `editor.json`, unknown files) is written back unchanged.
    pub fn save_as(&mut self, path: &Path) -> Result<(), SaveError> {
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("riglet")) {
            return Err(SaveError::NotRiglet);
        }
        if self.source.kind != SourceKind::Riglet && same_file(path, &self.source.path) {
            return Err(SaveError::WouldOverwriteImport(self.source.path.clone()));
        }
        let bytes = riglet_format::write_package_bytes(&self.document).map_err(SaveError::Encode)?;
        write_atomic(path, &bytes).map_err(|source| SaveError::Io { path: path.to_owned(), source })?;
        self.history.mark_saved();
        self.save_path = Some(path.to_owned());
        Ok(())
    }
}

/// Whether two paths name the same existing file (false if either is
/// missing).
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
