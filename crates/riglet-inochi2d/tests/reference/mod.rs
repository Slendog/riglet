//! A test-only reimplementation of how Inochi2D 0.8.7 positions vertices:
//! node transforms (`math/transform.d`, inmath `eulerRotation`), parameter
//! bindings (`core/param/package.d`, `binding.d`) and MeshGroup filtering
//! (`core/nodes/meshgroup/package.d`), from reading the sources. It ignores
//! everything that doesn't move vertices (colors, zSort, physics, drivers).
//! Used to measure where the importer's mapping differs from Inochi2D.
//!
//! This file is a PORT of parts of Inochi2D and inmath. Their notices:
//!
//! Inochi2D: Copyright (c) 2020, Inochi2D Project. Distributed under the
//! 2-Clause BSD License:
//!
//!   Redistribution and use in source and binary forms, with or without
//!   modification, are permitted provided that the following conditions
//!   are met:
//!   1. Redistributions of source code must retain the above copyright
//!      notice, this list of conditions and the following disclaimer.
//!   2. Redistributions in binary form must reproduce the above copyright
//!      notice, this list of conditions and the following disclaimer in the
//!      documentation and/or other materials provided with the distribution.
//!
//!   THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
//!   "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
//!   LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A
//!   PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
//!   HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
//!   SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
//!   LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
//!   DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
//!   THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
//!   (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
//!   OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//!
//! inmath (`eulerRotation`, `toMatrix`, `cubic`): Copyright (c) 2011, David Herberth; 2021, Inochi2D Project.
//! MIT License:
//!
//!   Permission is hereby granted, free of charge, to any person obtaining
//!   a copy of this software and associated documentation files (the
//!   "Software"), to deal in the Software without restriction, including
//!   without limitation the rights to use, copy, modify, merge, publish,
//!   distribute, sublicense, and/or sell copies of the Software, and to
//!   permit persons to whom the Software is furnished to do so, subject to
//!   the following conditions:
//!
//!   The above copyright notice and this permission notice shall be
//!   included in all copies or substantial portions of the Software.
//!
//!   THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
//!   EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//!   MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
//!   IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
//!   CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
//!   TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
//!   SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]
// Written to mirror the D sources line by line, not idiomatic Rust.
#![allow(
    clippy::too_many_arguments,
    clippy::chunks_exact_to_as_chunks,
    clippy::redundant_closure,
    clippy::collapsible_if,
    clippy::manual_is_multiple_of,
    clippy::needless_range_loop
)]

use std::collections::HashMap;

use riglet_inochi2d::reinterpolate::{self, Grid};
use serde_json::Value;

pub type V2 = [f32; 2];
/// Row-major 4x4 (inmath: `m[row][col]`, `M * v`).
pub type M4 = [[f32; 4]; 4];

pub const IDENTITY: M4 = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

pub fn mul(a: &M4, b: &M4) -> M4 {
    let mut r = [[0.0; 4]; 4];
    for (i, row) in r.iter_mut().enumerate() {
        for (j, x) in row.iter_mut().enumerate() {
            *x = (0..4).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

pub fn apply(m: &M4, p: [f32; 3]) -> [f32; 3] {
    let v = [p[0], p[1], p[2], 1.0];
    let row = |i: usize| (0..4).map(|k| m[i][k] * v[k]).sum::<f32>();
    [row(0), row(1), row(2)]
}

/// Inverse of an affine 4x4 (last row 0 0 0 1).
pub fn inverse(m: &M4) -> M4 {
    let a = |i: usize, j: usize| f64::from(m[i][j]);
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0))
        + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    // Cofactor C_ji (the adjugate's entry ij): minor without row j, column i.
    let cof = |i: usize, j: usize| {
        let rows: Vec<usize> = (0..3).filter(|&r| r != j).collect();
        let cols: Vec<usize> = (0..3).filter(|&c| c != i).collect();
        let sign = if (i + j) % 2 == 0 { 1.0 } else { -1.0 };
        sign * (a(rows[0], cols[0]) * a(rows[1], cols[1]) - a(rows[0], cols[1]) * a(rows[1], cols[0]))
    };
    let mut r = IDENTITY;
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (cof(i, j) / det) as f32;
        }
    }
    let t = [m[0][3], m[1][3], m[2][3]];
    for i in 0..3 {
        r[i][3] = -(0..3).map(|k| r[i][k] * t[k]).sum::<f32>();
    }
    r
}

