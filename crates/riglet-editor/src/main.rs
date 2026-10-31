//! Riglet editor (M4a): window, open (background load, native dialog, drag
//! and drop), viewport with pan, zoom and fit, save and undo. Design:
//! `docs/design/m4-editor.md`. All model logic is in the library
//! (`EditorState`, `Viewport`); this file is the egui layer.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod panels;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, Pos2, Rect, Sense, Vec2};
use eframe::{egui_wgpu, wgpu};
use riglet_editor::viewport::{outline_edges, pixel_rect};
use riglet_editor::{EditorState, ExportError, LoadError, Loader, SaveError, Viewport};
use riglet_format::Limits;
use riglet_render::{Budgets, Renderer};

const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const EXPORT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::E);
const SAVE_AS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);
const UNDO: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
const REDO: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::Z);
const REDO_Y: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Y);
const FIT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F);
const ACTUAL_SIZE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Num1);

fn main() -> eframe::Result {
    // The argument is untrusted like any path: it only goes to `load`,
    // which applies the usual limits and validation.
    let initial = std::env::args_os().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Riglet")
            .with_inner_size([1280.0, 800.0])
            .with_drag_and_drop(true),
        renderer: eframe::Renderer::Wgpu,
        // Dithering adds noise to every displayed value; the viewport must
        // show the renderer's bytes unchanged (color check).
        dithering: false,
        wgpu_options: wgpu_options(),
        ..Default::default()
    };
    eframe::run_native("Riglet", options, Box::new(|cc| Ok(Box::new(App::new(cc, initial)?))))
}

/// eframe's device, opened with the adapter's own limits (as the CLI's
/// renderer does) instead of egui's 8192-pixel default, so every model the
/// CLI renders opens in the editor too. GL keeps WebGL2 limits.
fn wgpu_options() -> egui_wgpu::WgpuConfiguration {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.device_descriptor = Arc::new(|adapter: &wgpu::Adapter| {
        let required_limits = if adapter.get_info().backend == wgpu::Backend::Gl {
            wgpu::Limits::downlevel_webgl2_defaults()
        } else {
            adapter.limits()
        };
        wgpu::DeviceDescriptor { label: Some("riglet editor"), required_limits, ..Default::default() }
    });
    egui_wgpu::WgpuConfiguration { wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup), ..Default::default() }
}

/// What to do once the user has confirmed discarding unsaved changes.
enum Pending {
    Open(PathBuf),
    Quit,
    /// Export over the imported source file: needs a confirmation naming it.
    ExportOverSource(PathBuf),
}

/// The viewport texture as registered with egui, and what it shows.
struct Shown {
    texture: egui::TextureId,
    target_generation: u64,
    key: Option<FrameKey>,
}

/// Everything the viewport image depends on: re-render when it changes.
#[derive(Clone, PartialEq)]
struct FrameKey {
    gpu_generation: u64,
    frame_version: u64,
    zoom: f32,
    pan: [f32; 2],
    size: (u32, u32),
    hidden: BTreeSet<String>,
}

struct App {
    render_state: egui_wgpu::RenderState,
    viewport: Viewport,
    shown: Option<Shown>,
    state: Option<EditorState>,
    loader: Loader,
    edits: panels::Edits,
    pending: Option<Pending>,
    error: Option<String>,
    render_error: Option<String>,
    needs_fit: bool,
    render_ms: f32,
    quitting: bool,
    show_warnings: bool,
    /// A short result line in the status bar (for example after an export).
    notice: Option<String>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Result<Self, String> {
        let rs = cc.wgpu_render_state.clone().ok_or("the editor needs the wgpu renderer")?;
        let renderer =
            Renderer::from_device(&rs.adapter.get_info(), rs.device.clone(), rs.queue.clone(), Budgets::default());
        let mut app = App {
            render_state: rs,
            viewport: Viewport::new(renderer),
            shown: None,
            state: None,
            loader: Loader::new(Limits::default()),
            edits: panels::Edits::default(),
            pending: None,
            error: None,
            render_error: None,
            needs_fit: false,
            render_ms: 0.0,
            quitting: false,
            show_warnings: false,
            notice: None,
        };
        // Same path as File > Open and drag and drop.
        if let Some(path) = initial {
            app.request_open(path, &cc.egui_ctx);
        }
        Ok(app)
    }

    fn dirty(&self) -> bool {
        self.state.as_ref().is_some_and(EditorState::is_dirty)
    }

