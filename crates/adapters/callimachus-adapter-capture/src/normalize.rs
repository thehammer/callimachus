// Pure path-normalization and domain-filter helpers for the capture adapter.
//
// Location scheme: `ep/{METHOD}/{percent-encoded-normalized-path}`
// Example: `ep/GET/%2Fconnect%2Fcoordinatedcare%2Frestapi%2Fconfig%2Fuser%2F{id}`

// ── Path normalization ────────────────────────────────────────────────────────

/// Normalize a URL path by replacing ID-like segments with `{id}`.
///
/// A segment is treated as an ID if it is:
/// - All ASCII digits and at least 4 characters long (`697504401867`, `3189`, `80163698`).
/// - A UUID (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`).
/// - A 24-character lowercase hex string (Mongo ObjectId).
///
/// Short alpha/mixed segments (`LOC`, `SYSTEM`, `restapi`, `details`) are left unchanged.
/// Query strings and fragments are stripped before normalization.
pub fn normalize_path(path: &str) -> String {
    // Strip query string and fragment.
    let path = path.split('?').next().unwrap_or(path);
    let path = path.split('#').next().unwrap_or(path);

    path.split('/')
        .map(|seg| {
            if is_id_segment(seg) {
                "{id}".to_string()
            } else {
                seg.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn is_id_segment(seg: &str) -> bool {
    if seg.is_empty() {
        return false;
    }
    // All-digits, 4+ chars: 3189, 80163698, 697504401867.
    if seg.len() >= 4 && seg.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    // UUID: 8-4-4-4-12 lowercase hex chars separated by hyphens.
    if is_uuid(seg) {
        return true;
    }
    // 24-char hex (Mongo-style ObjectId).
    if seg.len() == 24 && seg.bytes().all(|b| b.is_ascii_hexdigit()) {
        return true;
    }
    false
}

fn is_uuid(seg: &str) -> bool {
    let parts: Vec<&str> = seg.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let expected_lens = [8usize, 4, 4, 4, 12];
    for (part, &len) in parts.iter().zip(expected_lens.iter()) {
        if part.len() != len {
            return false;
        }
        if !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
    }
    true
}

// ── Domain filtering ──────────────────────────────────────────────────────────

/// Derive allowed host-suffix patterns from `meta.config.urlFilter`.
///
/// Returns a list of domain suffixes (e.g. `["curaspan.com"]`).
/// An empty list means "no domain filter — accept all non-telemetry hosts".
pub fn primary_domains(meta: &serde_json::Value) -> Vec<String> {
    if let Some(filter) = meta
        .get("config")
        .and_then(|c| c.get("urlFilter"))
        .and_then(|v| v.as_str())
        && !filter.is_empty()
    {
        return vec![filter.to_string()];
    }
    vec![]
}

/// Return true if `host` is in the telemetry/analytics denylist.
///
/// Denylisted domains are never indexed regardless of the `urlFilter`.
pub fn is_telemetry_host(host: &str) -> bool {
    const EXACT: &[&str] = &[
        "bam.nr-data.net",
        "google-analytics.com",
        "www.google-analytics.com",
        "stats.g.doubleclick.net",
    ];
    const SUFFIX: &[&str] = &[
        ".nr-data.net",
        ".newrelic.com",
        ".googletagmanager.com",
        ".doubleclick.net",
        ".segment.io",
        ".sentry.io",
        ".datadoghq.com",
    ];

    if EXACT.contains(&host) {
        return true;
    }
    SUFFIX.iter().any(|s| host.ends_with(s))
}

/// Return true if `host` is allowed given `primary_domains`.
///
/// A host is allowed when:
/// - It is NOT in the telemetry denylist, AND
/// - Either `primary_domains` is empty, OR the host matches (equals or ends with) one of the
///   primary domain suffixes.
pub fn is_domain_allowed(host: &str, primary_domains: &[String]) -> bool {
    if is_telemetry_host(host) {
        return false;
    }
    if primary_domains.is_empty() {
        return true;
    }
    primary_domains.iter().any(|d| {
        host == d.as_str() || host.ends_with(&format!(".{d}"))
    })
}

// ── Signature ─────────────────────────────────────────────────────────────────

/// Build an endpoint signature: `"GET /connect/.../user/{id}"`.
pub fn endpoint_signature(method: &str, normalized_path: &str) -> String {
    format!("{} {}", method.to_uppercase(), normalized_path)
}

// ── Location encoding ─────────────────────────────────────────────────────────

/// Percent-encode a string for use as a location path segment.
///
/// Leaves unreserved ASCII chars (A-Z, a-z, 0-9, `-`, `_`, `.`, `~`) unescaped;
/// encodes everything else (including `/`, `{`, `}`).
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3 / 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
                out.push(char::from_digit((b & 0xf) as u32, 16).unwrap().to_ascii_uppercase());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_enterprise_id() {
        assert_eq!(
            normalize_path(
                "/connect/coordinatedcare/restapi/enterprise/697504401867/details"
            ),
            "/connect/coordinatedcare/restapi/enterprise/{id}/details"
        );
    }

    #[test]
    fn normalize_user_id() {
        assert_eq!(
            normalize_path("/connect/coordinatedcare/restapi/config/user/80163698"),
            "/connect/coordinatedcare/restapi/config/user/{id}"
        );
    }

    #[test]
    fn normalize_facility_service_preserves_alpha_suffix() {
        assert_eq!(
            normalize_path(
                "/connect/coordinatedcare/restapi/providerprofile/provider/facilityService/3189/serviceCode/LOC"
            ),
            "/connect/coordinatedcare/restapi/providerprofile/provider/facilityService/{id}/serviceCode/LOC"
        );
    }

    #[test]
    fn normalize_no_ids() {
        assert_eq!(
            normalize_path(
                "/connect/coordinatedcare/restapi/external/organizationGroups"
            ),
            "/connect/coordinatedcare/restapi/external/organizationGroups"
        );
    }

    #[test]
    fn normalize_strips_query_string() {
        assert_eq!(
            normalize_path("/connect/coordinatedcare/restapi/config/user/80163698?foo=bar"),
            "/connect/coordinatedcare/restapi/config/user/{id}"
        );
    }

    #[test]
    fn normalize_short_numeric_not_id() {
        // 3-digit numeric should NOT be normalized (< 4 chars).
        let path = "/api/v1/page/123/detail";
        let norm = normalize_path(path);
        assert_eq!(norm, "/api/v1/page/123/detail");
    }

    #[test]
    fn normalize_uuid() {
        let path = "/api/users/550e8400-e29b-41d4-a716-446655440000/profile";
        let norm = normalize_path(path);
        assert_eq!(norm, "/api/users/{id}/profile");
    }

    #[test]
    fn normalize_24char_hex() {
        let path = "/api/docs/507f1f77bcf86cd799439011/view";
        let norm = normalize_path(path);
        assert_eq!(norm, "/api/docs/{id}/view");
    }

    #[test]
    fn telemetry_filter_nr_data() {
        assert!(is_telemetry_host("bam.nr-data.net"));
        assert!(is_telemetry_host("foo.nr-data.net"));
        assert!(is_telemetry_host("collector.newrelic.com"));
    }

    #[test]
    fn telemetry_filter_allows_curaspan() {
        assert!(!is_telemetry_host("network.curaspan.com"));
        assert!(!is_telemetry_host("curaspan.com"));
    }

    #[test]
    fn domain_allowed_with_filter() {
        let domains = vec!["curaspan.com".to_string()];
        assert!(is_domain_allowed("network.curaspan.com", &domains));
        assert!(is_domain_allowed("curaspan.com", &domains));
        assert!(!is_domain_allowed("other.com", &domains));
        assert!(!is_domain_allowed("bam.nr-data.net", &domains));
    }

    #[test]
    fn domain_allowed_without_filter() {
        assert!(is_domain_allowed("network.curaspan.com", &[]));
        assert!(!is_domain_allowed("bam.nr-data.net", &[]));
    }

    #[test]
    fn primary_domains_from_meta() {
        let meta = serde_json::json!({"config": {"urlFilter": "curaspan.com"}});
        assert_eq!(primary_domains(&meta), vec!["curaspan.com"]);
    }

    #[test]
    fn primary_domains_empty_meta() {
        let meta = serde_json::json!({});
        assert_eq!(primary_domains(&meta), Vec::<String>::new());
    }

    #[test]
    fn endpoint_signature_uppercases_method() {
        assert_eq!(
            endpoint_signature("get", "/api/users/{id}"),
            "GET /api/users/{id}"
        );
    }

    #[test]
    fn percent_encode_path() {
        let encoded = percent_encode("/connect/user/{id}");
        assert_eq!(encoded, "%2Fconnect%2Fuser%2F%7Bid%7D");
    }
}
