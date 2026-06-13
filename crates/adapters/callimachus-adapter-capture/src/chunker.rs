/// Event parsing, filtering, grouping, and Chunk construction for capture corpora.
///
/// One Chunk is produced per distinct `(method, normalized_path)` pair ("endpoint").
/// The chunk content is a deterministic, pretty-printed JSON object describing the
/// endpoint: its call count, observed status codes, sampled request/response bodies,
/// request header key names, and sequencing context (`next_signatures`).
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use anyhow::Result;
use callimachus_core::types::{Chunk, Location};
use serde::Deserialize;

use crate::normalize::{
    endpoint_signature, is_domain_allowed, normalize_path, percent_encode, primary_domains,
};

/// Maximum captured body length before truncating with `…(truncated)`.
const MAX_BODY: usize = 8 * 1024;

// ── NetworkEvent ─────────────────────────────────────────────────────────────

/// A parsed network event from an `events.jsonl` line.
///
/// All fields beyond `kind` are optional so that unexpected shapes parse cleanly
/// instead of failing the whole file.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct NetworkEvent {
    pub kind: String,
    #[serde(rename = "type")]
    pub event_type: Option<String>,
    /// Unix milliseconds (may be fractional float in some captures).
    pub timestamp: Option<f64>,
    pub url: Option<String>,
    pub method: Option<String>,
    pub status: Option<i64>,
    pub request_headers: Option<serde_json::Value>,
    pub request_body: Option<String>,
    pub response_body: Option<String>,
    pub response_body_file: Option<String>,
    pub response_body_mime_type: Option<String>,
    pub response_body_size: Option<i64>,
}

impl NetworkEvent {
    /// Return the URL path (without query string).
    fn path(&self) -> &str {
        self.url
            .as_deref()
            .and_then(|u| {
                // Strip scheme://host prefix to get the path.
                let stripped = u
                    .trim_start_matches("https://")
                    .trim_start_matches("http://");
                stripped.find('/').map(|i| &stripped[i..])
            })
            .unwrap_or("/")
    }

    /// Return the host portion of the URL.
    fn host(&self) -> &str {
        self.url
            .as_deref()
            .map(|u| {
                let stripped = u
                    .trim_start_matches("https://")
                    .trim_start_matches("http://");
                let end = stripped.find('/').unwrap_or(stripped.len());
                // Strip port if present.
                let host_port = &stripped[..end];
                host_port.split(':').next().unwrap_or(host_port)
            })
            .unwrap_or("")
    }

    fn is_network_event(&self) -> bool {
        matches!(
            self.event_type.as_deref(),
            Some("XHR") | Some("Fetch")
        )
    }

    fn ts(&self) -> f64 {
        self.timestamp.unwrap_or(0.0)
    }
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Parse `events.jsonl` content into network events.
///
/// Blank lines and lines that fail to parse are silently skipped.
/// Only `XHR` and `Fetch` type events are returned.
pub fn parse_events(jsonl: &str) -> Vec<NetworkEvent> {
    jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<NetworkEvent>(line).ok())
        .filter(|ev| ev.is_network_event())
        .collect()
}

// ── Grouping ─────────────────────────────────────────────────────────────────

