use callimachus_adapter_capture::normalize::*;

// ── normalize_path ────────────────────────────────────────────────────────────

#[test]
fn enterprise_numeric_id_normalized() {
    assert_eq!(
        normalize_path("/connect/coordinatedcare/restapi/enterprise/697504401867/details"),
        "/connect/coordinatedcare/restapi/enterprise/{id}/details"
    );
}

#[test]
fn user_id_normalized() {
    assert_eq!(
        normalize_path("/connect/coordinatedcare/restapi/config/user/80163698"),
        "/connect/coordinatedcare/restapi/config/user/{id}"
    );
}

#[test]
fn facility_service_preserves_alpha_suffix() {
    assert_eq!(
        normalize_path(
            "/connect/coordinatedcare/restapi/providerprofile/provider/facilityService/3189/serviceCode/LOC"
        ),
        "/connect/coordinatedcare/restapi/providerprofile/provider/facilityService/{id}/serviceCode/LOC"
    );
}

#[test]
fn no_ids_unchanged() {
    assert_eq!(
        normalize_path("/connect/coordinatedcare/restapi/external/organizationGroups"),
        "/connect/coordinatedcare/restapi/external/organizationGroups"
    );
}

#[test]
fn query_string_stripped() {
    assert_eq!(
        normalize_path(
            "/connect/coordinatedcare/restapi/external/organizationGroups/user?type=NLA_Provider_Group"
        ),
        "/connect/coordinatedcare/restapi/external/organizationGroups/user"
    );
}

#[test]
fn short_numeric_segment_not_normalized() {
    // "123" is 3 digits — below the 4-digit threshold.
    assert_eq!(
        normalize_path("/api/v1/page/123/detail"),
        "/api/v1/page/123/detail"
    );
}

#[test]
fn four_digit_numeric_is_normalized() {
    // "3189" is exactly 4 digits — at the threshold.
    assert_eq!(
        normalize_path("/api/items/3189/info"),
        "/api/items/{id}/info"
    );
}

#[test]
fn uuid_normalized() {
    assert_eq!(
        normalize_path("/api/users/550e8400-e29b-41d4-a716-446655440000/profile"),
        "/api/users/{id}/profile"
    );
}

#[test]
fn mongo_objectid_normalized() {
    assert_eq!(
        normalize_path("/api/docs/507f1f77bcf86cd799439011/view"),
        "/api/docs/{id}/view"
    );
}

#[test]
fn alpha_segment_not_normalized() {
    // "SYSTEM", "LOC", "ORG", "View" — alpha segments must be preserved.
    let path = "/connect/coordinatedcare/restapi/config/system/entitytype/SYSTEM/key/ENABLE_MASKED_REFERRAL";
    assert_eq!(normalize_path(path), path);
}

// ── is_telemetry_host ─────────────────────────────────────────────────────────

#[test]
fn bam_nr_data_net_is_telemetry() {
    assert!(is_telemetry_host("bam.nr-data.net"));
}

#[test]
fn subdomain_nr_data_net_is_telemetry() {
    assert!(is_telemetry_host("collector.nr-data.net"));
}

#[test]
fn newrelic_is_telemetry() {
    assert!(is_telemetry_host("metrics.newrelic.com"));
}

#[test]
fn google_analytics_is_telemetry() {
    assert!(is_telemetry_host("google-analytics.com"));
    assert!(is_telemetry_host("www.google-analytics.com"));
}

#[test]
fn sentry_is_telemetry() {
    assert!(is_telemetry_host("o123456.ingest.sentry.io"));
}

#[test]
fn curaspan_is_not_telemetry() {
    assert!(!is_telemetry_host("network.curaspan.com"));
    assert!(!is_telemetry_host("curaspan.com"));
}

// ── is_domain_allowed ─────────────────────────────────────────────────────────

#[test]
fn curaspan_allowed_when_filter_set() {
    let domains = vec!["curaspan.com".to_string()];
    assert!(is_domain_allowed("network.curaspan.com", &domains));
    assert!(is_domain_allowed("api.curaspan.com", &domains));
}

#[test]
fn non_primary_domain_blocked_when_filter_set() {
    let domains = vec!["curaspan.com".to_string()];
    assert!(!is_domain_allowed("other-api.com", &domains));
}

#[test]
fn telemetry_blocked_even_when_no_filter() {
    assert!(!is_domain_allowed("bam.nr-data.net", &[]));
}

#[test]
fn all_non_telemetry_allowed_when_no_filter() {
    assert!(is_domain_allowed("api.example.com", &[]));
    assert!(is_domain_allowed("myservice.io", &[]));
}

// ── primary_domains ───────────────────────────────────────────────────────────

#[test]
fn primary_domains_from_url_filter() {
    let meta = serde_json::json!({"config": {"urlFilter": "curaspan.com"}});
    assert_eq!(primary_domains(&meta), vec!["curaspan.com"]);
}

#[test]
fn primary_domains_empty_when_no_filter() {
    assert_eq!(
        primary_domains(&serde_json::json!({})),
        Vec::<String>::new()
    );
}

#[test]
fn primary_domains_empty_when_null() {
    assert_eq!(
        primary_domains(&serde_json::Value::Null),
        Vec::<String>::new()
    );
}

// ── endpoint_signature ────────────────────────────────────────────────────────

#[test]
fn signature_uppercases_method() {
    assert_eq!(
        endpoint_signature("get", "/api/users/{id}"),
        "GET /api/users/{id}"
    );
}

#[test]
fn signature_preserves_path_case() {
    assert_eq!(
        endpoint_signature("POST", "/connect/codeList/category"),
        "POST /connect/codeList/category"
    );
}
