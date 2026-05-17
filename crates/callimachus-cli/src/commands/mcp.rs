use anyhow::Result;
use callimachus_core::{
    corrections::CorrectionsEngine,
    query::QueryService,
    storage::{SqliteBackend, StorageBackend},
};
use callimachus_mcp::McpServer;
use std::path::Path;
use std::sync::Arc;

pub async fn run(db_path: &Path) -> Result<()> {
    let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open(db_path)?);

    // Load corrections for all corpora at startup so the query service can apply
    // merge/rename/alias overlays transparently to MCP tool results.
    let corrections = CorrectionsEngine::load_all(db.as_ref())?;

    let qs = QueryService::with_corrections(db, corrections);
    let server = McpServer::new(qs);
    server.run().await
}
