//! Snapshot-bound structural analysis contracts. Counts describe only the analyzed corpus.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blob {
    pub path: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StructureLimits {
    pub max_files: usize,
    pub max_blob_bytes: usize,
    pub max_entities_per_file: usize,
    pub max_visited: usize,
    pub max_paths: usize,
    pub max_results: usize,
    pub max_depth: usize,
    pub parse_budget_ms: u64,
}
impl Default for StructureLimits {
    fn default() -> Self {
        Self {
            max_files: 1000,
            max_blob_bytes: 512 * 1024,
            max_entities_per_file: 500,
            max_visited: 200,
            max_paths: 20,
            max_results: 200,
            max_depth: 4,
            parse_budget_ms: 250,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructureInput {
    pub repository: String,
    pub base_sha: String,
    pub head_sha: String,
    /// Pinned tree blobs, not working-tree paths. Callers must supply the complete intended corpus.
    pub head_blobs: Vec<Blob>,
    /// Pinned comparison-base blobs required to map deletions.
    pub base_blobs: Vec<Blob>,
    pub changed_files: Vec<super::ChangedFile>,
    pub limits: StructureLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub qualified_name: String,
    pub kind: String,
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub snapshot_sha: String,
    pub ambiguous: bool,
    pub public: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    pub path: String,
    pub line: u32,
    pub snapshot_sha: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub site: Site,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityPath {
    /// Ordered from the changed entity to its dependent; edges are reversed dependency edges.
    pub entities: Vec<Entity>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityResult {
    pub entity: Entity,
    pub anchor: Option<Site>,
    pub anchor_reason: Option<String>,
    pub resolved_dependants: Option<usize>,
    /// Distinct direct callers in the admitted HEAD graph.
    #[serde(default)]
    pub direct_dependants: Option<usize>,
    /// Distinct callers discovered through the depth frontier; unknown after visit/cancellation truncation.
    #[serde(default)]
    pub within_depth_dependants: Option<usize>,
    /// Unknown if any reachable caller may have been omitted.
    #[serde(default)]
    pub reachable_dependants: Option<usize>,
    #[serde(default)]
    pub depth_truncated: bool,
    #[serde(default)]
    pub visits_truncated: bool,
    pub direct_callees: Vec<Edge>,
    /// Target entities corresponding to direct_callees, embedded for persisted evidence.
    #[serde(default)]
    pub direct_callee_entities: Vec<Entity>,
    pub dependant_paths: Vec<EntityPath>,
    pub relationships_unavailable: Option<String>,
    pub complete: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusCoverage {
    pub analyzed_files: usize,
    pub analyzed_entities: usize,
    pub skipped_files: Vec<String>,
    pub unresolved_sites: Vec<Site>,
    pub unmapped_changed_rows: Vec<Site>,
    pub omitted_files: Option<usize>,
    pub omitted_results: usize,
    pub complete: bool,
    pub limitations: Vec<String>,
    pub parser_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructureOutput {
    pub repository: String,
    pub base_sha: String,
    pub head_sha: String,
    pub limits_digest: String,
    pub entities: Vec<Entity>,
    pub edges: Vec<Edge>,
    pub results: Vec<EntityResult>,
    pub coverage: CorpusCoverage,
}
