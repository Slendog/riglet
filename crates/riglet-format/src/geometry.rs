//! 2D math for deformers: warp bilinear map (E7), rotation frames (E5),
//! triangle binding (E6).

use crate::model::Vec2;

/// Column-major 2x2 matrix: `[col0, col1]`, so `m[1][0]` is row 0, column 1.
pub type Mat2 = [Vec2; 2];

pub fn add(a: Vec2, b: Vec2) -> Vec2 {
    [a[0] + b[0], a[1] + b[1]]
}

pub fn sub(a: Vec2, b: Vec2) -> Vec2 {
    [a[0] - b[0], a[1] - b[1]]
}

pub fn scale(a: Vec2, s: f32) -> Vec2 {
    [a[0] * s, a[1] * s]
}

pub fn cross(a: Vec2, b: Vec2) -> f32 {
    a[0] * b[1] - a[1] * b[0]
}

pub fn dot(a: Vec2, b: Vec2) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

pub fn det(m: Mat2) -> f32 {
    cross(m[0], m[1])
}

pub fn mul_vec(m: Mat2, v: Vec2) -> Vec2 {
    add(scale(m[0], v[0]), scale(m[1], v[1]))
}

pub fn mul_mat(a: Mat2, b: Mat2) -> Mat2 {
    [mul_vec(a, b[0]), mul_vec(a, b[1])]
}

pub fn inverse(m: Mat2) -> Option<Mat2> {
    let d = det(m);
    if d == 0.0 || !d.is_finite() {
        return None;
    }
    let [[a, c], [b, dd]] = m;
    Some([[dd / d, -c / d], [-b / d, a / d]])
}

/// Rotation by `degrees`; positive is clockwise on screen because model
/// space is +Y down (E5).
pub fn rotate(p: Vec2, degrees: f32) -> Vec2 {
    let (s, c) = degrees.to_radians().sin_cos();
    [c * p[0] - s * p[1], s * p[0] + c * p[1]]
}

/// Angle in radians of the rotation closest to `m` (the rotation part of
/// its polar decomposition). `None` when `m` has no defined rotation.
pub fn polar_rotation(m: Mat2) -> Option<f32> {
    let y = m[0][1] - m[1][0];
    let x = m[0][0] + m[1][1];
    if x == 0.0 && y == 0.0 {
        return None;
    }
    Some(y.atan2(x))
}

fn point(points: &[Vec2], index: usize) -> Vec2 {
    points.get(index).copied().unwrap_or([0.0, 0.0])
}

/// Bilinear warp map (E7) and its analytic Jacobian at `uv`.
/// `points` are row-major control points, `(rows + 1) * (cols + 1)` of them.
pub fn warp(points: &[Vec2], rows: u32, cols: u32, uv: Vec2) -> (Vec2, Mat2) {
    let (rows_f, cols_f) = (rows as f32, cols as f32);
    let fi = (uv[0] * cols_f).floor().clamp(0.0, (cols.max(1) - 1) as f32);
    let fj = (uv[1] * rows_f).floor().clamp(0.0, (rows.max(1) - 1) as f32);
    let s = uv[0] * cols_f - fi;
    let t = uv[1] * rows_f - fj;
    let (i, j) = (fi as usize, fj as usize);
    let stride = cols as usize + 1;
    let p00 = point(points, j * stride + i);
    let p10 = point(points, j * stride + i + 1);
    let p01 = point(points, (j + 1) * stride + i);
    let p11 = point(points, (j + 1) * stride + i + 1);

    let pos = add(
        add(scale(p00, (1.0 - s) * (1.0 - t)), scale(p10, s * (1.0 - t))),
        add(scale(p01, (1.0 - s) * t), scale(p11, s * t)),
    );
    let d_du = scale(add(scale(sub(p10, p00), 1.0 - t), scale(sub(p11, p01), t)), cols_f);
    let d_dv = scale(add(scale(sub(p01, p00), 1.0 - s), scale(sub(p11, p10), s)), rows_f);
    (pos, [d_du, d_dv])
}

