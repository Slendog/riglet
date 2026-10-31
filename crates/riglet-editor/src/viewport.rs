//! The editor's viewport rendering, without UI types: the evaluated model
//! rendered by `riglet-render` into a texture the UI shows directly (no
//! readback). The UI registers [`Viewport::target`] with egui.

use std::collections::BTreeSet;

use riglet_format::model::Id;
use riglet_format::{EvalOutput, Rig};
use riglet_render::{FrameOptions, GpuModel, RenderError, Renderer, View};

use crate::state::{EditorState, ViewState};

/// Renders an [`EditorState`] into a reusable target texture.
pub struct Viewport {
    renderer: Renderer,
    gpu: Option<(u64, GpuModel)>,
    target: Option<wgpu::Texture>,
    /// Bumped whenever `target` is replaced, so the UI re-registers it.
    target_generation: u64,
    /// The frame with hide-in-view applied (reused buffer).
    shown_frame: EvalOutput,
}

/// Applies the view-only hide toggles to a copy of `frame`: drawables and
/// composites under a hidden node get `visible = false`, exactly as if the
/// model hid them (so a hidden mask source still masks, as with model
/// visibility). The document and the evaluation are not touched.
pub fn hide_in_view(rig: &Rig, frame: &EvalOutput, hidden: &BTreeSet<Id>, out: &mut EvalOutput) {
    out.clone_from(frame);
    if hidden.is_empty() {
        return;
    }
    // Pre-order: a parent's flag is known before its children.
    let mut is_hidden = vec![false; rig.nodes.len()];
    for (i, n) in rig.nodes.iter().enumerate() {
        let parent = n.parent.and_then(|p| is_hidden.get(p).copied()).unwrap_or(false);
        if let Some(h) = is_hidden.get_mut(i) {
            *h = parent || hidden.contains(&n.id);
        }
    }
    let hid = |node: usize| is_hidden.get(node).copied().unwrap_or(false);
    for d in &mut out.drawables {
        if hid(d.node) {
            d.visible = false;
        }
    }
    for c in &mut out.composites {
        if hid(c.node) {
            c.visible = false;
        }
    }
}

/// The frame options for a viewport of `width` x `height` physical pixels:
/// `ViewState::zoom` is output pixels per model unit, the pan moves model
/// (0, 0) from the viewport center. No background: the UI draws the
/// checkerboard or solid color behind the (premultiplied) texture.
pub fn frame_options(view: &ViewState, width: u32, height: u32) -> FrameOptions {
    FrameOptions { scale: view.zoom, background: None, view: Some(View { width, height, pan: view.pan }) }
}

impl Viewport {
    pub fn new(renderer: Renderer) -> Self {
        Viewport { renderer, gpu: None, target: None, target_generation: 0, shown_frame: EvalOutput::new() }
    }

    pub fn renderer(&self) -> &Renderer {
        &self.renderer
    }

    /// The last rendered target, if any.
    pub fn target(&self) -> Option<&wgpu::Texture> {
        self.target.as_ref()
    }

    pub fn target_generation(&self) -> u64 {
        self.target_generation
    }

    /// Drops the uploaded model (for example when a document is closed).
    pub fn clear(&mut self) {
        self.gpu = None;
    }

    /// Renders the state's current frame into the target, `width` x
    /// `height` physical pixels. Uploads the model again only when the
    /// state's GPU data changed (a new document, or a command that changes
    /// meshes or textures), and reallocates the target only on a size
    /// change.
    pub fn render(&mut self, state: &mut EditorState, width: u32, height: u32) -> Result<&wgpu::Texture, RenderError> {
        let options = frame_options(&state.view, width, height);
        let (w, h) = self.renderer.frame_size((0.0, 0.0), &options)?;
        let generation = state.gpu_generation();
        if self.gpu.as_ref().is_none_or(|(g, _)| *g != generation) {
            self.gpu = None;
            let gpu = self.renderer.upload(state.document(), state.rig())?;
            self.gpu = Some((generation, gpu));
        }
        if self.target.as_ref().is_none_or(|t| (t.width(), t.height()) != (w, h)) {
            self.target = Some(self.renderer.create_target(w, h));
            self.target_generation = self.target_generation.wrapping_add(1);
        }
        let (Some((_, gpu)), Some(target)) = (self.gpu.as_ref(), self.target.as_ref()) else {
            return Err(RenderError::Target("no viewport target".into()));
        };
        let canvas = state.canvas();
        let hidden = state.view.hidden.clone();
        let (rig, frame) = state.frame();
        hide_in_view(rig, frame, &hidden, &mut self.shown_frame);
        self.renderer.render_to(gpu, rig, canvas, &self.shown_frame, &options, target)?;
        Ok(target)
    }
}

