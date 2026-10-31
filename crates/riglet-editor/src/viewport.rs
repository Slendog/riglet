//! The editor's viewport rendering, without UI types: the evaluated model
//! rendered by `riglet-render` into a texture the UI shows directly (no
//! readback). The UI registers [`Viewport::target`] with egui.

use riglet_render::{FrameOptions, GpuModel, RenderError, Renderer, View};

use crate::state::{EditorState, ViewState};

/// Renders an [`EditorState`] into a reusable target texture.
pub struct Viewport {
    renderer: Renderer,
    gpu: Option<(u64, GpuModel)>,
    target: Option<wgpu::Texture>,
    /// Bumped whenever `target` is replaced, so the UI re-registers it.
    target_generation: u64,
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
        Viewport { renderer, gpu: None, target: None, target_generation: 0 }
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
        let (rig, frame) = state.frame();
        self.renderer.render_to(gpu, rig, canvas, frame, &options, target)?;
        Ok(target)
    }
}
