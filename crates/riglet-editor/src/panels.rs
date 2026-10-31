//! Side panels: node tree, parameters, properties. Model edits go through
//! `EditorState::apply` as commands; slider values (posing) and the
//! hide-in-view toggles are session state.

use eframe::egui::{self, Color32, Id as EguiId, Rect, Sense, Stroke, Vec2};
use riglet_editor::warnings::{Entry, Origin, Target};
use riglet_editor::{Command, EditorState, Gesture, Merge};
use riglet_format::model::{Node, ParamKind};

/// In-progress edits of the properties panel.
#[derive(Default)]
pub struct Edits {
    /// The text field being typed in, its buffer and gesture. The buffer
    /// may hold text the model refused (an empty name), shown with the
    /// error until it is fixed or the field loses focus.
    field: Option<FieldEdit>,
    /// The value being dragged and its gesture.
    drag: Option<(EguiId, Gesture)>,
    pub selected_param: Option<String>,
    /// A node to reveal in the tree (expand its ancestors, scroll to it),
    /// set when a warning entry is clicked.
    pub reveal: Option<String>,
    /// Code filter of the warnings panel.
    pub warning_filter: String,
}

struct FieldEdit {
    id: EguiId,
    gesture: Gesture,
    text: String,
    error: Option<String>,
}

impl Edits {
    /// Forgets in-progress edits (after undo, redo or opening a file).
    pub fn reset(&mut self) {
        self.field = None;
        self.drag = None;
    }
}

// ---------- Node tree ----------

pub fn node_tree(ui: &mut egui::Ui, state: &mut EditorState, edits: &mut Edits) {
    ui.heading("Nodes");
    ui.separator();
    // Ancestors of the node to reveal: expanded this frame.
    let mut path = Vec::new();
    if let Some(id) = &edits.reveal {
        let nodes = &state.document().model.nodes;
        let mut at = nodes.get(id).and_then(|n| n.base().parent.clone());
        while let Some(p) = at {
            if path.len() > nodes.len() {
                break;
            }
            at = nodes.get(&p).and_then(|n| n.base().parent.clone());
            path.push(p);
        }
    }
    let reveal = edits.reveal.take();
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        let root = state.document().model.root.clone();
        node_row(ui, state, &root, 0, &path, reveal.as_deref());
    });
}

fn kind_label(node: &Node) -> &'static str {
    match node {
        Node::Group(g) if g.composite => "composite",
        Node::Group(_) => "group",
        Node::Drawable(_) => "drawable",
        Node::Warp(_) => "warp",
        Node::Rotation(_) => "rotation",
        Node::MeshDeformer(_) => "mesh deformer",
    }
}

fn node_row(ui: &mut egui::Ui, state: &mut EditorState, id: &str, depth: usize, open: &[String], reveal: Option<&str>) {
    // Depth is bounded by validation (the tree is acyclic and within the
    // node limit); egui's own nesting keeps this readable.
    let Some(node) = state.document().model.nodes.get(id) else { return };
    let base = node.base();
    let (name, children, visible, kind) = (base.name.clone(), base.children.clone(), base.visible, kind_label(node));
    let selected = state.view.selection.as_deref() == Some(id);
    let header = |ui: &mut egui::Ui, state: &mut EditorState| {
        let mut shown = !state.view.hidden.contains(id);
        if ui.checkbox(&mut shown, "").on_hover_text("Show in the viewport (this session only, not saved)").changed() {
            if shown {
                state.view.hidden.remove(id);
            } else {
                state.view.hidden.insert(id.to_owned());
            }
        }
        let mut text = egui::RichText::new(&name);
        if !visible {
            text = text.weak().italics();
        }
        let label = ui.selectable_label(selected, text).on_hover_text(format!("{kind} `{id}`"));
        if label.clicked() {
            state.view.selection = Some(id.to_owned());
        }
        if reveal == Some(id) {
            label.scroll_to_me(Some(egui::Align::Center));
        }
        ui.label(egui::RichText::new(kind).weak().small());
    };
    if children.is_empty() {
        ui.horizontal(|ui| header(ui, state));
    } else {
        let mut collapsing = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            EguiId::unique(("riglet node", id)),
            depth < 2,
        );
        if open.iter().any(|o| o == id) {
            collapsing.set_open(true);
        }
        collapsing.show_header(ui, |ui| header(ui, state)).body(|ui| {
            for child in &children {
                node_row(ui, state, child, depth + 1, open, reveal);
            }
        });
    }
}