    /// Opens `path` after asking about unsaved changes.
    fn request_open(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.dirty() {
            self.pending = Some(Pending::Open(path));
        } else {
            self.start_load(path, ctx);
        }
    }

    /// Loads on a background thread (see `Loader`): the window stays
    /// responsive, the current model stays open until the new one is
    /// ready, and a newer request supersedes an older one.
    fn start_load(&mut self, path: PathBuf, ctx: &egui::Context) {
        let ctx = ctx.clone();
        self.loader.start(path, move || ctx.request_repaint());
    }

    fn poll_load(&mut self) {
        let Some((_, result)) = self.loader.poll() else { return };
        match result {
            Ok(loaded) => {
                let has_warnings = loaded.document.manifest.provenance.iter().any(|p| !p.warnings.is_empty());
                self.state = Some(EditorState::open(loaded, self.loader.limits().clone()));
                self.edits = panels::Edits::default();
                self.show_warnings = has_warnings;
                self.notice = None;
                self.viewport.clear();
                self.needs_fit = true;
                self.render_error = None;
            }
            Err(e) => self.error = Some(load_error_text(&e)),
        }
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        let picked = rfd::FileDialog::new()
            .set_title("Open a model")
            .add_filter("Riglet and Inochi2D models", &["riglet", "inp", "inx"])
            .add_filter("All files", &["*"])
            .pick_file();
        if let Some(path) = picked {
            self.request_open(path, ctx);
        }
    }

    fn save(&mut self) {
        let Some(state) = self.state.as_mut() else { return };
        match state.save() {
            Ok(()) => {}
            Err(SaveError::NoPath) => self.save_as(),
            Err(e) => self.error = Some(format!("Could not save: {e}")),
        }
    }

