//! Resource caps for untrusted input (spec "Limits").

/// Reader caps. [`Limits::default`] holds the spec defaults; a file within
/// them must load in every conforming reader.
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
    /// Sum over mesh-deformer bindings of bound points × triangles. Caps
    /// load time against adversarial meshes (proposed for spec v0.2.1).
    pub max_mesh_binding_pairs: u64,
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
            max_mesh_binding_pairs: 1 << 28,
        }
    }
}
