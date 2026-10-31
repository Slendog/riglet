//! Riglet editor core: the open document, commands with undo/redo, dirty
//! tracking, atomic saving and viewport rendering. No UI types here, so all
//! of it is tested without a window; the egui app is in `main.rs`. Design: `docs/design/m4-editor.md`.

pub mod command;
pub mod history;
pub mod loader;
pub mod save;
pub mod state;
pub mod viewport;

pub use command::{Command, CommandError, Gesture, MAX_NAME_BYTES, Merge};
pub use history::{History, HistoryLimits};
pub use loader::{LoadFn, Loader};
pub use save::write_atomic;
pub use state::{EditorState, LoadError, Loaded, MAX_ZOOM, MIN_ZOOM, SaveError, Source, SourceKind, ViewState, load};
pub use viewport::{Viewport, frame_options};
