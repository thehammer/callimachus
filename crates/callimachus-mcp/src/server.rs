use crate::{dispatch, tools};
use callimachus_core::query::QueryService;
use callimachus_core::storage::StorageBackend;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, warn};

const SERVER_NAME: &str = "callimachus";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Supported MCP protocol versions (newest first).
const SUPPORTED_VERSIONS: &[&str] = &["2025-03-26", "2024-11-05"];

/// The MCP stdio server. Exposes all 17 tools (12 corpus-scoped + 5 collection-scoped).
pub struct McpServer {
    qs: QueryService,
    backend: Arc<dyn StorageBackend>,
}

impl McpServer {
    pub fn new(qs: QueryService) -> Self {
        let backend = qs.backend();
        Self { qs, backend }
    }

    /// Run the stdio JSON-RPC loop.
    ///
    /// Reads newline-delimited JSON from stdin, writes responses to stdout,
    /// diagnostics to stderr. Runs until stdin is closed or SIGINT/SIGTERM.
    pub async fn run(&self) -> anyhow::Result<()> {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);
        let mut stdout = stdout;
        let mut line = String::new();

        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                // EOF — stdin closed.
                break;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            debug!(msg = %trimmed, "← stdin");

            let msg: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    warn!("JSON parse error: {e}");
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": {
                            "code": -32700,
                            "message": format!("Parse error: {e}")
                        }
                    });
                    write_line(&mut stdout, &resp).await?;
                    continue;
                }
            };

            let id = msg.get("id").cloned();
            let method = msg
                .get("method")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            let params = msg.get("params").cloned().unwrap_or(Value::Null);

            // Notifications (no id) — handle but don't respond.
            if id.is_none() {
                match method.as_str() {
                    "notifications/initialized" => {
                        debug!("client initialized");
                    }
                    other => {
                        debug!("unhandled notification: {other}");
                    }
                }
                continue;
            }

            let response = self.handle(&method, params, id.clone().unwrap()).await;
            debug!(resp = %response, "→ stdout");
            write_line(&mut stdout, &response).await?;
        }

        Ok(())
    }

    async fn handle(&self, method: &str, params: Value, id: Value) -> Value {
        match method {
            "initialize" => self.handle_initialize(params, id),
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            "tools/list" => self.handle_tools_list(id),
            "tools/call" => self.handle_tools_call(params, id).await,
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {other}")
                }
            }),
        }
    }

    fn handle_initialize(&self, params: Value, id: Value) -> Value {
        // Negotiate protocol version.
        let client_version = params
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let negotiated = if SUPPORTED_VERSIONS.contains(&client_version) {
            client_version
        } else {
            // Fall back to oldest supported version.
            SUPPORTED_VERSIONS.last().copied().unwrap_or("2024-11-05")
        };

        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": negotiated,
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": SERVER_VERSION
                }
            }
        })
    }

    fn handle_tools_list(&self, id: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": tools::tools_list_json()
            }
        })
    }

    async fn handle_tools_call(&self, params: Value, id: Value) -> Value {
        let name = match params.get("name").and_then(|n| n.as_str()) {
            Some(n) => n.to_string(),
            None => {
                return json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32602,
                        "message": "Missing 'name' in tools/call params"
                    }
                });
            }
        };

        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let result = dispatch::dispatch(&self.qs, &self.backend, &name, args).await;
        let is_error = result
            .get("ok")
            .and_then(|v| v.as_bool())
            .is_some_and(|ok| !ok);

        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{
                    "type": "text",
                    "text": result.to_string()
                }],
                "isError": is_error
            }
        })
    }
}