/// Accumulated data for one endpoint group (one distinct `(method, normalized_path)`).
#[derive(Default)]
struct EndpointGroup {
    signature: String,
    method: String,
    path_template: String,
    observed_paths: BTreeSet<String>,
    statuses: BTreeSet<i64>,
    // (status, body) pairs, deduped by exact content.
    response_samples: Vec<(i64, String)>,
    response_sample_set: BTreeSet<String>, // dedup key
    request_bodies: BTreeSet<String>,
    request_header_keys: BTreeSet<String>,
    content_types: BTreeSet<String>,
    next_signatures: BTreeSet<String>,
    call_count: u32,
    min_seq: usize,
    max_seq: usize,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Parse, filter, and group events into one Chunk per endpoint.
///
/// `capture_dir` is used to resolve externalized body files.
/// `meta_json` is the parsed `meta.json` (used to derive the primary-domain filter).
/// `corpus_id` is embedded in each produced Chunk.
pub fn chunk_events(
    jsonl: &str,
    corpus_id: &str,
    capture_dir: &Path,
    meta_json: &serde_json::Value,
) -> Result<Vec<Chunk>> {
    let domains = primary_domains(meta_json);

    // Parse + filter.
    let mut events: Vec<NetworkEvent> = parse_events(jsonl)
        .into_iter()
        .filter(|ev| is_domain_allowed(ev.host(), &domains))
        .collect();

    // Sort by timestamp for deterministic ordering.
    events.sort_by(|a, b| a.ts().partial_cmp(&b.ts()).unwrap_or(std::cmp::Ordering::Equal));

    // --- First pass: build groups -----------------------------------------------
    // Key: (uppercase_method, normalized_path)
    let mut order: Vec<(String, String)> = Vec::new(); // insertion order of group keys
    let mut groups: HashMap<(String, String), EndpointGroup> = HashMap::new();

    for (seq, ev) in events.iter().enumerate() {
        let method = ev.method.as_deref().unwrap_or("GET").to_uppercase();
        let raw_path = ev.path();
        let norm_path = normalize_path(raw_path);
        let sig = endpoint_signature(&method, &norm_path);
        let key = (method.clone(), norm_path.clone());

        let group = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            EndpointGroup {
                signature: sig.clone(),
                method: method.clone(),
                path_template: norm_path.clone(),
                min_seq: seq,
                max_seq: seq,
                ..Default::default()
            }
        });

        group.call_count += 1;
        group.min_seq = group.min_seq.min(seq);
        group.max_seq = group.max_seq.max(seq);

        // Observed (un-normalized) path, stripped of query string.
        let observed = raw_path.split('?').next().unwrap_or(raw_path).to_string();
        group.observed_paths.insert(observed);

        if let Some(s) = ev.status {
            group.statuses.insert(s);
        }

        // Request header keys only (drop values).
        if let Some(hdrs) = &ev.request_headers
            && let Some(obj) = hdrs.as_object()
        {
            for k in obj.keys() {
                group.request_header_keys.insert(k.clone());
            }
        }

        // Request body (deduped, truncated).
        if let Some(body) = &ev.request_body {
            let body = truncate(body);
            group.request_bodies.insert(body);
        }

        // Content-type from response headers (the network event doesn't have a
        // separate responseHeaders field in our struct, but it might be in the
        // raw response body mime type).
        if let Some(ct) = &ev.response_body_mime_type {
            group.content_types.insert(ct.clone());
        }

        // Response body / externalized body.
        let status = ev.status.unwrap_or(0);
        let body_content = resolve_body(ev, capture_dir);
        let dedup_key = format!("{status}\x00{body_content}");
        if !group.response_sample_set.contains(&dedup_key) {
            group.response_sample_set.insert(dedup_key);
            group.response_samples.push((status, body_content));
        }
    }

    // --- Second pass: compute next_signatures -----------------------------------
    // Walk chronologically-sorted events; for adjacent pairs with distinct
    // normalized signatures, record the successor signature.
    let sorted_sigs: Vec<String> = events.iter().map(|ev| {
        let method = ev.method.as_deref().unwrap_or("GET").to_uppercase();
        let norm = normalize_path(ev.path());
        endpoint_signature(&method, &norm)
    }).collect();

    for window in sorted_sigs.windows(2) {
        let (a, b) = (&window[0], &window[1]);
        if a != b {
            let a_method = a.split_once(' ').map(|(m, _)| m.to_string()).unwrap_or_default();
            let a_path = a.split_once(' ').map(|(_, p)| p.to_string()).unwrap_or_default();
            let key = (a_method, a_path);
            if let Some(group) = groups.get_mut(&key) {
                group.next_signatures.insert(b.clone());
            }
        }
    }

    // --- Build chunks -----------------------------------------------------------
    let mut chunks = Vec::new();
    for key in &order {
        let group = &groups[key];
        let chunk = build_chunk(corpus_id, group)?;
        chunks.push(chunk);
    }

    Ok(chunks)
}

// ── Body resolution ───────────────────────────────────────────────────────────

