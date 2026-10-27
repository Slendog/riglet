//! Caps for untrusted input: format limits (spec "Limits") and this
//! loader's resource budgets (spec "Loader resource limits").

/// Reader caps. Every field except `binding_work_budget` is a format limit
/// with its spec default: a file within them must load in every conforming
/// reader. `binding_work_budget` is an implementation-defined loader budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub zip_max_entries: u64,
    pub zip_max_total_uncompressed: u64,
    pub zip_max_entry_uncompressed: u64,
    pub zip_max_compression_ratio: u64,
    /// Entries at or below this size skip the compression-ratio check.
    pub zip_ratio_check_min_size: u64,
    pub max_nodes: u64,
    pub max_vertices_per_mesh: u64,
    pub max_vertices_total: u64,
    /// Total decoded keyform position elements across the model.
    pub max_keyform_vertices_total: u64,
    pub max_keyform_grid: u64,
    pub max_texture_dimension: u32,
    /// Loader resource budget, not a format rule: maximum mesh-deformer
    /// binding work (candidate triangle tests plus grid cells visited, see
    /// [`crate::bind::TriangleGrid::work`]). Exceeding it fails with
    /// `LIMIT_MESH_BINDING`. 2^28 is about 2 s in release.
    pub binding_work_budget: u64,
}

const MIB: u64 = 1024 * 1024;

impl Default for Limits {
    fn default() -> Self {
        Limits {
            zip_max_entries: 10_000,
            zip_max_total_uncompressed: 2 * 1024 * MIB,
            zip_max_entry_uncompressed: 512 * MIB,
            zip_max_compression_ratio: 1000,
            zip_ratio_check_min_size: MIB,
            max_nodes: 65_536,
            max_vertices_per_mesh: 65_536,
            max_vertices_total: 4_194_304,
            max_keyform_vertices_total: 33_554_432,
            max_keyform_grid: 65_536,
            max_texture_dimension: 16_384,
            binding_work_budget: 1 << 28,
        }
    }
}