/// `mat4.translation(t) * quat.eulerRotation(r.x, r.y, r.z).toMatrix * mat4.scaling(s.x, s.y, 1)`.
pub fn trs(t: [f32; 3], r: [f32; 3], s: V2) -> M4 {
    let (roll, pitch, yaw) = (f64::from(r[0]), f64::from(r[1]), f64::from(r[2]));
    let (cr, cp, cy) = ((roll / 2.0).cos(), (pitch / 2.0).cos(), (yaw / 2.0).cos());
    let (sr, sp, sy) = ((roll / 2.0).sin(), (pitch / 2.0).sin(), (yaw / 2.0).sin());
    let w = cr * cp * cy + sr * sp * sy;
    let x = sr * cp * cy - cr * sp * sy;
    let y = cr * sp * cy + sr * cp * sy;
    let z = cr * cp * sy - sr * sp * cy;
    let q = [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ];
    let mut m = IDENTITY;
    let sc = [f64::from(s[0]), f64::from(s[1]), 1.0];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = (q[i][j] * sc[j]) as f32;
        }
        m[i][3] = t[i];
    }
    m
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Filter {
    None,
    Pre(usize),
    Post(usize),
}

#[derive(Debug, Clone)]
pub struct Node {
    pub uuid: u32,
    pub ty: String,
    pub name: String,
    pub children: Vec<usize>,
    pub enabled: bool,
    trans: [f32; 3],
    rot: [f32; 3],
    scale: V2,
    pub verts: Vec<V2>,
    origin: V2,
    indices: Vec<usize>,
    pub dynamic: bool,
    translate_children: bool,
    propagate: bool,
}

impl Node {
    pub fn drawable(&self) -> bool {
        matches!(self.ty.as_str(), "Part" | "Mask" | "MeshGroup")
    }
}

#[derive(Debug, Clone)]
enum BValues {
    Scalar(Vec<Vec<f32>>),
    Deform(Vec<Vec<Vec<V2>>>),
}

#[derive(Debug, Clone)]
struct Binding {
    node: usize,
    name: String,
    values: BValues,
    interp: String,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub uuid: u32,
    pub name: String,
    pub vec2: bool,
    pub min: V2,
    pub max: V2,
    pub defaults: V2,
    pub axis: [Vec<f32>; 2],
    bindings: Vec<Binding>,
}

/// One drawable's result: world-space vertices, and the vertices a
/// MeshGroup filter left undeformed because its bitmask lookup missed
/// (outside the rest bounds, or the cell's corner is in no triangle).
#[derive(Debug, Clone)]
pub struct Evaluated {
    pub positions: Vec<V2>,
    pub outside: Vec<usize>,
    /// Triangle per vertex from the MeshGroup lookup (-1 = miss); empty if
    /// no MeshGroup filters this drawable.
    pub triangles: Vec<i64>,
}

thread_local! {
    /// Sentinel `fixed` map: choose triangles by E6 instead of looking up.
    static E6_MARKER: FixedTriangles = HashMap::from([(u32::MAX, Vec::new())]);
}

fn e6_triangle(c: V2, verts: &[V2], indices: &[usize]) -> i64 {
    let tris: Vec<[V2; 3]> = indices.chunks_exact(3).map(|t| [verts[t[0]], verts[t[1]], verts[t[2]]]).collect();
    let bary = |t: [V2; 3]| {
        let d = (t[1][1] - t[2][1]) * (t[0][0] - t[2][0]) + (t[2][0] - t[1][0]) * (t[0][1] - t[2][1]);
        if d.abs() < 1e-12 {
            return None;
        }
        let l0 = ((t[1][1] - t[2][1]) * (c[0] - t[2][0]) + (t[2][0] - t[1][0]) * (c[1] - t[2][1])) / d;
        let l1 = ((t[2][1] - t[0][1]) * (c[0] - t[2][0]) + (t[0][0] - t[2][0]) * (c[1] - t[2][1])) / d;
        Some([l0, l1, 1.0 - l0 - l1])
    };
    if let Some(i) = tris.iter().position(|&t| bary(t).is_some_and(|l| l.iter().all(|&x| x >= -1e-6))) {
        return i as i64;
    }
    let seg = |p: V2, a: V2, b: V2| {
        let (ab, ap) = ([b[0] - a[0], b[1] - a[1]], [p[0] - a[0], p[1] - a[1]]);
        let len = ab[0] * ab[0] + ab[1] * ab[1];
        let t = if len > 0.0 { ((ap[0] * ab[0] + ap[1] * ab[1]) / len).clamp(0.0, 1.0) } else { 0.0 };
        let q = [a[0] + ab[0] * t - p[0], a[1] + ab[1] * t - p[1]];
        q[0] * q[0] + q[1] * q[1]
    };
    let mut best = (-1i64, f32::MAX);
    for (i, t) in tris.iter().enumerate() {
        if bary(*t).is_none() {
            continue;
        }
        let d = seg(c, t[0], t[1]).min(seg(c, t[1], t[2])).min(seg(c, t[2], t[0]));
        if d < best.1 {
            best = (i as i64, d);
        }
    }
    best.0
}