    fn save_as(&mut self) {
        let Some(state) = self.state.as_mut() else { return };
        let source = state.save_path().unwrap_or(&state.source().path).to_owned();
        let name = source.file_stem().map_or_else(|| "model".into(), |s| s.to_string_lossy().into_owned());
        let mut dialog = rfd::FileDialog::new()
            .set_title("Save as .riglet")
            .add_filter("Riglet model", &["riglet"])
            .set_file_name(format!("{name}.riglet"));
        if let Some(dir) = source.parent() {
            dialog = dialog.set_directory(dir);
        }
        let Some(mut path) = dialog.save_file() else { return };
        // Some portals return the name as typed, without the extension.
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("riglet")) {
            path.as_mut_os_string().push(".riglet");
        }
        if let Err(e) = state.save_as(&path) {
            self.error = Some(format!("Could not save: {e}"));
        }
    }

    fn export_dialog(&mut self) {
        let Some(state) = self.state.as_ref() else { return };
        let source = state.save_path().unwrap_or(&state.source().path).to_owned();
        let name = source.file_stem().map_or_else(|| "model".into(), |s| s.to_string_lossy().into_owned());
        let mut dialog = rfd::FileDialog::new()
            .set_title("Export to Inochi2D")
            .add_filter("Inochi2D puppet", &["inp"])
            .add_filter("Inochi Creator project", &["inx"])
            .set_file_name(format!("{name}.inp"));
        if let Some(dir) = source.parent() {
            dialog = dialog.set_directory(dir);
        }
        let Some(mut path) = dialog.save_file() else { return };
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("inp") || e.eq_ignore_ascii_case("inx")) {
            path.as_mut_os_string().push(".inp");
        }
        self.export(&path, None);
    }

    /// Exports, asking first (by name) when the target is the imported file.
    fn export(&mut self, path: &Path, confirmed_source: Option<&Path>) {
        let Some(state) = self.state.as_mut() else { return };
        match state.export_inochi2d(path, confirmed_source) {
            Ok(record) => {
                let n = record.warnings.len();
                self.notice =
                    Some(format!("Exported {} ({n} warning{})", file_name(path), if n == 1 { "" } else { "s" }));
                if n > 0 {
                    self.show_warnings = true;
                }
            }
            Err(ExportError::NeedsSourceConfirmation(_)) => {
                self.pending = Some(Pending::ExportOverSource(path.to_owned()))
            }
            Err(e) => self.error = Some(format!("Could not export: {e}")),
        }
    }

    fn undo(&mut self) {
        self.edits.reset();
        if let Some(Err(e)) = self.state.as_mut().map(EditorState::undo) {
            self.error = Some(format!("Could not undo: {e}"));
        }
    }

    fn redo(&mut self) {
        self.edits.reset();
        if let Some(Err(e)) = self.state.as_mut().map(EditorState::redo) {
            self.error = Some(format!("Could not redo: {e}"));
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // Text fields keep their own undo and keys.
        let typing = ctx.egui_wants_keyboard_input();
        let pressed = |s: &KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(s));
        if pressed(&OPEN) {
            self.open_dialog(ctx);
        }
        if pressed(&EXPORT) {
            self.export_dialog();
        }
        if pressed(&SAVE_AS) {
            self.save_as();
        } else if pressed(&SAVE) {
            self.save();
        }
        if typing {
            return;
        }
        if pressed(&REDO) || pressed(&REDO_Y) {
            self.redo();
        } else if pressed(&UNDO) {
            self.undo();
        }
        if pressed(&FIT) {
            self.needs_fit = true;
        }
        if pressed(&ACTUAL_SIZE)
            && let Some(state) = self.state.as_mut()
        {
            state.view.zoom = 1.0;
            state.view.pan = [0.0, 0.0];
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new("Open…").shortcut_text(ctx.format_shortcut(&OPEN))).clicked() {
                    self.open_dialog(&ctx);
                }
                let has = self.state.is_some();
                if ui.add_enabled(has, egui::Button::new("Save").shortcut_text(ctx.format_shortcut(&SAVE))).clicked() {
                    self.save();
                }
                if ui
                    .add_enabled(has, egui::Button::new("Save As…").shortcut_text(ctx.format_shortcut(&SAVE_AS)))
                    .clicked()
                {
                    self.save_as();
                }
                if ui
                    .add_enabled(
                        has,
                        egui::Button::new("Export to Inochi2D…").shortcut_text(ctx.format_shortcut(&EXPORT)),
                    )
                    .clicked()
                {
                    self.export_dialog();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                let (can_undo, can_redo) =
                    self.state.as_ref().map_or((false, false), |s| (s.history().can_undo(), s.history().can_redo()));
                if ui
                    .add_enabled(can_undo, egui::Button::new("Undo").shortcut_text(ctx.format_shortcut(&UNDO)))
                    .clicked()
                {
                    self.undo();
                }
                if ui
                    .add_enabled(can_redo, egui::Button::new("Redo").shortcut_text(ctx.format_shortcut(&REDO)))
                    .clicked()
                {
                    self.redo();
                }
            });
            ui.menu_button("View", |ui| {
                if ui.add(egui::Button::new("Fit to Window").shortcut_text(ctx.format_shortcut(&FIT))).clicked() {
                    self.needs_fit = true;
                }
                if ui.add(egui::Button::new("Actual Size").shortcut_text(ctx.format_shortcut(&ACTUAL_SIZE))).clicked()
                    && let Some(state) = self.state.as_mut()
                {
                    state.view.zoom = 1.0;
                    state.view.pan = [0.0, 0.0];
                }
                ui.separator();
                if let Some(state) = self.state.as_mut() {
                    let bg = &mut state.view.background;
                    ui.radio_value(bg, None, "Checkerboard");
                    ui.radio_value(bg, Some([255, 255, 255]), "White");
                    ui.radio_value(bg, Some([128, 128, 128]), "Gray");
                    ui.radio_value(bg, Some([0, 0, 0]), "Black");
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, warnings: usize) {
        ui.horizontal(|ui| {
            if self.state.is_some() {
                let label = format!("Warnings ({warnings})");
                ui.toggle_value(&mut self.show_warnings, label)
                    .on_hover_text("Import, export and live evaluation warnings");
                ui.separator();
            }
            let a = self.viewport.renderer().adapter();
            ui.label(format!("{} ({})", a.name, a.backend));
            ui.separator();
            if let Some(path) = self.loader.loading() {
                ui.spinner();
                ui.label(format!("Loading {}…", file_name(path)));
                ui.separator();
            }
            if let Some(state) = &self.state {
                ui.label(format!("{:.0}%", state.view.zoom * 100.0));
                ui.separator();
                ui.label(format!("render {:.1} ms", self.render_ms));
                if state.is_dirty() {
                    ui.separator();
                    ui.label("unsaved changes");
                }
            }
            if let Some(n) = &self.notice {
                ui.separator();
                ui.label(n);
            }
            if let Some(e) = &self.render_error {
                ui.separator();
                ui.colored_label(Color32::from_rgb(220, 80, 60), e);
            }
        });
    }

    fn viewport_ui(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point();
        // Snapped to physical pixels, so each texel lands on one screen
        // pixel (nearest sampling, no resampling of the renderer's bytes).
        let px = pixel_rect([rect.min.x, rect.min.y], [rect.max.x, rect.max.y], ppp);
        let (w, h) = (px.width, px.height);
        let shown_rect = Rect::from_min_max(Pos2::from(px.min_points), Pos2::from(px.max_points));
        let center = Vec2::new(px.origin[0] + w as f32 * 0.5, px.origin[1] + h as f32 * 0.5);
        let painter = ui.painter_at(rect);

        let Some(state) = self.state.as_mut() else {
            painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
            let text = if self.loader.loading().is_some() {
                "Loading…"
            } else {
                "Open a .riglet, .inp or .inx file (or drop it here)"
            };
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(16.0),
                ui.visuals().weak_text_color(),
            );
            return;
        };
        if w == 0 || h == 0 {
            return;
        }
        if self.needs_fit {
            state.view.fit(state.canvas(), w as f32, h as f32);
            self.needs_fit = false;
        }
        // Pan with any mouse button; zoom with the wheel or a pinch, about
        // the cursor. All in physical pixels.
        if response.dragged() {
            let d = response.drag_delta() * ppp;
            state.view.pan_by([d.x, d.y]);
        }
        if response.hovered() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = pinch * (scroll * 0.0015).exp();
            if factor != 1.0
                && let Some(p) = response.hover_pos()
            {
                let c = (p.to_vec2() * ppp) - center;
                state.view.zoom_about(factor, [c.x, c.y]);
            }
        }

        match state.view.background {
            None => checkerboard(&painter, shown_rect),
            Some([r, g, b]) => {
                painter.rect_filled(shown_rect, 0.0, Color32::from_rgb(r, g, b));
            }
        }

        let key = FrameKey {
            gpu_generation: state.gpu_generation(),
            frame_version: state.frame_version(),
            zoom: state.view.zoom,
            pan: state.view.pan,
            size: (w, h),
            hidden: state.view.hidden.clone(),
        };
        if self.shown.as_ref().and_then(|s| s.key.as_ref()) != Some(&key) {
            let start = Instant::now();
            match self.viewport.render(state, w, h) {
                Ok(_) => {
                    self.render_ms = start.elapsed().as_secs_f32() * 1000.0;
                    self.render_error = None;
                    self.register_target();
                    if let Some(s) = self.shown.as_mut() {
                        s.key = Some(key.clone());
                    }
                }
                Err(e) => {
                    self.render_error = Some(e.to_string());
                    if let Some(s) = self.shown.as_mut() {
                        s.key = None;
                    }
                }
            }
        }
        if self.render_error.is_none()
            && let Some(s) = &self.shown
        {
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            painter.image(s.texture, shown_rect, uv, Color32::WHITE);
        }
        // Selection outline: evaluated mesh edges, drawn by egui on top.
        if let Some(state) = self.state.as_mut()
            && let Some(node) = state.view.selection.clone()
        {
            let (zoom, pan) = (state.view.zoom, state.view.pan);
            let (rig, frame) = state.frame();
            let edges = outline_edges(rig, frame, &node);
            panels::draw_outline(&painter, &edges, zoom, center, pan, ppp);
        }
    }

    /// Registers the viewport target with egui, or points the existing
    /// registration at a new target after a resize.
    fn register_target(&mut self) {
        let Some(target) = self.viewport.target() else { return };
        let generation = self.viewport.target_generation();
        if self.shown.as_ref().is_some_and(|s| s.target_generation == generation) {
            return;
        }
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let device = &self.render_state.device;
        let mut renderer = self.render_state.renderer.write();
        match self.shown.as_mut() {
            Some(s) => {
                renderer.update_egui_texture_from_wgpu_texture(device, &view, wgpu::FilterMode::Nearest, s.texture);
                s.target_generation = generation;
            }
            None => {
                let texture = renderer.register_native_texture(device, &view, wgpu::FilterMode::Nearest);
                self.shown = Some(Shown { texture, target_generation: generation, key: None });
            }
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(message) = self.error.clone() {
            let modal = egui::Modal::new(egui::Id::unique("riglet error dialog")).show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.heading("Something went wrong");
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    ui.label(message);
                });
                ui.button("OK").clicked()
            });
            if modal.inner || modal.should_close() {
                self.error = None;
            }
        }
        if let Some(Pending::ExportOverSource(path)) = &self.pending {
            let path = path.clone();
            let mut choice = None;
            egui::Modal::new(egui::Id::unique("riglet export over source")).show(ctx, |ui| {
                ui.set_max_width(480.0);
                ui.heading(format!("Replace {}?", file_name(&path)));
                ui.label(format!(
                    "{} is the file this model was imported from. Exporting replaces it; the original contents are lost.",
                    path.display()
                ));
                ui.horizontal(|ui| {
                    if ui.button(format!("Replace {}", file_name(&path))).clicked() {
                        choice = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(false);
                    }
                });
            });
            if let Some(yes) = choice {
                self.pending = None;
                if yes {
                    self.export(&path, Some(&path));
                }
            }
            return;
        }
        if self.pending.is_some() {
            let mut choice = None;
            egui::Modal::new(egui::Id::unique("riglet unsaved dialog")).show(ctx, |ui| {
                ui.heading("Unsaved changes");
                ui.label("The open model has changes that are not saved.");
                ui.horizontal(|ui| {
                    if ui.button("Discard changes").clicked() {
                        choice = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(false);
                    }
                });
            });
            match (choice, self.pending.take()) {
                (Some(true), Some(Pending::Open(path))) => self.start_load(path, ctx),
                (Some(true), Some(Pending::Quit)) => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                (Some(_), _) => {}
                (None, p) => self.pending = p,
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_load();
        if self.loader.loading().is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // Dropped files open like picked ones (the first one only).
        if let Some(path) = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_owned())) {
            self.request_open(path, &ctx);
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty() && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending = Some(Pending::Quit);
        }
        self.shortcuts(&ctx);

        let title = match &self.state {
            Some(s) => {
                let path = s.save_path().unwrap_or(&s.source().path);
                format!("{}{} — Riglet", file_name(path), if s.is_dirty() { " •" } else { "" })
            }
            None => "Riglet".to_owned(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        // One list per frame (the live entries follow the current frame).
        let warnings = self.state.as_mut().map(EditorState::warnings).unwrap_or_default();
        egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui, warnings.len()));
        if let Some(state) = self.state.as_mut() {
            let edits = &mut self.edits;
            if self.show_warnings {
                egui::Panel::bottom("warnings").resizable(true).default_size(200.0).show(ui, |ui| {
                    panels::warnings_panel(ui, state, edits, &warnings);
                });
            }
            egui::Panel::left("nodes").resizable(true).default_size(260.0).show(ui, |ui| {
                panels::node_tree(ui, state, edits);
            });
            egui::Panel::right("parameters").resizable(true).default_size(300.0).show(ui, |ui| {
                egui::Panel::bottom("properties").resizable(true).default_size(260.0).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("riglet properties").show(ui, |ui| {
                        panels::properties(ui, state, edits);
                    });
                });
                egui::ScrollArea::vertical().id_salt("riglet parameters").auto_shrink([false, false]).show(ui, |ui| {
                    panels::parameters(ui, state, edits);
                });
            });
        }
        egui::CentralPanel::no_frame().show(ui, |ui| self.viewport_ui(ui));
        self.dialogs(&ctx);
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

fn load_error_text(e: &LoadError) -> String {
    let mut text = format!("Could not open the file.\n\n{e}");
    if let LoadError::Invalid { issues, .. } = e {
        for issue in issues.iter().take(50) {
            text.push_str(&format!("\n• {issue}"));
        }
        if issues.len() > 50 {
            text.push_str(&format!("\n… and {} more", issues.len() - 50));
        }
    }
    text
}

/// The transparent-background checkerboard, 8-point cells.
fn checkerboard(painter: &egui::Painter, rect: Rect) {
    const CELL: f32 = 8.0;
    let light = Color32::from_gray(204);
    let dark = Color32::from_gray(153);
    painter.rect_filled(rect, 0.0, light);
    let cols = (rect.width() / CELL).ceil() as i32;
    let rows = (rect.height() / CELL).ceil() as i32;
    for y in 0..rows {
        for x in (y % 2..cols).step_by(2) {
            let min = rect.min + Vec2::new(x as f32 * CELL, y as f32 * CELL);
            let cell = Rect::from_min_size(min, Vec2::splat(CELL)).intersect(rect);
            painter.rect_filled(cell, 0.0, dark);
        }
    }
}
