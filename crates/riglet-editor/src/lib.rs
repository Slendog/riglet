//! Riglet editor core: the open document, commands with undo/redo, dirty
//! tracking, atomic saving, autosave and crash recovery, and viewport rendering. No UI types here, so all
//! of it is tested without a window; the egui app is in `main.rs`. Design: `docs/design/m4-editor.md`.

pub mod command;
pub mod history;
pub mod loader;
pub mod recovery;
pub mod save;
pub mod state;
pub mod viewport;
pub mod warnings;

pub use command::{Command, CommandError, Gesture, MAX_NAME_BYTES, Merge};
pub use history::{History, HistoryLimits};
pub use loader::{LoadFn, Loader};
pub use recovery::{Autosave, AutosaveStatus, Orphan, Record, Session};
pub use save::write_atomic;
pub use state::{
    EditorState, ExportError, ExportRecord, LoadError, Loaded, MAX_ZOOM, MIN_ZOOM, SaveError, Source, SourceKind,
    ViewState, load,
};
pub use viewport::{Viewport, frame_options};