/// Triangle choices to reuse instead of Inochi2D's per-frame lookup, by
/// drawable uuid (a variant for measuring, not Inochi2D behavior).
pub type FixedTriangles = HashMap<u32, Vec<i64>>;

/// Per MeshGroup: the rest bitmask and per-triangle offset matrices.
#[derive(Debug, Clone)]
struct Precalc {
    bounds: [f32; 4],
    width: i64,
    mask: Vec<u16>,
    offset: Vec<[[f32; 3]; 3]>,
}

pub struct Puppet {
    pub nodes: Vec<Node>,
    pub params: Vec<Param>,
    filters: Vec<Filter>,
    precalc: HashMap<usize, Precalc>,
}

fn f32s(v: Option<&Value>) -> Vec<f32> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect())
        .unwrap_or_default()
}

fn pairs(f: &[f32]) -> Vec<V2> {
    f.chunks_exact(2).map(|c| [c[0], c[1]]).collect()
}

type Mat3 = [[f32; 3]; 3];

fn m3mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn m3apply(m: &Mat3, p: V2) -> V2 {
    [m[0][0] * p[0] + m[0][1] * p[1] + m[0][2], m[1][0] * p[0] + m[1][1] * p[1] + m[1][2]]
}

fn bounds_of(vs: &[V2]) -> [f32; 4] {
    let mut b = [f32::MAX, f32::MAX, -f32::MAX, -f32::MAX];
    for v in vs {
        b = [b[0].min(v[0]), b[1].min(v[1]), b[2].max(v[0]), b[3].max(v[1])];
    }
    [b[0].floor(), b[1].floor(), b[2].ceil(), b[3].ceil()]
}

