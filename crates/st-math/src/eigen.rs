//! Eigen-decomposition of a symmetric 3 x 3 matrix, for principal inertia axes.
//!
//! ADR 0008 keeps Rapier and Parry from diagonalising inertia tensors, because
//! their solver reaches the platform `acos` and `cos`. The simulation computes the
//! mass properties of a ship itself and hands Rapier the principal moments and the
//! principal frame. This module is the diagonaliser: a cyclic Jacobi method over a
//! fixed schedule of [`SWEEPS`] sweeps. It uses only `+`, `-`, `*`, `/`, `sqrt`,
//! comparisons and sorting by [`f64::total_cmp`], never a transcendental function or a
//! fused multiply-add, so the same bit pattern comes out on every platform.
//!
//! ```
//! use st_math::{eigen::symmetric_eigen3, glam::DMat3};
//!
//! // A diagonal tensor: the moments come back sorted, the frame is a right-handed basis.
//! let inertia = DMat3::from_diagonal(st_math::glam::DVec3::new(3.0, 1.0, 2.0));
//! let e = symmetric_eigen3(&inertia).expect("finite input");
//! assert_eq!(e.values.to_array(), [1.0, 2.0, 3.0]);
//! assert!((e.vectors.determinant() - 1.0).abs() < 1e-15);
//! ```

use glam::{DMat3, DVec3};

/// Number of Jacobi sweeps. Each sweep rotates the three off-diagonal pairs once. The
/// off-diagonal mass shrinks quadratically, so 12 sweeps reach round-off for any
/// well-scaled matrix. There is no convergence test, so the sequence of operations
/// depends only on this constant (and on exact zeros, which skip a rotation).
pub const SWEEPS: usize = 12;

/// The eigen-decomposition of a symmetric matrix `A`: `A = V * diag(values) * V^T`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SymmetricEigen3 {
    /// The eigenvalues in ascending order (ties keep the order of the diagonal).
    pub values: DVec3,
    /// The matching unit eigenvectors as columns. The columns are orthonormal and the
    /// matrix is a proper rotation (determinant +1).
    pub vectors: DMat3,
}

/// Decomposes the symmetric matrix `m`. Only the upper triangle is read, so a matrix
/// that is slightly asymmetric from rounding is treated as its upper-triangle
/// symmetrisation.
///
/// Returns `None` if an entry is NaN or infinite, or if the result is not finite
/// (an overflow from entries near `f64::MAX`).
///
/// ```
/// use st_math::{eigen::symmetric_eigen3, glam::DMat3};
///
/// assert!(symmetric_eigen3(&DMat3::from_diagonal([f64::NAN, 1.0, 1.0].into())).is_none());
/// ```
pub fn symmetric_eigen3(m: &DMat3) -> Option<SymmetricEigen3> {
    // Row-major working copy, symmetrised from the upper triangle.
    let c = m.to_cols_array_2d(); // c[col][row]
    let mut a = [
        [c[0][0], c[1][0], c[2][0]],
        [c[1][0], c[1][1], c[2][1]],
        [c[2][0], c[2][1], c[2][2]],
    ];
    if a.iter().flatten().any(|x| !x.is_finite()) {
        return None;
    }
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    for _ in 0..SWEEPS {
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            rotate(&mut a, &mut v, p, q);
        }
    }

    // Sort ascending by eigenvalue; ties keep index order (the sort is stable).
    let mut order = [0_usize, 1, 2];
    order.sort_by(|&i, &j| a[i][i].total_cmp(&a[j][j]));
    let values = DVec3::new(
        a[order[0]][order[0]],
        a[order[1]][order[1]],
        a[order[2]][order[2]],
    );
    let col = |k: usize| DVec3::new(v[0][order[k]], v[1][order[k]], v[2][order[k]]);
    let (c0, c1, c2) = (col(0), col(1), col(2));
    // A right-handed frame: flip the last axis if the permutation made it a reflection.
    let c2 = if c0.cross(c1).dot(c2) < 0.0 { -c2 } else { c2 };
    let vectors = DMat3::from_cols(c0, c1, c2);

    let finite = values.is_finite() && vectors.to_cols_array().iter().all(|x| x.is_finite());
    finite.then_some(SymmetricEigen3 { values, vectors })
}