async fn write_line(stdout: &mut tokio::io::Stdout, value: &Value) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    stdout.write_all(&bytes).await?;
    stdout.flush().await?;
    Ok(())
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use callimachus_core::{
        query::QueryService,
        storage::{SqliteBackend, StorageBackend},
    };
    use std::sync::Arc;
    use tokio::io::duplex;

    fn make_server() -> McpServer {
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let qs = QueryService::new(db);
        McpServer::new(qs)
    }

    /// Drive the server with a synthetic stdin/stdout pair.
    /// Sends each request line, returns the parsed response objects.
    #[allow(dead_code)]
    async fn drive(requests: Vec<Value>) -> Vec<Value> {
        // Build input bytes.
        let mut input = Vec::<u8>::new();
        for req in &requests {
            serde_json::to_writer(&mut input, req).unwrap();
            input.push(b'\n');
        }

        // We use a channel-based approach: write all input, then close.
        let (client_tx, server_rx) = duplex(65536);
        let (server_tx, mut client_rx) = duplex(65536);

        // Write all input into client_tx then drop it (simulates EOF).
        let mut w = client_tx;
        tokio::io::AsyncWriteExt::write_all(&mut w, &input)
            .await
            .unwrap();
        drop(w);

        // Run the server in a task, reading from server_rx and writing to server_tx.
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let qs = QueryService::new(db);
        let server = McpServer::new(qs);
        let _ = tokio::spawn(async move {
            let mut reader = BufReader::new(server_rx);
            let mut line = String::new();
            let mut writer = server_tx;
            loop {
                line.clear();
                let n = reader.read_line(&mut line).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let msg: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let id = msg.get("id").cloned();
                let method = msg
                    .get("method")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                let params = msg.get("params").cloned().unwrap_or(Value::Null);

                if id.is_none() {
                    continue;
                }
                let id = id.unwrap();
                let response = server.handle(&method, params, id).await;
                let mut bytes = serde_json::to_vec(&response).unwrap();
                bytes.push(b'\n');
                tokio::io::AsyncWriteExt::write_all(&mut writer, &bytes)
                    .await
                    .unwrap();
            }
        })
        .await;

        // Read all responses.
        let mut responses = Vec::new();
        let mut reader = BufReader::new(&mut client_rx);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                        responses.push(v);
                    }
                }
            }
        }
        responses
    }

    #[tokio::test]
    async fn initialize_returns_capabilities() {
        let server = make_server();
        let resp = server.handle(
            "initialize",
            json!({ "protocolVersion": "2024-11-05", "clientInfo": { "name": "test", "version": "0.1" } }),
            json!(1),
        ).await;

        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert_eq!(resp["result"]["serverInfo"]["name"], "callimachus");
    }

    #[tokio::test]
    async fn initialize_negotiates_version() {
        let server = make_server();
        // Unsupported version → fall back to oldest.
        let resp = server
            .handle(
                "initialize",
                json!({ "protocolVersion": "9999-99-99" }),
                json!(1),
            )
            .await;
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
    }

    #[tokio::test]
    async fn tools_list_returns_all_tools() {
        let server = make_server();
        let resp = server.handle("tools/list", Value::Null, json!(1)).await;
        let tools = resp["result"]["tools"].as_array().unwrap();
        // 23 original + 2 taxonomy tools (entity_search_by_abstract_kind, list_abstract_kinds)
        assert_eq!(tools.len(), 25);
    }

    #[tokio::test]
    async fn tools_call_corpus_list_empty_db() {
        let server = make_server();
        let resp = server
            .handle(
                "tools/call",
                json!({ "name": "corpus_list", "arguments": {} }),
                json!(1),
            )
            .await;
        let content = &resp["result"]["content"][0]["text"].as_str().unwrap();
        let data: Value = serde_json::from_str(content).unwrap();
        assert_eq!(data["ok"], true);
        assert!(data["data"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_returns_error() {
        let server = make_server();
        let resp = server
            .handle(
                "tools/call",
                json!({ "name": "does_not_exist", "arguments": {} }),
                json!(1),
            )
            .await;
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let data: Value = serde_json::from_str(content).unwrap();
        assert_eq!(data["ok"], false);
        assert_eq!(resp["result"]["isError"], true);
    }

    #[tokio::test]
    async fn tools_call_invalid_json_input_returns_error() {
        let server = make_server();
        // corpus_overview requires corpus_id — pass nothing.
        let resp = server
            .handle(
                "tools/call",
                json!({ "name": "corpus_overview", "arguments": {} }),
                json!(1),
            )
            .await;
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let data: Value = serde_json::from_str(content).unwrap();
        assert_eq!(data["ok"], false);
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let server = make_server();
        let resp = server.handle("unknown/method", Value::Null, json!(1)).await;
        assert!(resp["error"]["code"].as_i64().unwrap() == -32601);
    }
}
