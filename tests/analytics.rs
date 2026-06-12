#[path = "../src/analytics.rs"]
mod analytics;

use analytics::{extract_path_segments, RequestEvent};

fn event<'a>() -> RequestEvent<'a> {
    RequestEvent {
        account_id: "cholmes",
        product_id: "admin-boundaries",
        file_path: "countries.parquet",
        method: "GET",
        user_id: "",
        client_ip: "203.0.113.7",
        country: "US",
        content_type: "application/octet-stream",
        bytes_sent: 1024.0,
        status_code: 200.0,
        duration_ms: 42.5,
    }
}

// ── extract_path_segments ───────────────────────────────────────────

#[test]
fn segments_empty_path() {
    assert_eq!(extract_path_segments("/"), (None, None, None));
    assert_eq!(extract_path_segments(""), (None, None, None));
}

#[test]
fn segments_account_only() {
    assert_eq!(
        extract_path_segments("/cholmes"),
        (Some("cholmes"), None, None)
    );
}

#[test]
fn segments_account_product() {
    assert_eq!(
        extract_path_segments("/cholmes/admin-boundaries"),
        (Some("cholmes"), Some("admin-boundaries"), None)
    );
}

#[test]
fn segments_full_path_with_nested_key() {
    assert_eq!(
        extract_path_segments("/cholmes/admin-boundaries/dir/sub/file.parquet"),
        (
            Some("cholmes"),
            Some("admin-boundaries"),
            Some("dir/sub/file.parquet")
        )
    );
}

// ── RequestEvent schema ─────────────────────────────────────────────

#[test]
fn index_is_account_slash_product() {
    assert_eq!(event().index(), "cholmes/admin-boundaries");
}

#[test]
fn blobs_in_schema_order() {
    assert_eq!(
        event().blobs(),
        [
            "cholmes",                  // blob1: account_id
            "admin-boundaries",         // blob2: product_id
            "countries.parquet",        // blob3: file_path
            "GET",                      // blob4: method
            "",                         // blob5: user_id (anonymous)
            "US",                       // blob6: country
            "application/octet-stream", // blob7: content_type
            "203.0.113.7",              // blob8: client_ip
        ]
    );
}

#[test]
fn doubles_in_schema_order() {
    assert_eq!(
        event().doubles(),
        [
            1024.0, // double1: bytes_sent
            200.0,  // double2: status_code
            42.5,   // double3: duration_ms
        ]
    );
}

#[test]
fn file_path_blob_truncated_to_256_bytes() {
    let long_path = "a".repeat(300);
    let ev = RequestEvent {
        file_path: &long_path,
        ..event()
    };
    let blobs = ev.blobs();
    assert_eq!(blobs[2].len(), 256);
}

#[test]
fn file_path_truncation_respects_char_boundaries() {
    // 'é' is 2 bytes in UTF-8; 130 of them = 260 bytes, and byte 256 falls
    // mid-character, so truncation must back off to 255 bytes.
    let long_path = "é".repeat(130);
    let ev = RequestEvent {
        file_path: &long_path,
        ..event()
    };
    let blobs = ev.blobs();
    assert!(blobs[2].len() <= 256);
    assert!(blobs[2].chars().all(|c| c == 'é'));
}
