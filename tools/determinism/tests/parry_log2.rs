//! The accepted residual of ADR 0008: Parry's BVH calls the platform `log2`.
//!
//! Parry's `Bvh::optimization_config` runs on every physics step (Rapier's broad phase calls
//! it). It computes how many subtrees to rebuild from the leaf count `n` with
//! `target_root_node_count.log2()` and `target_subtree_leaf_count.log2()`, and `f64::log2` goes
//! to the platform libm, which differs from `libm::log2` for about 0.1 % of integer inputs
//! (ADR 0008, "Facts verified"). The only thing Parry derives from those values is an
//! integer, `target_optimized_subtree_count`, and this test checks that for every leaf count
//! from 1 to 1,048,576 (the cap on colliders per system world) that integer is the same with
//! the platform `log2` and with `libm::log2`. A platform whose libm gives a different integer
//! for any count fails the build, on every CI target (wasm32 through `wasm32-wasip1`).
//!
//! The function below repeats the arithmetic of `optimization_config` in `parry3d-f64` 0.31.1
//! (`src/partitioning/bvh/bvh_optimize.rs`). The upgrade checklist of ADR 0008 re-reads that
//! function whenever `Cargo.lock` moves Parry.

/// The largest leaf count a system world may reach: `st_sim::MAX_SHIPS_PER_SYSTEM`.
const MAX_LEAVES: u32 = 1_048_576;

/// What Parry derives from `num_leaves`: the number of subtrees to optimise, before and
/// after the `max(1)` that applies when the root is skipped. `log2` is the function under
/// test.
fn derived(num_leaves: u32, log2: impl Fn(f64) -> f64) -> (usize, usize) {
    const TARGET_REBUILD_NODE_PERCENTAGE: u32 = 5;
    let num_optimized_leaves = (num_leaves * TARGET_REBUILD_NODE_PERCENTAGE).div_ceil(100);
    let num_leaves_sqrt = f64::from(num_leaves).sqrt();
    let target_root_node_count = num_leaves_sqrt.ceil();
    let target_subtree_leaf_count = (num_leaves_sqrt * 4.0).ceil();
    let root_refinement_cost = target_root_node_count * log2(target_root_node_count)
        / (target_subtree_leaf_count * log2(target_subtree_leaf_count));
    let rounded = (f64::from(num_optimized_leaves) / target_subtree_leaf_count
        - root_refinement_cost)
        .round();
    // Parry writes `.max(0.0) as usize`; for a non-negative result the two are the same, and a
    // negative one casts to 0 either way.
    let count = if rounded > 0.0 { rounded as usize } else { 0 };
    (count, count.max(1))
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "compares platform log2 with libm: ADR 0008"
)]
fn parry_bvh_log2_domain() {
    let mut mismatches = Vec::new();
    for n in 1..=MAX_LEAVES {
        let platform = derived(n, f64::log2);
        let software = derived(n, libm::log2);
        if platform != software {
            mismatches.push((n, platform, software));
        }
    }
    assert!(
        mismatches.is_empty(),
        "Parry would optimise a different number of subtrees for {} leaf counts, the first \
         ones (count, platform, libm): {:?}",
        mismatches.len(),
        &mismatches[..mismatches.len().min(8)]
    );
}

#[test]
fn the_derived_counts_have_the_shape_parry_relies_on() {
    // Sanity of the replica: one leaf optimises nothing but the skip rule makes it one, and
    // the count grows with the tree.
    assert_eq!(derived(1, libm::log2), (0, 1));
    let small = derived(1_000, libm::log2).1;
    let large = derived(MAX_LEAVES, libm::log2).1;
    assert!(small >= 1 && large > small, "{small} {large}");
}
