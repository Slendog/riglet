//! Riglet editor core: the open document, commands with undo/redo, dirty
//! tracking and atomic saving. No UI types here, so all of it is tested
//! without a window. Design: `docs/design/m4-editor.md`.

pub mod command;
pub mod history;
pub mod save;
pub mod state;

pub use command::{Command, CommandError, Gesture, MAX_NAME_BYTES, Merge};
pub use history::{History, HistoryLimits};
pub use save::write_atomic;
pub use state::{EditorState, LoadError, Loaded, SaveError, Source, SourceKind, ViewState, load};
