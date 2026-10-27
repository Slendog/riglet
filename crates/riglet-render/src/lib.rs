//! GPU renderer for Riglet rigs (spec "Rendering", R1-R8).
//!
//! One code path for every consumer: the CLI renders offscreen and reads
//! back a PNG; the editor (M4) will render the same frames into its
//! viewport with the same pipelines and target format (R1).
//!
//! Untrusted input: every size is checked against the renderer budgets
//! ([`Budgets`], spec "Loader resource limits") and the adapter's limits
//! before anything is allocated on the GPU. GPU validation errors are
//! caught and returned as [`RenderError`], never panics.

pub mod image;

use riglet_format::model::{BlendMode, MaskMode};
use riglet_format::rig::{Rig, RigKind};
use riglet_format::{Document, EvalOutput, Member};

pub use crate::image::{Image, TextureData};

/// Target format for every color target (R1: never an sRGB format).
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no GPU adapter available: {0}")]
    NoAdapter(String),
    #[error("could not open the GPU device: {0}")]
    Device(String),
    #[error("texture `{id}`: {reason}")]
    Texture { id: String, reason: String },
    #[error("texture `{id}` is {width}x{height}, larger than this GPU's limit of {max} pixels")]
    TextureTooLarge { id: String, width: u32, height: u32, max: u32 },
    #[error("texture `{0}` is missing from the package")]
    MissingTexture(String),
    #[error("output size {width}x{height} is not allowed (limit {max}x{max}, minimum 1x1)")]
    OutputSize { width: u64, height: u64, max: u32 },
    #[error("output scale must be finite and positive")]
    BadScale,
    #[error("composites nest {depth} deep, the renderer allows {max}")]
    NestingTooDeep { depth: usize, max: usize },
    #[error("rendering needs {needed} bytes of offscreen memory, the budget is {budget}")]
    OffscreenBudget { needed: u64, budget: u64 },
    #[error("the evaluated frame does not match the uploaded model")]
    FrameMismatch,
    #[error("GPU error: {0}")]
    Gpu(String),
    #[error("could not encode the image: {0}")]
    Encode(String),
}

/// Renderer budgets (implementation-defined, spec "Loader resource
/// limits"). Checked before any GPU allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budgets {
    pub max_output_dimension: u32,
    pub max_composite_depth: usize,
    /// Conservative estimate: every target counts 4 bytes per pixel.
    pub max_offscreen_bytes: u64,
}

impl Default for Budgets {
    fn default() -> Self {
        Budgets { max_output_dimension: 8192, max_composite_depth: 16, max_offscreen_bytes: 1 << 30 }
    }
}

/// How to pick the GPU adapter.
#[derive(Debug, Clone, Default)]
pub struct RendererOptions {
    /// Prefer a software adapter (lavapipe, WARP, ...).
    pub force_software: bool,
    pub budgets: Budgets,
}

/// Facts about the adapter in use (for logs and CI reports).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterSummary {
    pub name: String,
    pub backend: String,
    pub device_type: String,
    pub driver: String,
    pub max_texture_dimension: u32,
}

/// Output settings for one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameOptions {
    /// Output scale `s` (R7).
    pub scale: f32,
    /// Opaque straight-RGB background to flatten onto (not part of the
    /// model, R4); `None` keeps transparency.
    pub background: Option<[u8; 3]>,
}

impl Default for FrameOptions {
    fn default() -> Self {
        FrameOptions { scale: 1.0, background: None }
    }
}

struct Pipelines {
    /// Index: blend mode * 2 + culling.
    drawable: Vec<wgpu::RenderPipeline>,
    /// Index: culling.
    mask: Vec<wgpu::RenderPipeline>,
    /// Index: blend mode.
    composite: Vec<wgpu::RenderPipeline>,
    globals_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    mask_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

/// A GPU device plus Riglet's pipelines. Create once, reuse for many
/// models and frames.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    summary: AdapterSummary,
    budgets: Budgets,
    pipelines: Pipelines,
    draw_stride: u64,
}

