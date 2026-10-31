//! Undo and redo stacks of applied commands, with coalescing, a memory cap
//! and the saved-state marker behind dirty tracking.

use crate::command::{Command, Merge};

/// Caps for the undo history; the oldest steps are dropped first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimits {
    pub max_steps: usize,
    pub max_bytes: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        HistoryLimits { max_steps: 1000, max_bytes: 64 << 20 }
    }
}

/// One undo step: the command as applied and its inverse.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Step {
    /// Unique per step (and new when a step absorbs a coalesced edit), so
    /// "the document is in the saved state" is "the top step's id is the
    /// one recorded at save time".
    pub(crate) id: u64,
    pub(crate) redo: Command,
    pub(crate) undo: Command,
    /// The gesture that made this step, if any (see [`Merge`]).
    pub(crate) gesture: Option<u64>,
}

impl Step {
    fn bytes(&self) -> usize {
        self.redo.bytes() + self.undo.bytes()
    }
}

#[derive(Debug, Clone)]
pub struct History {
    limits: HistoryLimits,
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// Id of the state below the oldest kept step: 0 for the state the
    /// document was opened in, or the id of the last step dropped by the cap.
    base: u64,
    next_id: u64,
    /// Id of the state at the last save (`None`: never saved, for example
    /// an import that has no `.riglet` yet).
    saved: Option<u64>,
    /// Whether a command of the top step's gesture may still merge into it
    /// (false after an undo, a redo or a save).
    open: bool,
    bytes: usize,
}

impl History {
    /// `saved`: whether the opened state counts as saved (a `.riglet` read
    /// from disk) or not (an import or a recovered file).
    pub fn new(limits: HistoryLimits, saved: bool) -> Self {
        History {
            limits,
            undo: Vec::new(),
            redo: Vec::new(),
            base: 0,
            next_id: 1,
            saved: saved.then_some(0),
            open: false,
            bytes: 0,
        }
    }

    fn current(&self) -> u64 {
        self.undo.last().map_or(self.base, |s| s.id)
    }

    fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    /// True when the document differs from what was last saved.
    pub fn is_dirty(&self) -> bool {
        self.saved != Some(self.current())
    }

    pub fn mark_saved(&mut self) {
        self.saved = Some(self.current());
        self.open = false;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Records an applied command; merges it into the top step by the rule
    /// on [`Merge`].
    pub(crate) fn push(&mut self, redo: Command, undo: Command, merge: Merge) {
        for s in self.redo.drain(..) {
            self.bytes = self.bytes.saturating_sub(s.bytes());
        }
        let gesture = match merge {
            Merge::Never => None,
            Merge::Gesture(g) => Some(g.0),
        };
        let merges = gesture.is_some()
            && self.open
            && self.undo.last().is_some_and(|top| top.gesture == gesture && top.redo.target() == redo.target());
        let id = self.fresh_id();
        match self.undo.last_mut() {
            Some(top) if merges => {
                let before = top.bytes();
                top.redo = redo;
                top.id = id;
                self.bytes = self.bytes.saturating_sub(before).saturating_add(top.bytes());
            }
            _ => {
                let step = Step { id, redo, undo, gesture };
                self.bytes = self.bytes.saturating_add(step.bytes());
                self.undo.push(step);
            }
        }
        self.open = true;
        while self.undo.len() > self.limits.max_steps.max(1) || self.bytes > self.limits.max_bytes {
            if self.undo.len() <= 1 {
                break;
            }
            let dropped = self.undo.remove(0);
            self.bytes = self.bytes.saturating_sub(dropped.bytes());
            self.base = dropped.id;
        }
    }

    /// Approximate memory the history holds (see [`Command`]'s size).
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The step to undo, moved to the redo stack. The caller applies its
    /// `undo` command; if that fails it calls [`History::restore_undo`].
    pub(crate) fn take_undo(&mut self) -> Option<Step> {
        self.open = false;
        let step = self.undo.pop()?;
        self.redo.push(step.clone());
        Some(step)
    }

    pub(crate) fn restore_undo(&mut self) {
        if let Some(step) = self.redo.pop() {
            self.undo.push(step);
        }
    }

    pub(crate) fn take_redo(&mut self) -> Option<Step> {
        self.open = false;
        let step = self.redo.pop()?;
        self.undo.push(step.clone());
        Some(step)
    }

    pub(crate) fn restore_redo(&mut self) {
        if let Some(step) = self.undo.pop() {
            self.redo.push(step);
        }
    }
}