fn resolve_body(ev: &NetworkEvent, capture_dir: &Path) -> String {
    // 1. Inline body.
    if let Some(body) = &ev.response_body {
        return truncate(body);
    }
    // 2. Externalized body file.
    if let Some(file) = &ev.response_body_file {
        let path = capture_dir.join("bodies").join(file);
        // Determine if binary.
        let mime = ev.response_body_mime_type.as_deref().unwrap_or("");
        if is_binary_mime(mime) {
            let size = ev.response_body_size.unwrap_or(0);
            return format!("<binary: {mime}, {size} bytes>");
        }
        // Try to read as UTF-8 text.
        match std::fs::read_to_string(&path) {
            Ok(text) => return truncate(&text),
            Err(_) => {
                // Unreadable or binary: record placeholder.
                let size = ev.response_body_size.unwrap_or(0);
                return format!("<binary: {mime}, {size} bytes>");
            }
        }
    }
    String::new()
}

fn is_binary_mime(mime: &str) -> bool {
    let mime = mime.split(';').next().unwrap_or(mime).trim();
    // Binary if not text/* or *json or *xml or similar text-like types.
    if mime.starts_with("text/") {
        return false;
    }
    if mime.contains("json") || mime.contains("xml") || mime.contains("javascript") {
        return false;
    }
    // Treat as binary if mime is non-empty and doesn't look textual.
    !mime.is_empty()
}

fn truncate(s: &str) -> String {
    if s.len() <= MAX_BODY {
        s.to_string()
    } else {
        format!("{}…(truncated)", &s[..MAX_BODY])
    }
}

// ── Chunk construction ────────────────────────────────────────────────────────

fn build_chunk(corpus_id: &str, group: &EndpointGroup) -> Result<Chunk> {
    let response_samples: Vec<serde_json::Value> = group
        .response_samples
        .iter()
        .map(|(status, body)| {
            serde_json::json!({
                "status": status,
                "body": body,
            })
        })
        .collect();

    let content_obj = serde_json::json!({
        "signature": group.signature,
        "method": group.method,
        "path_template": group.path_template,
        "observed_paths": group.observed_paths.iter().collect::<Vec<_>>(),
        "call_count": group.call_count,
        "sequence_range": [group.min_seq, group.max_seq],
        "statuses": group.statuses.iter().collect::<Vec<_>>(),
        "request_bodies": group.request_bodies.iter().collect::<Vec<_>>(),
        "response_samples": response_samples,
        "request_headers_seen": group.request_header_keys.iter().collect::<Vec<_>>(),
        "content_types": group.content_types.iter().collect::<Vec<_>>(),
        "next_signatures": group.next_signatures.iter().collect::<Vec<_>>(),
    });

    let content = serde_json::to_string_pretty(&content_obj)?;

    // Location path: ep/{METHOD}/{percent-encoded-normalized-path}
    let location_path = format!(
        "ep/{}/{}",
        group.method,
        percent_encode(&group.path_template)
    );

    let location = Location::new(corpus_id, location_path);
    Ok(Chunk::new(
        corpus_id.to_string(),
        None,
        "endpoint".to_string(),
        location,
        content,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_events_filters_non_network() {
        let jsonl = r#"{"kind":"page","timestamp":1000,"url":"https://example.com/"}
{"kind":"network","type":"XHR","timestamp":1001,"url":"https://api.example.com/users","method":"GET","status":200}
{"kind":"input","timestamp":1002,"key":"Enter"}
{"kind":"network","type":"Fetch","timestamp":1003,"url":"https://api.example.com/posts","method":"POST","status":201}"#;

        let events = parse_events(jsonl);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type.as_deref(), Some("XHR"));
        assert_eq!(events[1].event_type.as_deref(), Some("Fetch"));
    }

    #[test]
    fn parse_events_skips_malformed_lines() {
        let jsonl = "not json\n{\"kind\":\"network\",\"type\":\"XHR\",\"url\":\"https://x.com/a\",\"method\":\"GET\"}";
        let events = parse_events(jsonl);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn host_extraction() {
        let mut ev = NetworkEvent::default();
        ev.event_type = Some("XHR".to_string());
        ev.url = Some("https://network.curaspan.com/connect/restapi/user/123".to_string());
        assert_eq!(ev.host(), "network.curaspan.com");
    }

    #[test]
    fn path_extraction_strips_query() {
        let mut ev = NetworkEvent::default();
        ev.event_type = Some("XHR".to_string());
        ev.url = Some("https://api.example.com/users/456?include=details".to_string());
        assert_eq!(ev.path(), "/users/456?include=details"); // path() returns before query strip
    }
}