// ---------- Parameters (posing: session state) ----------

pub fn parameters(ui: &mut egui::Ui, state: &mut EditorState, edits: &mut Edits) {
    ui.horizontal(|ui| {
        ui.heading("Parameters");
        if ui.button("Reset all").on_hover_text("Every parameter back to its default (not an edit)").clicked() {
            state.reset_params();
        }
    });
    ui.separator();
    // Groups in order of first appearance; ungrouped first.
    let params: Vec<(String, String, Option<String>, ParamKind)> = state
        .document()
        .model
        .parameters
        .iter()
        .map(|p| (p.id.clone(), p.name.clone(), p.group.clone(), p.kind))
        .collect();
    let mut groups: Vec<Option<String>> = Vec::new();
    for (_, _, g, _) in &params {
        if !groups.contains(g) {
            groups.push(g.clone());
        }
    }
    groups.sort_by_key(Option::is_some);
    for group in groups {
        let members: Vec<_> = params.iter().filter(|(_, _, g, _)| *g == group).collect();
        let title = group.clone().unwrap_or_else(|| "Ungrouped".into());
        egui::CollapsingHeader::new(format!("{title} ({})", members.len()))
            .id_salt(("riglet param group", &group))
            .default_open(true)
            .show(ui, |ui| {
                for (id, name, _, kind) in members {
                    param_row(ui, state, edits, id, name, *kind);
                }
            });
    }
}

fn param_row(ui: &mut egui::Ui, state: &mut EditorState, edits: &mut Edits, id: &str, name: &str, kind: ParamKind) {
    let Some(i) = state.rig().param_index(id) else { return };
    let Some(p) = state.rig().params.get(i).cloned() else { return };
    let Some(&value) = state.param_values().values.get(i) else { return };
    ui.horizontal(|ui| {
        let selected = edits.selected_param.as_deref() == Some(id);
        if ui.selectable_label(selected, name).on_hover_text(format!("`{id}`: click to edit its properties")).clicked()
        {
            edits.selected_param = Some(id.to_owned());
            state.view.selection = None;
        }
        if ui.small_button("Reset").on_hover_text("Back to the default").clicked() {
            state.reset_param(id);
        }
    });
    match kind {
        ParamKind::OneD => {
            let mut v = value[0];
            let slider = egui::Slider::new(&mut v, p.min[0]..=p.max[0]).clamping(egui::SliderClamping::Always);
            if ui.add(slider).changed() {
                state.set_param(id, [v, value[1]]);
            }
        }
        ParamKind::TwoD => {
            if let Some(v) = pad(ui, id, value, p.min, p.max, p.default) {
                state.set_param(id, v);
            }
        }
    }
    ui.add_space(4.0);
}

/// A 2D pad: drag to set both components; double-click resets. Returns
/// the new value while dragging.
fn pad(
    ui: &mut egui::Ui,
    id: &str,
    value: [f32; 2],
    min: [f32; 2],
    max: [f32; 2],
    default: [f32; 2],
) -> Option<[f32; 2]> {
    let size = Vec2::splat(ui.available_width().clamp(80.0, 160.0));
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let to_screen = |v: [f32; 2]| {
        let t = [(v[0] - min[0]) / (max[0] - min[0]), (v[1] - min[1]) / (max[1] - min[1])];
        // +Y up on the pad, like a joystick.
        egui::pos2(rect.left() + t[0] * rect.width(), rect.bottom() - t[1] * rect.height())
    };
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 2.0, visuals.extreme_bg_color);
    let d = to_screen(default);
    let weak = Stroke::new(1.0, visuals.weak_text_color());
    painter.line_segment([egui::pos2(rect.left(), d.y), egui::pos2(rect.right(), d.y)], weak);
    painter.line_segment([egui::pos2(d.x, rect.top()), egui::pos2(d.x, rect.bottom())], weak);
    painter.circle_filled(to_screen(value), 5.0, visuals.selection.bg_fill);
    let response = response.on_hover_text(format!("`{id}`: {:.3}, {:.3}", value[0], value[1]));
    if response.double_clicked() {
        return Some(default);
    }
    if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let tx = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let ty = ((rect.bottom() - pos.y) / rect.height()).clamp(0.0, 1.0);
        return Some([min[0] + tx * (max[0] - min[0]), min[1] + ty * (max[1] - min[1])]);
    }
    None
}

