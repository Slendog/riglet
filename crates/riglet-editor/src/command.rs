//! Document commands: every change the editor makes to a [`Document`] is a
//! [`Command`]. Applying one returns its inverse, which is what undo applies
//! (design `docs/design/m4-editor.md`, section 5).

use riglet_format::Document;
use riglet_format::model::{Id, ParamKind};

/// Longest node or parameter name a command accepts, in bytes. Names come
/// from the user here, but the cap keeps the history's memory bounded.
pub const MAX_NAME_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    RenameNode {
        node: Id,
        name: String,
    },
    /// The model's `visible` flag (saved), not the view-only hide toggle.
    SetNodeVisible {
        node: Id,
        visible: bool,
    },
    RenameParameter {
        parameter: Id,
        name: String,
    },
    SetParameterGroup {
        parameter: Id,
        group: Option<String>,
    },
    /// One value per component (1 or 2), inside the parameter's range.
    SetParameterDefault {
        parameter: Id,
        default: Vec<f32>,
    },
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("there is no node `{0}`")]
    NoNode(Id),
    #[error("there is no parameter `{0}`")]
    NoParameter(Id),
    #[error("names are limited to {MAX_NAME_BYTES} bytes")]
    NameTooLong,
    #[error("parameter `{0}` needs one value per component, each finite and inside its range")]
    BadDefault(Id),
    #[error("the change would make the model invalid: {0}")]
    Invalid(String),
}

/// One continuous edit by the user: a slider or value drag, from press to
/// release, or typing in one text field, from focus to unfocus. Get one
/// from [`crate::EditorState::begin_gesture`] when the edit starts and pass
/// it with every command of that edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gesture(pub(crate) u64);

/// How an applied command joins the undo history.
///
/// Merge rule: a command with `Merge::Gesture(g)` merges into the newest
/// undo step when that step was made in the same gesture `g`, edits the
/// same field (same command kind and target), and nothing ended the
/// gesture's step in between (an undo, a redo or a save). The merged step
/// keeps the state before the gesture's first command as its undo, and the
/// last command as its redo, so one undo reverts the whole drag. Anything
/// else starts a new step. Commands of different gestures never merge,
/// even on the same field, so two drags of one slider are two steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merge {
    /// A step of its own (menu actions, toggles).
    Never,
    /// Part of a continuous edit.
    Gesture(Gesture),
}

/// What a command changes, for coalescing consecutive edits of the same
/// field into one undo step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    NodeName(Id),
    NodeVisible(Id),
    ParameterName(Id),
    ParameterGroup(Id),
    ParameterDefault(Id),
}

impl Command {
    pub(crate) fn target(&self) -> Target {
        match self {
            Command::RenameNode { node, .. } => Target::NodeName(node.clone()),
            Command::SetNodeVisible { node, .. } => Target::NodeVisible(node.clone()),
            Command::RenameParameter { parameter, .. } => Target::ParameterName(parameter.clone()),
            Command::SetParameterGroup { parameter, .. } => Target::ParameterGroup(parameter.clone()),
            Command::SetParameterDefault { parameter, .. } => Target::ParameterDefault(parameter.clone()),
        }
    }

    /// Whether applying the command changes data uploaded to the GPU
    /// (meshes, UVs, indices, textures, the drawable set), so the viewport
    /// must upload the model again. None of M4a's commands do; M4b's mesh
    /// edits will.
    pub fn changes_gpu_data(&self) -> bool {
        match self {
            Command::RenameNode { .. }
            | Command::SetNodeVisible { .. }
            | Command::RenameParameter { .. }
            | Command::SetParameterGroup { .. }
            | Command::SetParameterDefault { .. } => false,
        }
    }

    /// Approximate memory a stored command holds, for the history cap:
    /// the enum plus every heap allocation it owns. Commands that carry mesh
    /// or texture data (M4b) must count those buffers here.
    pub(crate) fn bytes(&self) -> usize {
        let text = match self {
            Command::RenameNode { node, name } => node.len() + name.len(),
            Command::SetNodeVisible { node, .. } => node.len(),
            Command::RenameParameter { parameter, name } => parameter.len() + name.len(),
            Command::SetParameterGroup { parameter, group } => parameter.len() + group.as_ref().map_or(0, String::len),
            Command::SetParameterDefault { parameter, default } => parameter.len() + default.len() * 4,
        };
        std::mem::size_of::<Command>() + text
    }

    /// Applies the command to `doc` and returns its inverse. On an error the
    /// document is unchanged. This checks only the command's own
    /// preconditions; the caller validates the resulting model.
    pub fn apply(&self, doc: &mut Document) -> Result<Command, CommandError> {
        let model = &mut doc.model;
        match self {
            Command::RenameNode { node, name } => {
                check_name(name)?;
                let n = model.nodes.get_mut(node).ok_or_else(|| CommandError::NoNode(node.clone()))?;
                let old = std::mem::replace(&mut n.base_mut().name, name.clone());
                Ok(Command::RenameNode { node: node.clone(), name: old })
            }
            Command::SetNodeVisible { node, visible } => {
                let n = model.nodes.get_mut(node).ok_or_else(|| CommandError::NoNode(node.clone()))?;
                let old = std::mem::replace(&mut n.base_mut().visible, *visible);
                Ok(Command::SetNodeVisible { node: node.clone(), visible: old })
            }
            Command::RenameParameter { parameter, name } => {
                check_name(name)?;
                let p = model
                    .parameters
                    .iter_mut()
                    .find(|p| p.id == *parameter)
                    .ok_or_else(|| CommandError::NoParameter(parameter.clone()))?;
                let old = std::mem::replace(&mut p.name, name.clone());
                Ok(Command::RenameParameter { parameter: parameter.clone(), name: old })
            }
            Command::SetParameterGroup { parameter, group } => {
                if let Some(g) = group {
                    check_name(g)?;
                }
                let p = model
                    .parameters
                    .iter_mut()
                    .find(|p| p.id == *parameter)
                    .ok_or_else(|| CommandError::NoParameter(parameter.clone()))?;
                let old = std::mem::replace(&mut p.group, group.clone());
                Ok(Command::SetParameterGroup { parameter: parameter.clone(), group: old })
            }
            Command::SetParameterDefault { parameter, default } => {
                let p = model
                    .parameters
                    .iter_mut()
                    .find(|p| p.id == *parameter)
                    .ok_or_else(|| CommandError::NoParameter(parameter.clone()))?;
                let components = match p.kind {
                    ParamKind::OneD => 1,
                    ParamKind::TwoD => 2,
                };
                let in_range = default.len() == components
                    && default.iter().enumerate().all(|(i, v)| {
                        let (Some(lo), Some(hi)) = (p.min.get(i), p.max.get(i)) else { return false };
                        v.is_finite() && lo <= v && v <= hi
                    });
                if !in_range {
                    return Err(CommandError::BadDefault(parameter.clone()));
                }
                let old = std::mem::replace(&mut p.default, default.clone());
                Ok(Command::SetParameterDefault { parameter: parameter.clone(), default: old })
            }
        }
    }
}

fn check_name(name: &str) -> Result<(), CommandError> {
    if name.len() > MAX_NAME_BYTES { Err(CommandError::NameTooLong) } else { Ok(()) }
}