fn blend_index(mode: BlendMode) -> usize {
    match mode {
        BlendMode::Normal => 0,
        BlendMode::Additive => 1,
        BlendMode::Multiply => 2,
    }
}

/// R4, premultiplied.
fn blend_state(mode: BlendMode) -> wgpu::BlendState {
    use wgpu::{BlendComponent as C, BlendFactor as F, BlendOperation as O};
    let keep_alpha = C { src_factor: F::Zero, dst_factor: F::One, operation: O::Add };
    match mode {
        BlendMode::Normal => wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
        BlendMode::Additive => wgpu::BlendState {
            color: C { src_factor: F::One, dst_factor: F::One, operation: O::Add },
            alpha: keep_alpha,
        },
        BlendMode::Multiply => wgpu::BlendState {
            color: C { src_factor: F::Dst, dst_factor: F::OneMinusSrcAlpha, operation: O::Add },
            alpha: keep_alpha,
        },
    }
}

const BLENDS: [BlendMode; 3] = [BlendMode::Normal, BlendMode::Additive, BlendMode::Multiply];

impl Renderer {
    /// Opens a GPU device. Fails with [`RenderError::NoAdapter`] when the
    /// machine has none (callers decide whether that is fatal).
    pub fn new(options: RendererOptions) -> Result<Self, RenderError> {
        pollster::block_on(Self::new_async(options))
    }