/// The viewport's area snapped to whole physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelRect {
    /// Top-left corner in physical pixels (whole numbers).
    pub origin: [f32; 2],
    /// Size in physical pixels: the render target's size.
    pub width: u32,
    pub height: u32,
    /// The same rectangle in egui points, for drawing the texture.
    pub min_points: [f32; 2],
    pub max_points: [f32; 2],
}

/// Snaps a rectangle given in points (`min`, `max`) to physical pixels at
/// `pixels_per_point` (1.25, 1.5, ... on scaled displays). Drawing the
/// target over `min_points..max_points` maps each texel onto exactly one
/// screen pixel, so nearest sampling shows the renderer's bytes unchanged
/// instead of blurring or skipping a row at fractional scale factors.
pub fn pixel_rect(min: [f32; 2], max: [f32; 2], pixels_per_point: f32) -> PixelRect {
    let ppp = if pixels_per_point.is_finite() && pixels_per_point > 0.0 { pixels_per_point } else { 1.0 };
    let snap = |v: f32| (v * ppp).round();
    let (x0, y0, x1, y1) = (snap(min[0]), snap(min[1]), snap(max[0]), snap(max[1]));
    let width = (x1 - x0).max(0.0) as u32;
    let height = (y1 - y0).max(0.0) as u32;
    PixelRect { origin: [x0, y0], width, height, min_points: [x0 / ppp, y0 / ppp], max_points: [x1 / ppp, y1 / ppp] }
}

/// Most outline edges drawn for one selection (a whole-model selection of
/// a large model is drawn up to this many).
pub const MAX_OUTLINE_EDGES: usize = 200_000;

/// The evaluated mesh edges (model space) of every drawable at or under
/// `node`, each shared edge once, skipping edges with a non-finite end
/// (E10), at most [`MAX_OUTLINE_EDGES`].
pub fn outline_edges(rig: &Rig, frame: &EvalOutput, node: &str) -> Vec<[[f32; 2]; 2]> {
    let Some(top) = rig.node_index(node) else { return Vec::new() };
    // Pre-order: descendants follow their ancestor.
    let mut under = vec![false; rig.nodes.len()];
    for (i, n) in rig.nodes.iter().enumerate() {
        let parent = n.parent.and_then(|p| under.get(p).copied()).unwrap_or(false);
        if let Some(u) = under.get_mut(i) {
            *u = i == top || parent;
        }
    }
    let mut edges = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for d in &frame.drawables {
        if !under.get(d.node).copied().unwrap_or(false) {
            continue;
        }
        let Some(riglet_format::rig::RigKind::Drawable { indices, .. }) = rig.nodes.get(d.node).map(|n| &n.kind) else {
            continue;
        };
        seen.clear();
        for &[i, j, k] in indices.as_chunks::<3>().0 {
            for (a, b) in [(i, j), (j, k), (k, i)] {
                if !seen.insert((a.min(b), a.max(b))) {
                    continue;
                }
                let (Some(p), Some(q)) = (d.positions.get(a as usize), d.positions.get(b as usize)) else { continue };
                if p.iter().chain(q).all(|v| v.is_finite()) {
                    edges.push([*p, *q]);
                    if edges.len() >= MAX_OUTLINE_EDGES {
                        return edges;
                    }
                }
            }
        }
    }
    edges
}