// ---------- Properties (model edits: commands) ----------

pub fn properties(ui: &mut egui::Ui, state: &mut EditorState, edits: &mut Edits) {
    ui.heading("Properties");
    ui.separator();
    if let Some(node) = state.view.selection.clone() {
        node_properties(ui, state, edits, &node);
    } else if let Some(param) = edits.selected_param.clone() {
        param_properties(ui, state, edits, &param);
    } else {
        ui.weak("Select a node or a parameter name.");
    }
}

fn node_properties(ui: &mut egui::Ui, state: &mut EditorState, edits: &mut Edits, id: &str) {
    let Some(node) = state.document().model.nodes.get(id) else {
        ui.weak("The selected node no longer exists.");
        return;
    };
    let (name, visible, kind) = (node.base().name.clone(), node.base().visible, kind_label(node));
    egui::Grid::new("riglet node props").num_columns(2).show(ui, |ui| {
        ui.label("Name");
        let node = id.to_owned();
        text_field(ui, state, edits, EguiId::unique(("riglet node name", id)), &name, move |text| {
            Some(Command::RenameNode { node: node.clone(), name: text })
        });
        ui.end_row();
        ui.label("Id");
        ui.label(egui::RichText::new(id).monospace().weak())
            .on_hover_text("Ids never change (export and tracking use them)");
        ui.end_row();
        ui.label("Type");
        ui.label(kind);
        ui.end_row();
        ui.label("Visible");
        let mut v = visible;
        if ui.checkbox(&mut v, "").on_hover_text("Saved in the model (undoable)").changed()
            && let Err(e) = state.apply(Command::SetNodeVisible { node: id.to_owned(), visible: v }, Merge::Never)
        {
            ui.colored_label(error_color(), e.to_string());
        }
        ui.end_row();
    });
}

fn param_properties(ui: &mut egui::Ui, state: &mut EditorState, edits: &mut Edits, id: &str) {
    let Some(p) = state.document().model.parameters.iter().find(|p| p.id == id).cloned() else {
        ui.weak("The selected parameter no longer exists.");
        return;
    };
    egui::Grid::new("riglet param props").num_columns(2).show(ui, |ui| {
        ui.label("Name");
        let pid = id.to_owned();
        text_field(ui, state, edits, EguiId::unique(("riglet param name", id)), &p.name, move |text| {
            Some(Command::RenameParameter { parameter: pid.clone(), name: text })
        });
        ui.end_row();
        ui.label("Group");
        let pid = id.to_owned();
        let group = p.group.clone().unwrap_or_default();
        text_field(ui, state, edits, EguiId::unique(("riglet param group", id)), &group, move |text| {
            // An empty field means "no group".
            let group = if text.trim().is_empty() { None } else { Some(text) };
            Some(Command::SetParameterGroup { parameter: pid.clone(), group })
        });
        ui.end_row();
        ui.label("Id");
        ui.label(egui::RichText::new(id).monospace().weak())
            .on_hover_text("Ids never change (export and tracking use them)");
        ui.end_row();
        let axes: &[&str] = if p.kind == ParamKind::TwoD { &["X", "Y"] } else { &[""] };
        for (c, axis) in axes.iter().enumerate() {
            let (Some(&lo), Some(&hi), Some(&d)) = (p.min.get(c), p.max.get(c), p.default.get(c)) else { continue };
            ui.label(format!("Range {axis}"));
            ui.horizontal(|ui| {
                for (which, current) in [(0, lo), (1, hi)] {
                    let mut v = current;
                    let drag_id = EguiId::unique(("riglet range", id, c, which));
                    let response = ui
                        .add(egui::DragValue::new(&mut v).speed(0.01).max_decimals(4))
                        .on_hover_text("A range must contain the default and every keyform key");
                    if let Some(g) = drag_gesture(state, edits, drag_id, &response)
                        && response.changed()
                    {
                        let (mut min, mut max) = (p.min.clone(), p.max.clone());
                        if let Some(slot) = if which == 0 { min.get_mut(c) } else { max.get_mut(c) } {
                            *slot = v;
                        }
                        let cmd = Command::SetParameterRange { parameter: id.to_owned(), min, max };
                        if let Err(e) = state.apply(cmd, Merge::Gesture(g)) {
                            response.show_tooltip_text(e.to_string());
                        }
                    }
                }
            });
            ui.end_row();
            ui.label(format!("Default {axis}"));
            ui.horizontal(|ui| {
                let mut v = d;
                let drag_id = EguiId::unique(("riglet default", id, c));
                let response = ui.add(egui::DragValue::new(&mut v).speed(0.01).range(lo..=hi).max_decimals(4));
                if let Some(g) = drag_gesture(state, edits, drag_id, &response)
                    && response.changed()
                {
                    let mut default = p.default.clone();
                    if let Some(slot) = default.get_mut(c) {
                        *slot = v;
                    }
                    let cmd = Command::SetParameterDefault { parameter: id.to_owned(), default };
                    if let Err(e) = state.apply(cmd, Merge::Gesture(g)) {
                        response.show_tooltip_text(e.to_string());
                    }
                }
            });
            ui.end_row();
        }
    });
    if ui.button("Use the current value as default").clicked()
        && let Some(i) = state.rig().param_index(id)
        && let Some(&v) = state.param_values().values.get(i)
    {
        let default = v.iter().take(p.kind.components()).copied().collect();
        if let Err(e) = state.apply(Command::SetParameterDefault { parameter: id.to_owned(), default }, Merge::Never) {
            ui.colored_label(error_color(), e.to_string());
        }
    }
}