    async fn new_async(options: RendererOptions) -> Result<Self, RenderError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                force_fallback_adapter: options.force_software,
                compatible_surface: None,
                ..Default::default()
            })
            .await
            .map_err(|e| RenderError::NoAdapter(e.to_string()))?;
        let info = adapter.get_info();
        let limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("riglet"),
                required_features: wgpu::Features::empty(),
                required_limits: limits.clone(),
                ..Default::default()
            })
            .await
            .map_err(|e| RenderError::Device(e.to_string()))?;
        // GPU errors become RenderError via error scopes; never panic.
        device.on_uncaptured_error(std::sync::Arc::new(|_| {}));
        let summary = AdapterSummary {
            name: info.name,
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: format!("{} {}", info.driver, info.driver_info).trim().to_owned(),
            max_texture_dimension: limits.max_texture_dimension_2d,
        };
        let pipelines = create_pipelines(&device);
        let align = u64::from(limits.min_uniform_buffer_offset_alignment).max(48);
        Ok(Renderer { device, queue, summary, budgets: options.budgets, pipelines, draw_stride: align })
    }

    pub fn adapter(&self) -> &AdapterSummary {
        &self.summary
    }

    pub fn budgets(&self) -> &Budgets {
        &self.budgets
    }

    /// Uploads a rig's textures and static mesh data.
    pub fn upload(&self, doc: &Document, rig: &Rig) -> Result<GpuModel, RenderError> {
        let max = self.summary.max_texture_dimension;
        let mut textures = Vec::with_capacity(rig.textures.len());
        for id in &rig.textures {
            let tex = doc
                .model
                .textures
                .iter()
                .find(|t| &t.id == id)
                .ok_or_else(|| RenderError::MissingTexture(id.clone()))?;
            let bytes = doc.files.get(&tex.file).ok_or_else(|| RenderError::MissingTexture(id.clone()))?;
            let data = image::decode_texture(id, bytes, tex.premultiplied_alpha, max)?;
            textures.push(self.texture_bind_group(&data, id));
        }

        // Static per-drawable data: UVs and indices into one shared buffer.
        let mut ranges = Vec::with_capacity(rig.drawables.len());
        let mut indices: Vec<u32> = Vec::new();
        let mut uvs = Vec::new();
        for &node in &rig.drawables {
            let Some(RigKind::Drawable { uvs: u, indices: idx, .. }) = rig.nodes.get(node).map(|n| &n.kind) else {
                return Err(RenderError::FrameMismatch);
            };
            let base = uvs.len() as u32;
            let first = indices.len() as u32;
            indices.extend(idx.iter().map(|&i| base + i));
            uvs.extend_from_slice(u);
            ranges.push(first..indices.len() as u32);
        }
        let vertex_bytes = (uvs.len().max(1) * 16) as u64;
        let index_data: Vec<u8> =
            if indices.is_empty() { vec![0; 4] } else { indices.iter().flat_map(|i| i.to_le_bytes()).collect() };
        let vertex_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("riglet vertices"),
            size: vertex_bytes,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let index_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("riglet indices"),
            size: index_data.len() as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&index_buffer, 0, &index_data);
        let draws = (rig.drawables.len() + rig.composites.len()).max(1) as u64;
        let uniform_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("riglet draws"),
            size: draws * self.draw_stride,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("riglet frame"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("riglet globals"),
            layout: &self.pipelines.globals_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: frame_buffer.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &uniform_buffer,
                        offset: 0,
                        size: wgpu::BufferSize::new(48),
                    }),
                },
            ],
        });
        let has_masks = rig.nodes.iter().any(|n| match &n.kind {
            RigKind::Drawable { masks, .. } | RigKind::Group { masks, .. } => !masks.is_empty(),
            _ => false,
        });
        Ok(GpuModel {
            textures,
            uvs,
            ranges,
            vertex_buffer,
            index_buffer,
            uniform_buffer,
            frame_buffer,
            globals,
            has_masks,
            depth: rig.composite_depth(),
            drawable_count: rig.drawables.len(),
            composite_count: rig.composites.len(),
        })
    }

    fn texture_bind_group(&self, data: &TextureData, id: &str) -> wgpu::BindGroup {
        let size = wgpu::Extent3d { width: data.width, height: data.height, depth_or_array_layers: 1 };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(id),
            size,
            mip_level_count: 1, // R2: no mipmaps
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT, // R1: never Rgba8UnormSrgb
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data.rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(data.width * 4), rows_per_image: None },
            size,
        );
        self.bind_texture(&texture.create_view(&wgpu::TextureViewDescriptor::default()))
    }

    fn bind_texture(&self, view: &wgpu::TextureView) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("riglet texture"),
            layout: &self.pipelines.texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.pipelines.sampler) },
            ],
        })
    }

    /// Output size for `scale` (R7), checked against the budgets.
    pub fn output_size(&self, rig_canvas: (f32, f32), scale: f32) -> Result<(u32, u32), RenderError> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(RenderError::BadScale);
        }
        let max = self.budgets.max_output_dimension.min(self.summary.max_texture_dimension);
        let w = (f64::from(rig_canvas.0) * f64::from(scale)).round();
        let h = (f64::from(rig_canvas.1) * f64::from(scale)).round();
        let bad = || RenderError::OutputSize { width: w.max(0.0) as u64, height: h.max(0.0) as u64, max };
        if !(1.0..=f64::from(max)).contains(&w) || !(1.0..=f64::from(max)).contains(&h) {
            return Err(bad());
        }
        Ok((w as u32, h as u32))
    }

    /// Offscreen memory a frame needs (R6 / budgets): the output, one layer
    /// per nesting depth (reused by sibling composites), and two mask
    /// buffers, each counted at 4 bytes per pixel.
    pub fn offscreen_bytes(width: u32, height: u32, depth: usize, has_masks: bool) -> u64 {
        let targets = 1 + depth as u64 + if has_masks { 2 } else { 0 };
        targets.saturating_mul(u64::from(width)).saturating_mul(u64::from(height)).saturating_mul(4)
    }

    /// Renders one evaluated frame and reads it back as a straight-alpha
    /// image (R8).
    pub fn render(
        &self,
        model: &GpuModel,
        rig: &Rig,
        canvas: (f32, f32),
        frame: &EvalOutput,
        options: &FrameOptions,
    ) -> Result<Image, RenderError> {
        if frame.drawables.len() != model.drawable_count || frame.composites.len() != model.composite_count {
            return Err(RenderError::FrameMismatch);
        }
        let (width, height) = self.output_size(canvas, options.scale)?;
        if model.depth > self.budgets.max_composite_depth {
            return Err(RenderError::NestingTooDeep { depth: model.depth, max: self.budgets.max_composite_depth });
        }
        let needed = Self::offscreen_bytes(width, height, model.depth, model.has_masks);
        if needed > self.budgets.max_offscreen_bytes {
            return Err(RenderError::OffscreenBudget { needed, budget: self.budgets.max_offscreen_bytes });
        }

        let validation_scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let oom_scope = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let result = self.render_inner(model, rig, frame, options, width, height);
        let oom = pollster::block_on(oom_scope.pop());
        let validation = pollster::block_on(validation_scope.pop());
        if let Some(e) = oom.or(validation) {
            return Err(RenderError::Gpu(e.to_string()));
        }
        result
    }

    fn render_inner(
        &self,
        model: &GpuModel,
        rig: &Rig,
        frame: &EvalOutput,
        options: &FrameOptions,
        width: u32,
        height: u32,
    ) -> Result<Image, RenderError> {
        // Per-frame data: vertices (current positions + static UVs) and
        // one draw uniform per drawable and composite.
        // Interleaved per vertex: pos.x, pos.y, u, v.
        let mut interleaved = Vec::with_capacity(model.uvs.len() * 16);
        let mut uv_iter = model.uvs.iter();
        for d in &frame.drawables {
            for p in &d.positions {
                let uv = uv_iter.next().copied().unwrap_or([0.0, 0.0]);
                for v in [p[0], p[1], uv[0], uv[1]] {
                    interleaved.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        if !interleaved.is_empty() {
            self.queue.write_buffer(&model.vertex_buffer, 0, &interleaved);
        }

        let mut uniforms =
            vec![0u8; ((model.drawable_count + model.composite_count).max(1) as u64 * self.draw_stride) as usize];
        let mut put = |slot: usize, values: [f32; 12]| {
            let at = slot * self.draw_stride as usize;
            if let Some(dst) = uniforms.get_mut(at..at + 48) {
                for (chunk, v) in dst.as_chunks_mut::<4>().0.iter_mut().zip(values) {
                    *chunk = v.to_le_bytes();
                }
            }
        };
        let flags = |masks: &[(usize, MaskMode)]| {
            let clip = masks.iter().any(|(_, m)| *m == MaskMode::Clip);
            let invert = masks.iter().any(|(_, m)| *m == MaskMode::Invert);
            (if clip { 1.0 } else { 0.0 }, if invert { 1.0 } else { 0.0 })
        };
        for (slot, d) in frame.drawables.iter().enumerate() {
            let masks = node_masks(rig, d.node);
            let (clip, invert) = flags(masks);
            let [mr, mg, mb] = d.multiply_color;
            let [sr, sg, sb] = d.screen_color;
            put(slot, [mr, mg, mb, 1.0, sr, sg, sb, 0.0, d.opacity, clip, invert, 0.0]);
        }
        for (slot, c) in frame.composites.iter().enumerate() {
            let (clip, invert) = flags(node_masks(rig, c.node));
            put(model.drawable_count + slot, [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, c.opacity, clip, invert, 0.0]);
        }
        self.queue.write_buffer(&model.uniform_buffer, 0, &uniforms);
        let frame_uniform: Vec<u8> =
            [width as f32, height as f32, options.scale, 0.0].iter().flat_map(|v| v.to_le_bytes()).collect();
        self.queue.write_buffer(&model.frame_buffer, 0, &frame_uniform);

        // Targets: output, one layer per depth, two mask buffers.
        let color = |label: &str, extra: wgpu::TextureUsages| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: COLOR_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | extra,
                view_formats: &[],
            })
        };
        let output = color("riglet output", wgpu::TextureUsages::COPY_SRC);
        let layers: Vec<wgpu::Texture> =
            (0..model.depth).map(|_| color("riglet layer", wgpu::TextureUsages::TEXTURE_BINDING)).collect();
        let mask_size = if model.has_masks { (width, height) } else { (1, 1) };
        let mask_texture = |label: &str, size: (u32, u32)| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: MASK_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let clip = mask_texture("riglet clip mask", mask_size);
        let invert = mask_texture("riglet invert mask", mask_size);
        // Mask passes render INTO the mask buffers, so they bind 1x1
        // dummies in their place (a texture can't be read and written in
        // one pass).
        let dummy_a = mask_texture("riglet dummy mask", (1, 1));
        let dummy_b = mask_texture("riglet dummy mask", (1, 1));
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let (clip_view, invert_view) = (view(&clip), view(&invert));
        let mask_group_for = |a: &wgpu::TextureView, b: &wgpu::TextureView| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("riglet masks"),
                layout: &self.pipelines.mask_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(a) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(b) },
                ],
            })
        };
        let mask_group = mask_group_for(&clip_view, &invert_view);
        let dummy_group = mask_group_for(&view(&dummy_a), &view(&dummy_b));
        let layer_views: Vec<wgpu::TextureView> = layers.iter().map(view).collect();
        let layer_groups: Vec<wgpu::BindGroup> = layer_views.iter().map(|v| self.bind_texture(v)).collect();

        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("riglet frame") });
        let ctx = FrameContext {
            renderer: self,
            model,
            rig,
            frame,
            layer_views: &layer_views,
            layer_groups: &layer_groups,
            clip_view: &clip_view,
            invert_view: &invert_view,
            mask_group: &mask_group,
            dummy_group: &dummy_group,
        };
        ctx.render_scope(&mut encoder, &view(&output), &frame.members, 0);

        // Readback with rows padded to wgpu's 256-byte alignment.
        let unpadded = width as usize * 4;
        let padded = unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("riglet readback"),
            size: (padded * height as usize) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded as u32),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);

        let (tx, rx) = std::sync::mpsc::channel();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .map_err(|e| RenderError::Gpu(e.to_string()))?;
        rx.recv().map_err(|e| RenderError::Gpu(e.to_string()))?.map_err(|e| RenderError::Gpu(e.to_string()))?;
        let bytes = {
            let mapped = readback.slice(..).get_mapped_range().map_err(|e| RenderError::Gpu(e.to_string()))?;
            image::unpad_rows(&mapped, width, height, padded)
                .ok_or_else(|| RenderError::Gpu("short readback".into()))?
        };
        readback.unmap();
        Ok(Image::from_premultiplied(width, height, bytes, options.background))
    }
}

