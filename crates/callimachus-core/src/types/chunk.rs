use crate::types::location::Location;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    /// Content-addressed ID: sha256 hex of the content string.
    pub id: String,
    pub corpus_id: String,
    /// URI of the parent chunk, if any (e.g. chapter contains scenes).
    pub parent_path: Option<String>,
    /// Adapter-defined kind: "chapter", "scene", "function", "class", "page", etc.
    pub kind: String,
    pub location: Location,
    pub content: String,
    pub byte_length: usize,
    pub created_at: String,
    /// SHA-256 of the source file that produced this chunk.  Written by
    /// chunk_pass after Stage 0 runs; `None` for chunks written before this
    /// feature was introduced.
    #[serde(default)]
    pub source_hash: Option<String>,
    /// Corpus version reference at which this chunk was first produced.
    #[serde(default)]
    pub introduced_at_version: Option<String>,
    /// Corpus version reference at which this chunk's source file was last
    /// seen dirty (i.e. was processed in a change-detection run).
    #[serde(default)]
    pub last_modified_at_version: Option<String>,
    /// `sha256_hex(sorted(entity_ids in this file))` — the file-grain shape
    /// hash used as the Layer-2 cache invalidation boundary. Empty string until
    /// the structure pass populates it.
    #[serde(default)]
    pub file_shape_hash: String,
    /// The sorted JSON array of entity IDs that `file_shape_hash` is computed
    /// from, retained for debuggability. Defaults to `"[]"`.
    #[serde(default = "default_entity_id_list")]
    pub entity_id_list: String,
}

fn default_entity_id_list() -> String {
    "[]".to_string()
}

impl Chunk {
    pub fn new(
        corpus_id: String,
        parent_path: Option<String>,
        kind: String,
        location: Location,
        content: String,
    ) -> Self {
        let byte_length = content.len();
        let id = hash_content(&content);
        Self {
            id,
            corpus_id,
            parent_path,
            kind,
            location,
            byte_length,
            content,
            created_at: chrono::Utc::now().to_rfc3339(),
            source_hash: None,
            introduced_at_version: None,
            last_modified_at_version: None,
            file_shape_hash: String::new(),
            entity_id_list: default_entity_id_list(),
        }
    }
}

pub fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}