/// The gesture for a dragged or typed-in value: a new one when the drag
/// starts or the field gains focus, the same one while it continues, none
/// after it ends.
fn drag_gesture(state: &mut EditorState, edits: &mut Edits, id: EguiId, r: &egui::Response) -> Option<Gesture> {
    if r.drag_started() || r.gained_focus() || (r.changed() && edits.drag.is_none_or(|(d, _)| d != id)) {
        edits.drag = Some((id, state.begin_gesture()));
    }
    let gesture = edits.drag.filter(|(d, _)| *d == id).map(|(_, g)| g);
    if r.drag_stopped() || r.lost_focus() {
        edits.drag = None;
    }
    gesture
}

/// A single-line text field that applies `make(text)` as one gesture per
/// focus. Refused text (an empty name) stays in the field with the error,
/// and the model keeps its last valid value.
fn text_field(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    edits: &mut Edits,
    id: EguiId,
    current: &str,
    make: impl Fn(String) -> Option<Command>,
) {
    let mut text = match &edits.field {
        Some(f) if f.id == id => f.text.clone(),
        _ => current.to_owned(),
    };
    ui.vertical(|ui| {
        let response = ui.add(egui::TextEdit::singleline(&mut text).id(id).desired_width(180.0));
        if response.gained_focus() || (response.has_focus() && edits.field.as_ref().is_none_or(|f| f.id != id)) {
            edits.field = Some(FieldEdit { id, gesture: state.begin_gesture(), text: text.clone(), error: None });
        }
        if response.changed()
            && let Some(f) = edits.field.as_mut().filter(|f| f.id == id)
        {
            f.text = text.clone();
            f.error = match make(text.clone()).map(|cmd| state.apply(cmd, Merge::Gesture(f.gesture))) {
                Some(Err(e)) => Some(e.to_string()),
                _ => None,
            };
        }
        if let Some(e) = edits.field.as_ref().filter(|f| f.id == id).and_then(|f| f.error.as_ref()) {
            ui.colored_label(error_color(), e);
        }
        if response.lost_focus() && edits.field.as_ref().is_some_and(|f| f.id == id) {
            edits.field = None;
        }
    });
}

fn error_color() -> Color32 {
    Color32::from_rgb(220, 80, 60)
}