fn node_masks(rig: &Rig, node: usize) -> &[(usize, MaskMode)] {
    match rig.nodes.get(node).map(|n| &n.kind) {
        Some(RigKind::Drawable { masks, .. } | RigKind::Group { masks, .. }) => masks,
        _ => &[],
    }
}

/// Static GPU data for one rig; reuse it for every frame of that rig.
pub struct GpuModel {
    textures: Vec<wgpu::BindGroup>,
    uvs: Vec<[f32; 2]>,
    /// Index range per drawable slot.
    ranges: Vec<std::ops::Range<u32>>,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    uniform_buffer: wgpu::Buffer,
    frame_buffer: wgpu::Buffer,
    globals: wgpu::BindGroup,
    has_masks: bool,
    depth: usize,
    drawable_count: usize,
    composite_count: usize,
}

impl GpuModel {
    /// Deepest composite nesting (number of offscreen layers a frame needs).
    pub fn composite_depth(&self) -> usize {
        self.depth
    }

    pub fn has_masks(&self) -> bool {
        self.has_masks
    }
}

struct FrameContext<'a> {
    renderer: &'a Renderer,
    model: &'a GpuModel,
    rig: &'a Rig,
    frame: &'a EvalOutput,
    layer_views: &'a [wgpu::TextureView],
    layer_groups: &'a [wgpu::BindGroup],
    clip_view: &'a wgpu::TextureView,
    invert_view: &'a wgpu::TextureView,
    mask_group: &'a wgpu::BindGroup,
    dummy_group: &'a wgpu::BindGroup,
}

