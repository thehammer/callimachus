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
        }
    }
}

pub fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}
