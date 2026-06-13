use callimachus_adapter_capture::chunker::parse_events;

/// Path to the sample fixture capture directory.
fn fixture_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample-capture")
}

// ── parse_events ─────────────────────────────────────────────────────────────

#[test]
fn parse_events_keeps_only_xhr_and_fetch() {
    // The fixture has: 1 page event, 7 XHR events
    // (organizationGroups, config/user x2, codeList, enterprise/{id}, facilityService, bam telemetry).
    // parse_events returns all XHR/Fetch; domain filtering happens later in chunk_events.
    let jsonl = std::fs::read_to_string(fixture_dir().join("events.jsonl")).unwrap();
    let events = parse_events(&jsonl);
    assert_eq!(events.len(), 7, "should parse all 7 XHR events, skipping only the page event");
    assert!(events.iter().all(|e| e.kind == "network"),
        "all parsed events should be kind=network");
}

// ── chunk_events ─────────────────────────────────────────────────────────────

#[test]
fn chunker_filters_telemetry_and_groups_duplicates() {
    use callimachus_adapter_capture::chunker::chunk_events;

    let dir = fixture_dir();
    let jsonl = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
    let meta = serde_json::json!({"config": {"urlFilter": "curaspan.com"}});

    let chunks = chunk_events(&jsonl, "test-corpus", &dir, &meta).unwrap();

    // 6 XHR events: 5 curaspan + 1 telemetry.
    // Curaspan events: 5 distinct after grouping user/{id} duplicates.
    // Expected: 5 endpoint chunks (organizationGroups, config/user/{id}, codeList/category,
    //           enterprise/{id}/details, facilityService/{id}/serviceCode/LOC).
    assert_eq!(
        chunks.len(),
        5,
        "should produce 5 endpoint chunks (1 per distinct curaspan endpoint); got {}",
        chunks.len()
    );
    assert!(
        chunks.iter().all(|c| c.kind == "endpoint"),
        "all chunks should have kind=endpoint"
    );
}

#[test]
fn chunker_normalizes_ids_in_path_template() {
    use callimachus_adapter_capture::chunker::chunk_events;

    let dir = fixture_dir();
    let jsonl = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
    let meta = serde_json::json!({"config": {"urlFilter": "curaspan.com"}});

    let chunks = chunk_events(&jsonl, "test-corpus", &dir, &meta).unwrap();

    // The config/user/80163698 endpoint should be normalized to config/user/{id}.
    let user_chunk = chunks.iter().find(|c| {
        let content: serde_json::Value = serde_json::from_str(&c.content).unwrap();
        content["path_template"]
            .as_str()
            .map(|p| p.contains("config/user/{id}"))
            .unwrap_or(false)
    });
    assert!(user_chunk.is_some(), "should find chunk with path_template containing config/user/{{id}}");

    let content: serde_json::Value =
        serde_json::from_str(&user_chunk.unwrap().content).unwrap();
    assert_eq!(
        content["call_count"].as_u64().unwrap(),
        2,
        "user/{{id}} endpoint was called twice; call_count should be 2"
    );
}

#[test]
fn chunker_request_headers_contain_only_keys() {
    use callimachus_adapter_capture::chunker::chunk_events;

    let dir = fixture_dir();
    let jsonl = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
    let meta = serde_json::json!({"config": {"urlFilter": "curaspan.com"}});

    let chunks = chunk_events(&jsonl, "test-corpus", &dir, &meta).unwrap();

    for chunk in &chunks {
        let content: serde_json::Value = serde_json::from_str(&chunk.content).unwrap();
        let keys = content["request_headers_seen"].as_array().unwrap();
        for key in keys {
            let k = key.as_str().unwrap();
            // Header keys must not contain ":", "=", or known secrets.
            assert!(!k.contains(':'), "header key should not contain ':': {k}");
            assert!(!k.contains('='), "header key should not contain '=': {k}");
            // Values like "Bearer token123" would appear if we accidentally stored values.
            assert!(!k.contains("Bearer"), "header values must not be stored: {k}");
        }
    }
}

#[test]
fn chunker_no_telemetry_in_chunks() {
    use callimachus_adapter_capture::chunker::chunk_events;

    let dir = fixture_dir();
    let jsonl = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
    let meta = serde_json::json!({"config": {"urlFilter": "curaspan.com"}});

    let chunks = chunk_events(&jsonl, "test-corpus", &dir, &meta).unwrap();

    for chunk in &chunks {
        let content: serde_json::Value = serde_json::from_str(&chunk.content).unwrap();
        let sig = content["signature"].as_str().unwrap_or("");
        assert!(
            !sig.contains("nr-data.net") && !sig.contains("bam."),
            "telemetry endpoint should not appear in chunks: {sig}"
        );
    }
}

#[test]
fn chunker_produces_sequencing_context() {
    use callimachus_adapter_capture::chunker::chunk_events;

    let dir = fixture_dir();
    let jsonl = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
    let meta = serde_json::json!({"config": {"urlFilter": "curaspan.com"}});

    let chunks = chunk_events(&jsonl, "test-corpus", &dir, &meta).unwrap();

    // At least one chunk should have non-empty next_signatures.
    let has_next = chunks.iter().any(|c| {
        let content: serde_json::Value = serde_json::from_str(&c.content).unwrap();
        content["next_signatures"]
            .as_array()
            .map(|arr| !arr.is_empty())
            .unwrap_or(false)
    });
    assert!(has_next, "at least one chunk should have next_signatures for sequencing edges");
}