/// One Jacobi rotation that zeroes `a[p][q]`, applied to `a` (as `J^T A J`) and to the
/// accumulated eigenvector matrix `v` (as `V J`).
fn rotate(a: &mut [[f64; 3]; 3], v: &mut [[f64; 3]; 3], p: usize, q: usize) {
    let apq = a[p][q];
    if apq == 0.0 {
        return;
    }
    let theta = (a[q][q] - a[p][p]) / (2.0 * apq);
    // The smaller root of t^2 + 2 theta t - 1 = 0, which keeps the rotation angle within
    // 45 degrees. For a huge theta, theta * theta overflows to infinity and t is zero.
    let sign = if theta < 0.0 { -1.0 } else { 1.0 };
    let t = sign / (theta.abs() + (theta * theta + 1.0).sqrt());
    let c = 1.0 / (t * t + 1.0).sqrt();
    let s = t * c;
    for row in a.iter_mut() {
        let (x, y) = (row[p], row[q]);
        row[p] = c * x - s * y;
        row[q] = s * x + c * y;
    }
    let (row_p, row_q) = (a[p], a[q]);
    a[p] = std::array::from_fn(|k| c * row_p[k] - s * row_q[k]);
    a[q] = std::array::from_fn(|k| s * row_p[k] + c * row_q[k]);
    for row in v.iter_mut() {
        let (x, y) = (row[p], row[q]);
        row[p] = c * x - s * y;
        row[q] = s * x + c * y;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(xx: f64, yy: f64, zz: f64, xy: f64, xz: f64, yz: f64) -> DMat3 {
        DMat3::from_cols(
            DVec3::new(xx, xy, xz),
            DVec3::new(xy, yy, yz),
            DVec3::new(xz, yz, zz),
        )
    }

    fn reconstruct(e: &SymmetricEigen3) -> DMat3 {
        e.vectors * DMat3::from_diagonal(e.values) * e.vectors.transpose()
    }

    fn close(a: &DMat3, b: &DMat3, tol: f64) -> bool {
        a.to_cols_array()
            .iter()
            .zip(b.to_cols_array())
            .all(|(x, y)| (x - y).abs() <= tol)
    }

    #[test]
    fn diagonal_input_is_sorted_and_the_frame_is_a_rotation() {
        let e = symmetric_eigen3(&DMat3::from_diagonal(DVec3::new(3.0, 1.0, 2.0))).unwrap();
        assert_eq!(e.values.to_array(), [1.0, 2.0, 3.0]);
        assert!((e.vectors.determinant() - 1.0).abs() < 1e-15);
        assert!(close(
            &reconstruct(&e),
            &DMat3::from_diagonal(DVec3::new(3.0, 1.0, 2.0)),
            1e-15
        ));
    }

    #[test]
    fn dense_matrices_reconstruct_and_have_orthonormal_columns() {
        let cases = [
            sym(4.0, 3.0, 2.0, 1.0, 0.5, -0.25),
            sym(1.0e6, 2.0e6, 3.0e6, 4.0e5, -3.0e5, 2.0e5),
            sym(1.0, 1.0, 1.0, 0.0, 0.0, 0.0),
            sym(2.0, 2.0, 2.0, 1.0, 1.0, 1.0),
            sym(1.0e-6, 5.0e-7, 2.0e-6, 1.0e-7, 2.0e-7, -1.0e-7),
        ];
        for m in cases {
            let e = symmetric_eigen3(&m).unwrap();
            let scale = m
                .to_cols_array()
                .iter()
                .fold(0.0_f64, |s, x| crate::det::max(s, x.abs()));
            assert!(close(&reconstruct(&e), &m, 1e-13 * scale), "{m:?}");
            assert!(close(
                &(e.vectors.transpose() * e.vectors),
                &DMat3::IDENTITY,
                1e-14
            ));
            assert!((e.vectors.determinant() - 1.0).abs() < 1e-14);
            assert!(e.values.x <= e.values.y && e.values.y <= e.values.z);
        }
    }

    #[test]
    fn the_upper_triangle_defines_the_matrix() {
        let a = DMat3::from_cols(
            DVec3::new(2.0, 99.0, 99.0),
            DVec3::new(1.0, 3.0, 99.0),
            DVec3::new(0.5, 0.25, 4.0),
        );
        let b = sym(2.0, 3.0, 4.0, 1.0, 0.5, 0.25);
        assert_eq!(symmetric_eigen3(&a), symmetric_eigen3(&b));
    }

    #[test]
    fn bad_input_is_none_and_never_panics() {
        assert!(symmetric_eigen3(&sym(f64::NAN, 1.0, 1.0, 0.0, 0.0, 0.0)).is_none());
        assert!(symmetric_eigen3(&sym(1.0, 1.0, 1.0, f64::INFINITY, 0.0, 0.0)).is_none());
        assert!(symmetric_eigen3(&DMat3::ZERO).is_some());
        // Entries near the top of the range: the answer may overflow, never panic.
        let _ = symmetric_eigen3(&sym(
            f64::MAX,
            f64::MAX,
            f64::MAX,
            f64::MAX,
            f64::MAX,
            f64::MAX,
        ));
    }

    /// Bit patterns computed once on the reference machine. The test runs on every CI
    /// platform, so a platform difference or a changed operation order fails it.
    #[test]
    fn golden_bit_patterns() {
        let e = symmetric_eigen3(&sym(4.0, 3.0, 2.0, 1.0, 0.5, -0.25)).unwrap();
        let bits: Vec<u64> = e
            .values
            .to_array()
            .iter()
            .chain(e.vectors.to_cols_array().iter())
            .map(|x| x.to_bits())
            .collect();
        assert_eq!(bits, GOLDEN, "{bits:#x?}");
    }

    const GOLDEN: [u64; 12] = [
        0x3ffa8884cca2b33d,
        0x400584b2f6300deb,
        0x40129b8551bf4c37,
        0xbfd6eabf3328d1fc,
        0x3fdb035214cd6a72,
        0x3feaa6aa75bd37dd,
        0xbfd7a7b4102b6c44,
        0x3fe82959e32e9946,
        0xbfe1547c7685aa4f,
        0xbfeb6ffaf12b648e,
        0xbfe00e5698638277,
        0xbfbd46c98471b9a1,
    ];
}