fn in_triangle(pt: V2, t: [V2; 3]) -> bool {
    let sign = |p1: V2, p2: V2, p3: V2| (p1[0] - p3[0]) * (p2[1] - p3[1]) - (p2[0] - p3[0]) * (p1[1] - p3[1]);
    let (d1, d2, d3) = (sign(pt, t[0], t[1]), sign(pt, t[1], t[2]), sign(pt, t[2], t[0]));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

fn precalculate(n: &Node) -> Precalc {
    let bounds = bounds_of(&n.verts);
    let mut offset = Vec::new();
    for tri in n.indices.chunks_exact(3) {
        let (p1, p2, p3) = (n.verts[tri[0]], n.verts[tri[1]], n.verts[tri[2]]);
        let mut axis0 = [p2[0] - p1[0], p2[1] - p1[1]];
        let axis0len = (axis0[0] * axis0[0] + axis0[1] * axis0[1]).sqrt();
        axis0 = [axis0[0] / axis0len, axis0[1] / axis0len];
        let mut axis1 = [p3[0] - p1[0], p3[1] - p1[1]];
        let axis1len = (axis1[0] * axis1[0] + axis1[1] * axis1[1]).sqrt();
        axis1 = [axis1[0] / axis1len, axis1[1] / axis1len];
        let rot: Mat3 = [[axis0[0], axis0[1], 0.0], [-axis0[1], axis0[0], 0.0], [0.0, 0.0, 1.0]];
        let raxis1 = m3apply(&rot, axis1);
        let (cos_a, sin_a) = (raxis1[0], raxis1[1]);
        let inv_len = |l: f32| if l > 0.0 { 1.0 / l } else { 0.0 };
        let m = m3mul(
            &m3mul(
                &m3mul(
                    &[[inv_len(axis0len), 0.0, 0.0], [0.0, inv_len(axis1len), 0.0], [0.0, 0.0, 1.0]],
                    &[[1.0, -cos_a / sin_a, 0.0], [0.0, 1.0 / sin_a, 0.0], [0.0, 0.0, 1.0]],
                ),
                &rot,
            ),
            &[[1.0, 0.0, -p1[0]], [0.0, 1.0, -p1[1]], [0.0, 0.0, 1.0]],
        );
        offset.push(m);
    }
    let width = (bounds[2].ceil() - bounds[0].floor() + 1.0) as i64;
    let height = (bounds[3].ceil() - bounds[1].floor() + 1.0) as i64;
    let mut mask = vec![0u16; (width * height).max(0) as usize];
    for (i, tri) in n.indices.chunks_exact(3).enumerate() {
        let t = [n.verts[tri[0]], n.verts[tri[1]], n.verts[tri[2]]];
        let tb = bounds_of(&t);
        let bw = (tb[2].ceil() - tb[0].floor() + 1.0) as i64;
        let bh = (tb[3].ceil() - tb[1].floor() + 1.0) as i64;
        let (top, left) = (tb[1].floor() as i64, tb[0].floor() as i64);
        for y in 0..bh {
            for x in 0..bw {
                let pt = [(left + x) as f32, (top + y) as f32];
                if in_triangle(pt, t) {
                    let (px, py) = (pt[0] - bounds[0], pt[1] - bounds[1]);
                    let idx = (py as i64) * width + px as i64;
                    if let Some(slot) = mask.get_mut(idx as usize) {
                        *slot = (i + 1) as u16;
                    }
                }
            }
        }
    }
    Precalc { bounds, width, mask, offset }
}

impl Puppet {
    pub fn from_json(j: &Value) -> Puppet {
        let mut nodes = Vec::new();
        fn walk(v: &Value, nodes: &mut Vec<Node>) -> usize {
            let tr = v.get("transform");
            let get3 = |k: &str, d: f32| {
                let f = f32s(tr.and_then(|t| t.get(k)));
                [0, 1, 2].map(|i| f.get(i).copied().unwrap_or(d))
            };
            let s = f32s(tr.and_then(|t| t.get("scale")));
            let mesh = v.get("mesh");
            let flag = |k: &str| v.get(k).and_then(Value::as_bool);
            let idx = nodes.len();
            nodes.push(Node {
                uuid: v["uuid"].as_u64().unwrap() as u32,
                ty: v.get("type").and_then(Value::as_str).unwrap_or("Node").to_owned(),
                name: v.get("name").and_then(Value::as_str).unwrap_or("").to_owned(),
                children: Vec::new(),
                enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                trans: get3("trans", 0.0),
                rot: get3("rot", 0.0),
                scale: [s.first().copied().unwrap_or(1.0), s.get(1).copied().unwrap_or(1.0)],
                verts: pairs(&f32s(mesh.and_then(|m| m.get("verts")))),
                origin: {
                    let o = f32s(mesh.and_then(|m| m.get("origin")));
                    [o.first().copied().unwrap_or(0.0), o.get(1).copied().unwrap_or(0.0)]
                },
                indices: f32s(mesh.and_then(|m| m.get("indices"))).iter().map(|&i| i as usize).collect(),
                dynamic: flag("dynamic_deformation").unwrap_or(false),
                // Missing key = false (MeshGroup.deserializeFromFghj).
                translate_children: flag("translate_children").unwrap_or(false),
                propagate: flag("propagate_meshgroup").unwrap_or(false),
            });
            if let Some(cs) = v.get("children").and_then(Value::as_array) {
                for c in cs {
                    let ci = walk(c, nodes);
                    nodes[idx].children.push(ci);
                }
            }
            idx
        }
        walk(&j["nodes"], &mut nodes);
        let by_uuid: HashMap<u32, usize> = nodes.iter().enumerate().map(|(i, n)| (n.uuid, i)).collect();

        let mut params = Vec::new();
        for p in j["param"].as_array().unwrap() {
            let vec2 = p["is_vec2"].as_bool().unwrap_or(false);
            let two = |k: &str, d: f32| {
                let f = f32s(p.get(k));
                [f.first().copied().unwrap_or(d), f.get(1).copied().unwrap_or(d)]
            };
            let axes = p["axis_points"].as_array().unwrap();
            let axis = [f32s(axes.first()), if vec2 { f32s(axes.get(1)) } else { vec![0.0] }];
            let (nx, ny) = (axis[0].len(), axis[1].len());
            let mut bindings = Vec::new();
            for b in p["bindings"].as_array().unwrap() {
                let Some(&node) = b["node"].as_u64().and_then(|u| by_uuid.get(&(u as u32))) else { continue };
                let name = b["param_name"].as_str().unwrap().to_owned();
                let set: Vec<Vec<bool>> = match b.get("isSet").and_then(Value::as_array) {
                    Some(c) => c
                        .iter()
                        .map(|r| r.as_array().unwrap().iter().map(|x| x.as_bool().unwrap_or(false)).collect())
                        .collect(),
                    None => vec![vec![true; ny]; nx],
                };
                let raw = b["values"].as_array().unwrap();
                let values = if name == "deform" {
                    let n = nodes[node].verts.len();
                    let mut v: Vec<Vec<Vec<V2>>> = raw
                        .iter()
                        .map(|c| {
                            c.as_array()
                                .unwrap()
                                .iter()
                                .map(|cell| {
                                    let pts: Vec<V2> = cell
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .map(|q| {
                                            let f = f32s(Some(q));
                                            [f[0], f[1]]
                                        })
                                        .collect();
                                    if pts.len() == n { pts } else { vec![[0.0; 2]; n] }
                                })
                                .collect()
                        })
                        .collect();
                    reinterpolate::reinterpolate(Grid { values: &mut v, set: &set, axis: [&axis[0], &axis[1]] })
                        .unwrap();
                    BValues::Deform(v)
                } else {
                    let mut v: Vec<Vec<f32>> = raw
                        .iter()
                        .map(|c| c.as_array().unwrap().iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect())
                        .collect();
                    reinterpolate::reinterpolate(Grid { values: &mut v, set: &set, axis: [&axis[0], &axis[1]] })
                        .unwrap();
                    BValues::Scalar(v)
                };
                let interp = b.get("interpolate_mode").and_then(Value::as_str).unwrap_or("Linear").to_owned();
                bindings.push(Binding { node, name, values, interp });
            }
            params.push(Param {
                uuid: p["uuid"].as_u64().unwrap() as u32,
                name: p["name"].as_str().unwrap_or("").to_owned(),
                vec2,
                min: two("min", 0.0),
                max: two("max", 1.0),
                defaults: two("defaults", 0.0),
                axis,
                bindings,
            });
        }

        let mut puppet = Puppet { filters: vec![Filter::None; nodes.len()], nodes, params, precalc: HashMap::new() };
        puppet.setup_mesh_groups();
        puppet
    }

    /// MeshGroup.precalculate + setupChild/setGroup, in tree order.
    fn setup_mesh_groups(&mut self) {
        let mut order = Vec::new();
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            order.push(i);
            stack.extend(self.nodes[i].children.iter().rev());
        }
        for &g in &order {
            let n = &self.nodes[g];
            if n.ty != "MeshGroup" || n.indices.is_empty() {
                continue;
            }
            self.precalc.insert(g, precalculate(n));
            let (dynamic, translate) = (n.dynamic, n.translate_children);
            for &c in &n.children.clone() {
                self.set_group(g, c, dynamic, translate);
            }
        }
    }

    fn set_group(&mut self, g: usize, node: usize, dynamic: bool, translate: bool) {
        let n = &self.nodes[node];
        let is_drawable = n.drawable();
        let is_group = n.ty == "MeshGroup";
        let is_composite = n.ty == "Composite" && n.propagate;
        let must_propagate = (is_drawable && !is_group) || is_composite;
        self.filters[node] = if translate || is_drawable {
            if is_drawable && dynamic { Filter::Post(g) } else { Filter::Pre(g) }
        } else {
            Filter::None
        };
        if must_propagate {
            for c in self.nodes[node].children.clone() {
                self.set_group(g, c, dynamic, translate);
            }
        }
    }

    /// Parameter values by uuid (unlisted: defaults). Returns each
    /// drawable's world-space vertices (`M * (v - origin + deform)`).
    pub fn evaluate(&self, values: &HashMap<u32, V2>) -> HashMap<u32, Evaluated> {
        self.evaluate_with(values, None)
    }

    /// The rest pose (no parameter offsets at all), with each filtered
    /// vertex's triangle chosen by Riglet's E6 rule instead of the bitmask:
    /// the lowest-index rest triangle containing it, else the nearest.
    pub fn rest_e6(&self) -> HashMap<u32, Evaluated> {
        let p = self.clone_without_bindings();
        p.evaluate_with(&HashMap::new(), Some(&E6_MARKER.with(|m| m.clone())))
    }

    fn clone_without_bindings(&self) -> Puppet {
        Puppet {
            nodes: self.nodes.clone(),
            params: Vec::new(),
            filters: self.filters.clone(),
            precalc: self.precalc.clone(),
        }
    }

    pub fn evaluate_with(&self, values: &HashMap<u32, V2>, fixed: Option<&FixedTriangles>) -> HashMap<u32, Evaluated> {
        let n = self.nodes.len();
        let mut off_t = vec![[0.0f32; 3]; n];
        let mut off_r = vec![[0.0f32; 3]; n];
        let mut off_s = vec![[1.0f32; 2]; n];
        let mut deform: Vec<Vec<V2>> = self.nodes.iter().map(|x| vec![[0.0; 2]; x.verts.len()]).collect();
        for p in &self.params {
            let v = values.get(&p.uuid).copied().unwrap_or(p.defaults);
            let norm = [0, 1].map(|k| ((v[k] - p.min[k]) / (p.max[k] - p.min[k])).clamp(0.0, 1.0));
            let find = |axis: &[f32], val: f32| -> (usize, f32) {
                for i in 0..axis.len().saturating_sub(1) {
                    if axis[i + 1] > val || i == axis.len() - 2 {
                        return (i, (val - axis[i]) / (axis[i + 1] - axis[i]));
                    }
                }
                (0, 0.0)
            };
            let (ix, ox) = find(&p.axis[0], norm[0]);
            let (iy, oy) = if p.vec2 { find(&p.axis[1], norm[1]) } else { (0, 0.0) };
            for b in &p.bindings {
                match &b.values {
                    BValues::Scalar(g) => {
                        let val = interpolate(
                            g,
                            p.vec2,
                            &b.interp,
                            ix,
                            iy,
                            ox,
                            oy,
                            |a, b, t| a * (1.0 - t) + b * t,
                            |p0, p1, p2, p3, t| reinterpolate::cubic(p0, p1, p2, p3, t),
                        );
                        let i = b.node;
                        match b.name.as_str() {
                            "transform.t.x" => off_t[i][0] += val,
                            "transform.t.y" => off_t[i][1] += val,
                            "transform.t.z" => off_t[i][2] += val,
                            "transform.r.x" => off_r[i][0] += val,
                            "transform.r.y" => off_r[i][1] += val,
                            "transform.r.z" => off_r[i][2] += val,
                            "transform.s.x" => off_s[i][0] *= val,
                            "transform.s.y" => off_s[i][1] *= val,
                            _ => {}
                        }
                    }
                    BValues::Deform(g) => {
                        let lerp = |a: &Vec<V2>, b: &Vec<V2>, t: f32| -> Vec<V2> {
                            a.iter()
                                .zip(b)
                                .map(|(a, b)| [a[0] * (1.0 - t) + b[0] * t, a[1] * (1.0 - t) + b[1] * t])
                                .collect()
                        };
                        let val = interpolate(
                            g,
                            p.vec2,
                            &b.interp,
                            ix,
                            iy,
                            ox,
                            oy,
                            |a, b, t| lerp(&a, &b, t),
                            |p0, p1, p2, p3, t| reinterpolate::cubic(p0, p1, p2, p3, t),
                        );
                        for (d, x) in deform[b.node].iter_mut().zip(val) {
                            d[0] += x[0];
                            d[1] += x[1];
                        }
                    }
                }
            }
        }

        let mut out = HashMap::new();
        let mut tri: HashMap<usize, (Vec<Mat3>, M4, M4)> = HashMap::new();
        self.update(0, &IDENTITY, &mut off_t, &off_r, &off_s, &mut deform, &mut tri, &mut out, fixed);
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &self,
        i: usize,
        parent: &M4,
        off_t: &mut [[f32; 3]],
        off_r: &[[f32; 3]],
        off_s: &[V2],
        deform: &mut [Vec<V2>],
        tri: &mut HashMap<usize, (Vec<Mat3>, M4, M4)>,
        out: &mut HashMap<u32, Evaluated>,
        fixed: Option<&FixedTriangles>,
    ) {
        let e6 = fixed.is_some_and(|f| f.contains_key(&u32::MAX));
        let fixed_here = if e6 { None } else { fixed.and_then(|f| f.get(&self.nodes[i].uuid)) };
        let n = &self.nodes[i];
        let local = |off_t: &[[f32; 3]]| {
            let t = [0, 1, 2].map(|k| n.trans[k] + off_t[i][k]);
            let r = [0, 1, 2].map(|k| n.rot[k] + off_r[i][k]);
            let s = [n.scale[0] * off_s[i][0], n.scale[1] * off_s[i][1]];
            trs(t, r, s)
        };
        // Node.preProcess: a filtered non-drawable moves its translation.
        if let Filter::Pre(g) = self.filters[i]
            && !n.drawable()
        {
            let v = [n.trans[0], n.trans[1]];
            let d = [off_t[i][0], off_t[i][1]];
            if let Some((res, _)) =
                self.filter(g, &[v], &[d], parent, tri, &mut Vec::new(), &mut Vec::new(), None, false)
            {
                off_t[i][0] = res[0][0];
                off_t[i][1] = res[0][1];
            }
        }
        let global = mul(parent, &local(off_t));
        let mut misses = Vec::new();
        let mut tris = Vec::new();
        let mut matrix = global;
        if n.drawable() {
            if let Filter::Pre(g) = self.filters[i]
                && let Some((res, m)) =
                    self.filter(g, &n.verts, &deform[i], &global, tri, &mut misses, &mut tris, fixed_here, e6)
            {
                deform[i] = res;
                if let Some(m) = m {
                    matrix = m;
                }
            }
            // Drawable.update: preProcess, then deformStack.update ->
            // refreshDeform -> updateDeform -> postProcess, all before a
            // MeshGroup builds its triangles and before any child updates.
            if let Filter::Post(g) = self.filters[i]
                && let Some((res, m)) =
                    self.filter(g, &n.verts, &deform[i], &global, tri, &mut misses, &mut tris, fixed_here, e6)
            {
                deform[i] = res;
                if let Some(m) = m {
                    matrix = m;
                }
            }
            if n.ty == "MeshGroup" && !n.indices.is_empty() {
                let pc = &self.precalc[&i];
                let cur: Vec<V2> = n.verts.iter().zip(&deform[i]).map(|(v, d)| [v[0] + d[0], v[1] + d[1]]).collect();
                let maps = n
                    .indices
                    .chunks_exact(3)
                    .zip(&pc.offset)
                    .map(|(t, off)| {
                        let (p1, p2, p3) = (cur[t[0]], cur[t[1]], cur[t[2]]);
                        let basis = [
                            [p2[0] - p1[0], p3[0] - p1[0], p1[0]],
                            [p2[1] - p1[1], p3[1] - p1[1], p1[1]],
                            [0.0, 0.0, 1.0],
                        ];
                        m3mul(&basis, off)
                    })
                    .collect();
                tri.insert(i, (maps, global, inverse(&global)));
            }
        }
        for &c in &n.children {
            self.update(c, &global, off_t, off_r, off_s, deform, tri, out, fixed);
        }
        if n.drawable() {
            if n.ty != "MeshGroup" {
                let pts = n
                    .verts
                    .iter()
                    .zip(&deform[i])
                    .map(|(v, d)| {
                        let p = apply(&matrix, [v[0] - n.origin[0] + d[0], v[1] - n.origin[1] + d[1], 0.0]);
                        [p[0], p[1]]
                    })
                    .collect();
                out.insert(n.uuid, Evaluated { positions: pts, outside: misses, triangles: tris });
            }
        }
    }

    /// MeshGroup.filterChildren.
    #[allow(clippy::type_complexity)]
    fn filter(
        &self,
        g: usize,
        verts: &[V2],
        deform: &[V2],
        transform: &M4,
        tri: &HashMap<usize, (Vec<Mat3>, M4, M4)>,
        misses: &mut Vec<usize>,
        tris: &mut Vec<i64>,
        fixed: Option<&Vec<i64>>,
        e6: bool,
    ) -> Option<(Vec<V2>, Option<M4>)> {
        let pc = self.precalc.get(&g)?;
        let (maps, forward, inverse_m) = tri.get(&g)?;
        let dynamic = self.nodes[g].dynamic;
        let center = mul(inverse_m, transform);
        let b = pc.bounds;
        let mut inv = inverse(&center);
        inv[0][3] = 0.0;
        inv[1][3] = 0.0;
        inv[2][3] = 0.0;
        let mut res = deform.to_vec();
        for (k, v) in verts.iter().enumerate() {
            // The variant (fixed triangles) uses Riglet's E6 delta rule: the
            // current local position, deltas included, goes through the map.
            let variant = fixed.is_some() || e6;
            let src = if dynamic || variant { [v[0] + deform[k][0], v[1] + deform[k][1]] } else { *v };
            let c3 = apply(&center, [src[0], src[1], 0.0]);
            let c = [c3[0], c3[1]];
            let mut index: i64 = -1;
            if b[0] <= c[0] && c[0] < b[2] && b[1] <= c[1] && c[1] < b[3] {
                let cell = ((c[1] - b[1]) as i64) * pc.width + (c[0] - b[0]) as i64;
                index = i64::from(pc.mask.get(cell as usize).copied().unwrap_or(0)) - 1;
            }
            if let Some(f) = fixed {
                index = f.get(k).copied().unwrap_or(-1);
            }
            if e6 {
                index = e6_triangle(c, &self.nodes[g].verts, &self.nodes[g].indices);
            }
            tris.push(index);
            let new = if index < 0 { c } else { m3apply(&maps[index as usize], c) };
            if index < 0 {
                misses.push(k);
            }
            if dynamic {
                res[k] = [new[0] - v[0], new[1] - v[1]];
            } else if variant {
                let cv = apply(&center, [v[0], v[1], 0.0]);
                let d = apply(&inv, [new[0] - cv[0], new[1] - cv[1], 0.0]);
                res[k] = [d[0], d[1]];
            } else {
                let d = apply(&inv, [new[0] - c[0], new[1] - c[1], 0.0]);
                res[k][0] += d[0];
                res[k][1] += d[1];
            }
        }
        Some((res, dynamic.then_some(*forward)))
    }
}