pub fn triangle(points: &[Vec2], indices: &[u32], tri: usize) -> [Vec2; 3] {
    let corner = |k: usize| {
        let index = indices.get(tri * 3 + k).copied().unwrap_or(0) as usize;
        point(points, index)
    };
    [corner(0), corner(1), corner(2)]
}

/// Twice the signed area.
pub fn doubled_area(t: [Vec2; 3]) -> f32 {
    cross(sub(t[1], t[0]), sub(t[2], t[0]))
}

/// Unclamped barycentric coordinates of `p`. `None` for a degenerate triangle.
pub fn barycentric(t: [Vec2; 3], p: Vec2) -> Option<[f32; 3]> {
    let d = doubled_area(t);
    if d == 0.0 || !d.is_finite() {
        return None;
    }
    let l1 = cross(sub(p, t[0]), sub(t[2], t[0])) / d;
    let l2 = cross(sub(t[1], t[0]), sub(p, t[0])) / d;
    Some([1.0 - l1 - l2, l1, l2])
}

pub fn from_barycentric(t: [Vec2; 3], l: [f32; 3]) -> Vec2 {
    add(add(scale(t[0], l[0]), scale(t[1], l[1])), scale(t[2], l[2]))
}

/// Affine map that takes `rest` onto `current`: `p -> current` with the
/// same barycentric coordinates. Returns `p` unchanged for a degenerate
/// rest triangle.
pub fn triangle_map(rest: [Vec2; 3], current: [Vec2; 3], p: Vec2) -> Vec2 {
    match barycentric(rest, p) {
        Some(l) => from_barycentric(current, l),
        None => p,
    }
}

/// Linear part of [`triangle_map`].
pub fn triangle_jacobian(rest: [Vec2; 3], current: [Vec2; 3]) -> Option<Mat2> {
    let r = [sub(rest[1], rest[0]), sub(rest[2], rest[0])];
    let c = [sub(current[1], current[0]), sub(current[2], current[0])];
    Some(mul_mat(c, inverse(r)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec2, b: Vec2) -> bool {
        (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4
    }

    #[test]
    fn rotation_is_clockwise_on_screen() {
        // +Y down: rotating +X by +90 degrees points down the screen (+Y).
        assert!(close(rotate([1.0, 0.0], 90.0), [0.0, 1.0]));
    }

    #[test]
    fn polar_rotation_ignores_shear_and_scale() {
        let r = |deg: f32| {
            let (s, c) = deg.to_radians().sin_cos();
            [[c, s], [-s, c]]
        };
        let shear: Mat2 = [[1.0, 0.0], [0.5, 1.0]];
        let m = mul_mat(r(30.0), mul_mat([[3.0, 0.0], [0.0, 3.0]], [[1.0, 0.0], [0.0, 1.0]]));
        assert!((polar_rotation(m).unwrap_or(0.0).to_degrees() - 30.0).abs() < 1e-3);
        // Symmetric shear has no rotation part.
        let sym: Mat2 = [[1.0, 0.5], [0.5, 1.0]];
        assert!(polar_rotation(sym).unwrap_or(1.0).abs() < 1e-6);
        assert!(polar_rotation(shear).is_some());
        assert_eq!(polar_rotation([[0.0, 0.0], [0.0, 0.0]]), None);
    }

    #[test]
    fn warp_jacobian_matches_finite_difference() {
        let pts = [[0.0, 0.0], [10.0, 1.0], [2.0, 8.0], [13.0, 12.0]];
        let uv = [0.3, 0.6];
        let (_, j) = warp(&pts, 1, 1, uv);
        let h = 1e-3;
        let (a, _) = warp(&pts, 1, 1, [uv[0] + h, uv[1]]);
        let (b, _) = warp(&pts, 1, 1, [uv[0] - h, uv[1]]);
        let fd = scale(sub(a, b), 0.5 / h);
        assert!((fd[0] - j[0][0]).abs() < 1e-2 && (fd[1] - j[0][1]).abs() < 1e-2);
    }
}