fn color_pass<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    target: &'e wgpu::TextureView,
    clear: bool,
) -> wgpu::RenderPass<'e> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("riglet pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: if clear { wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT) } else { wgpu::LoadOp::Load },
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    })
}

impl FrameContext<'_> {
    fn texture_of(&self, node: usize) -> Option<(&wgpu::BindGroup, bool)> {
        match self.rig.nodes.get(node).map(|n| &n.kind) {
            Some(RigKind::Drawable { texture, culling, .. }) => {
                self.model.textures.get(*texture).map(|t| (t, *culling))
            }
            _ => None,
        }
    }

    fn draw_offset(&self, slot: usize) -> u32 {
        (slot as u64 * self.renderer.draw_stride) as u32
    }

    /// Draws one scope (E4) into `target`: cleared first, members in order.
    fn render_scope(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        members: &[Member],
        depth: usize,
    ) {
        drop(color_pass(encoder, target, true));
        for member in members {
            match *member {
                Member::Drawable(slot) => self.draw_drawable(encoder, target, slot),
                Member::Composite(slot) => self.draw_composite(encoder, target, slot, depth),
            }
        }
    }

    fn draw_drawable(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, slot: usize) {
        let Some(d) = self.frame.drawables.get(slot) else { return };
        let Some(range) = self.model.ranges.get(slot).cloned() else { return };
        if !d.visible || range.is_empty() {
            return;
        }
        let Some((texture, culling)) = self.texture_of(d.node) else { return };
        let blend = match self.rig.nodes.get(d.node).map(|n| &n.kind) {
            Some(RigKind::Drawable { blend_mode, .. }) => *blend_mode,
            _ => BlendMode::Normal,
        };
        self.render_masks(encoder, node_masks(self.rig, d.node));
        let p = &self.renderer.pipelines;
        let Some(pipeline) = p.drawable.get(blend_index(blend) * 2 + usize::from(culling)) else { return };
        let mut pass = color_pass(encoder, target, false);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.model.globals, &[self.draw_offset(slot)]);
        pass.set_bind_group(1, texture, &[]);
        pass.set_bind_group(2, self.mask_group, &[]);
        pass.set_vertex_buffer(0, self.model.vertex_buffer.slice(..));
        pass.set_index_buffer(self.model.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(range, 0, 0..1);
    }

    fn draw_composite(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        slot: usize,
        depth: usize,
    ) {
        let Some(c) = self.frame.composites.get(slot) else { return };
        let (Some(layer), Some(layer_group)) = (self.layer_views.get(depth), self.layer_groups.get(depth)) else {
            return;
        };
        if !c.visible {
            return;
        }
        self.render_scope(encoder, layer, &c.members, depth + 1);
        let blend = match self.rig.nodes.get(c.node).map(|n| &n.kind) {
            Some(RigKind::Group { blend_mode, .. }) => *blend_mode,
            _ => BlendMode::Normal,
        };
        self.render_masks(encoder, node_masks(self.rig, c.node));
        let Some(pipeline) = self.renderer.pipelines.composite.get(blend_index(blend)) else { return };
        let mut pass = color_pass(encoder, target, false);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.model.globals, &[self.draw_offset(self.model.drawable_count + slot)]);
        pass.set_bind_group(1, layer_group, &[]);
        pass.set_bind_group(2, self.mask_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// R5: rasterizes clip and invert coverage into the two mask buffers.
    fn render_masks(&self, encoder: &mut wgpu::CommandEncoder, masks: &[(usize, MaskMode)]) {
        if masks.is_empty() {
            return;
        }
        for (mode, view) in [(MaskMode::Clip, self.clip_view), (MaskMode::Invert, self.invert_view)] {
            if !masks.iter().any(|(_, m)| *m == mode) {
                continue;
            }
            let mut pass = color_pass(encoder, view, true);
            pass.set_bind_group(0, &self.model.globals, &[0]);
            pass.set_bind_group(2, self.dummy_group, &[]);
            pass.set_vertex_buffer(0, self.model.vertex_buffer.slice(..));
            pass.set_index_buffer(self.model.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            for &(source, _) in masks.iter().filter(|(_, m)| *m == mode) {
                let Some(Member::Drawable(slot)) = self.rig.slot_of(source) else { continue };
                let Some(range) = self.model.ranges.get(slot).cloned() else { continue };
                let Some((texture, culling)) = self.texture_of(source) else { continue };
                let Some(pipeline) = self.renderer.pipelines.mask.get(usize::from(culling)) else { continue };
                pass.set_pipeline(pipeline);
                pass.set_bind_group(1, texture, &[]);
                pass.draw_indexed(range, 0, 0..1);
            }
        }
    }
}

fn create_pipelines(device: &wgpu::Device) -> Pipelines {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("riglet shaders"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders.wgsl").into()),
    });
    let uniform = |binding, dynamic| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: dynamic,
            min_binding_size: None,
        },
        count: None,
    };
    let texture = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("riglet globals"),
        entries: &[uniform(0, false), uniform(1, true)],
    });
    let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("riglet texture"),
        entries: &[
            texture(0),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let mask_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("riglet masks"),
        entries: &[texture(0), texture(1)],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("riglet"),
        bind_group_layouts: &[Some(&globals_layout), Some(&texture_layout), Some(&mask_layout)],
        ..Default::default()
    });
    let vertex_layout = wgpu::VertexBufferLayout {
        array_stride: 16,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
    };
    let pipeline = |label: &str, vs: &str, fs: &str, mesh: bool, format, blend, culling: bool| {
        let mesh_buffers = [Some(vertex_layout.clone())];
        let buffers: &[Option<wgpu::VertexBufferLayout<'_>>] = if mesh { &mesh_buffers } else { &[] };
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some(vs),
                buffers,
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // Clockwise on screen (+Y down) is counter-clockwise in NDC
                // (+Y up): front facing (R7).
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: if culling { Some(wgpu::Face::Back) } else { None },
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(), // R7: no AA
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(fs),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        })
    };
    let union = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrc,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent::OVER,
    };
    let drawable = BLENDS
        .iter()
        .flat_map(|&mode| [false, true].map(|cull| (mode, cull)))
        .map(|(mode, cull)| {
            pipeline("riglet drawable", "vs_mesh", "fs_drawable", true, COLOR_FORMAT, blend_state(mode), cull)
        })
        .collect();
    let mask =
        [false, true].map(|cull| pipeline("riglet mask", "vs_mesh", "fs_mask", true, MASK_FORMAT, union, cull)).into();
    let composite = BLENDS
        .iter()
        .map(|&mode| {
            pipeline("riglet composite", "vs_fullscreen", "fs_composite", false, COLOR_FORMAT, blend_state(mode), false)
        })
        .collect();
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("riglet sampler"),
        // R2: bilinear, clamp to edge, no mipmaps.
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    Pipelines { drawable, mask, composite, globals_layout, texture_layout, mask_layout, sampler }
}

/// Uploads `doc`'s rig and renders one evaluated frame: the common CLI path.
pub fn render_document(
    renderer: &Renderer,
    doc: &Document,
    rig: &Rig,
    frame: &EvalOutput,
    options: &FrameOptions,
) -> Result<Image, RenderError> {
    let model = renderer.upload(doc, rig)?;
    let canvas = (doc.model.canvas.width, doc.model.canvas.height);
    renderer.render(&model, rig, canvas, frame, options)
}