#[allow(clippy::too_many_arguments)]
fn interpolate<T: Clone>(
    g: &[Vec<T>],
    vec2: bool,
    mode: &str,
    ix: usize,
    iy: usize,
    ox: f32,
    oy: f32,
    lerp: impl Fn(T, T, f32) -> T,
    cubic: impl Fn(&T, &T, &T, &T, f32) -> T,
) -> T {
    let xlen = g.len() - 1;
    let ylen = g[0].len() - 1;
    match mode {
        "Nearest" => {
            let px = ix + usize::from(ox >= 0.5);
            let py = if vec2 { iy + usize::from(oy >= 0.5) } else { 0 };
            g[px][py].clone()
        }
        "Cubic" => {
            let xm = ix.saturating_sub(1);
            let xp = (ix + 2).min(xlen);
            if vec2 {
                let col = |y: usize| cubic(&g[xm][y], &g[ix][y], &g[ix + 1][y], &g[xp][y], ox);
                let ys: Vec<usize> = (0..4).map(|k| (iy as i64 + k - 1).clamp(0, ylen as i64) as usize).collect();
                let p: Vec<T> = ys.iter().map(|&y| col(y)).collect();
                cubic(&p[0], &p[1], &p[2], &p[3], oy)
            } else {
                cubic(&g[xm][0], &g[ix][0], &g[ix + 1][0], &g[xp][0], ox)
            }
        }
        _ => {
            let (p0, p1) = if vec2 {
                (
                    lerp(g[ix][iy].clone(), g[ix][iy + 1].clone(), oy),
                    lerp(g[ix + 1][iy].clone(), g[ix + 1][iy + 1].clone(), oy),
                )
            } else {
                (g[ix][0].clone(), g[ix + 1][0].clone())
            };
            lerp(p0, p1, ox)
        }
    }
}