/// Draws model-space edges over the viewport. `center` and `pan` in
/// physical pixels; `ppp` converts to points.
pub fn draw_outline(
    painter: &egui::Painter,
    edges: &[[[f32; 2]; 2]],
    zoom: f32,
    center: Vec2,
    pan: [f32; 2],
    ppp: f32,
) {
    let stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 170, 0, 200));
    let to_points =
        |m: [f32; 2]| egui::pos2((m[0] * zoom + center.x + pan[0]) / ppp, (m[1] * zoom + center.y + pan[1]) / ppp);
    let clip: Rect = painter.clip_rect();
    for [a, b] in edges {
        let (p, q) = (to_points(*a), to_points(*b));
        if clip.intersects(Rect::from_two_pos(p, q)) {
            painter.line_segment([p, q], stroke);
        }
    }
}

// ---------- Warnings ----------

fn origin_title(origin: &Origin) -> String {
    match origin {
        Origin::Import { source, source_version, imported_at } => {
            let source = serde_name(source);
            let version = source_version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
            format!("From import ({source}{version}, {imported_at}); may be out of date after edits")
        }
        Origin::Export { file } => format!("Last export ({file})"),
        Origin::Evaluation => "Current frame (live)".into(),
    }
}

fn serde_name(source: &riglet_format::model::ProvenanceSource) -> String {
    use riglet_format::model::ProvenanceSource as P;
    match source {
        P::Native => "native",
        P::Live2dMoc3 => "live2d-moc3",
        P::Inochi2dInp => "inochi2d-inp",
        P::Inochi2dInx => "inochi2d-inx",
        P::Psd => "psd",
        P::Kra => "kra",
    }
    .into()
}

/// The warnings panel: entries grouped by source, each with its code and
/// message; an entry naming a node or parameter selects it when clicked.
pub fn warnings_panel(ui: &mut egui::Ui, state: &mut EditorState, edits: &mut Edits, entries: &[Entry]) {
    ui.horizontal(|ui| {
        ui.heading(format!("Warnings ({})", entries.len()));
        ui.label("Filter:");
        ui.add(egui::TextEdit::singleline(&mut edits.warning_filter).hint_text("code").desired_width(160.0));
    });
    ui.separator();
    let filter = edits.warning_filter.trim().to_uppercase();
    let shown: Vec<&Entry> = entries.iter().filter(|e| filter.is_empty() || e.code.contains(&filter)).collect();
    if shown.is_empty() {
        ui.weak(if entries.is_empty() { "No warnings." } else { "No warnings match the filter." });
        return;
    }
    let mut groups: Vec<&Origin> = Vec::new();
    for e in &shown {
        if !groups.contains(&&e.origin) {
            groups.push(&e.origin);
        }
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for origin in groups {
            let members: Vec<&&Entry> = shown.iter().filter(|e| e.origin == *origin).collect();
            egui::CollapsingHeader::new(format!("{} — {}", origin_title(origin), members.len()))
                .id_salt(("riglet warnings", format!("{origin:?}")))
                .default_open(true)
                .show(ui, |ui| {
                    for e in members {
                        warning_row(ui, state, edits, e);
                    }
                });
        }
    });
}

fn warning_row(ui: &mut egui::Ui, state: &mut EditorState, edits: &mut Edits, e: &Entry) {
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(&e.code).monospace().strong());
        match &e.target {
            Target::Node(id) => {
                let name = state.document().model.nodes.get(id).map_or_else(|| id.clone(), |n| n.base().name.clone());
                if ui.link(name).on_hover_text(format!("Select node `{id}`")).clicked() {
                    state.view.selection = Some(id.clone());
                    edits.selected_param = None;
                    edits.reveal = Some(id.clone());
                }
            }
            Target::Parameter(id) => {
                let name = state
                    .document()
                    .model
                    .parameters
                    .iter()
                    .find(|p| p.id == *id)
                    .map_or_else(|| id.clone(), |p| p.name.clone());
                if ui.link(name).on_hover_text(format!("Select parameter `{id}`")).clicked() {
                    edits.selected_param = Some(id.clone());
                    state.view.selection = None;
                }
            }
            Target::Other { kind, id } => {
                ui.weak(format!("({kind} `{id}`)"));
            }
            Target::Removed(id) => {
                ui.weak(format!("(refers to `{id}`, no longer in the model)"));
            }
            Target::None => {}
        }
        ui.label(&e.message);
    });
}
