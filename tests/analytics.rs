#[path = "../src/analytics.rs"]
mod analytics;

use analytics::{access_key_id, hash_ip, strip_range_unit, RequestEvent, SampleConfig, Sampler};

fn event<'a>() -> RequestEvent<'a> {
    RequestEvent {
        account_id: "cholmes",
        product_id: "admin-boundaries",
        file_path: "countries.parquet",
        method: "GET",
        user_id: "",
        access_key_id: "",
        client_ip_hash: "deadbeef",
        range: "0-1023", // already stripped of the "bytes=" unit prefix
        country: "US",
        content_type: "application/octet-stream",
        bytes_sent: 1024.0,
        status_code: 200.0,
        duration_ms: 42.5,
    }
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
            "deadbeef",                 // blob8: client_ip_hash
            "0-1023",                   // blob9: range (bytes= prefix stripped)
            "",                         // blob10: access_key_id (anonymous)
        ]
    );
}

#[test]
fn doubles_in_schema_order() {
    assert_eq!(
        event().doubles(10.0),
        [
            1024.0, // double1: bytes_sent
            200.0,  // double2: status_code
            42.5,   // double3: duration_ms
            10.0,   // double4: sample weight
        ]
    );
}

// ── Sampling ────────────────────────────────────────────────────────

fn config(every: u32, burst: u32) -> SampleConfig {
    SampleConfig { every, burst }
}

/// u that always keeps / always drops a sampled event (kept when u < 1/every).
const KEEP: f64 = 0.0;
const DROP: f64 = 0.999;

#[test]
fn sampling_off_writes_everything_at_weight_one() {
    let mut s = Sampler::new(config(1, 0));
    for i in 0..100 {
        assert_eq!(s.decide(&event(), i, DROP), Some(1.0));
    }
}

#[test]
fn a_products_first_requests_in_a_window_are_written_in_full() {
    let mut s = Sampler::new(config(10, 3));
    for _ in 0..3 {
        assert_eq!(s.decide(&event(), 0, DROP), Some(1.0));
    }
    // Past the allowance, one in `every` is kept at weight `every`...
    assert_eq!(s.decide(&event(), 0, KEEP), Some(10.0));
    // ...and the rest are dropped.
    assert_eq!(s.decide(&event(), 0, DROP), None);
}

#[test]
fn the_allowance_renews_each_window() {
    let mut s = Sampler::new(config(10, 2));
    for _ in 0..2 {
        s.decide(&event(), 0, DROP);
    }
    assert_eq!(s.decide(&event(), 59_999, DROP), None);
    assert_eq!(s.decide(&event(), 60_000, DROP), Some(1.0));
}

#[test]
fn a_clock_that_steps_back_renews_the_window() {
    let mut s = Sampler::new(config(10, 1));
    s.decide(&event(), 100_000, DROP);
    assert_eq!(s.decide(&event(), 100_000, DROP), None);
    assert_eq!(s.decide(&event(), 5_000, DROP), Some(1.0));
}

#[test]
fn a_hot_product_does_not_use_up_a_quiet_ones_allowance() {
    let mut s = Sampler::new(config(10, 2));
    for _ in 0..1_000 {
        s.decide(&event(), 0, DROP);
    }
    let quiet = RequestEvent {
        product_id: "rarely-read",
        ..event()
    };
    assert_eq!(s.decide(&quiet, 0, DROP), Some(1.0));
    assert_eq!(s.decide(&quiet, 0, DROP), Some(1.0));
}

#[test]
fn errors_other_than_404_are_never_sampled() {
    let mut s = Sampler::new(config(10, 0));
    for status in [400.0, 401.0, 403.0, 412.0, 429.0, 500.0, 503.0, 520.0] {
        let e = RequestEvent {
            status_code: status,
            ..event()
        };
        assert_eq!(s.decide(&e, 0, DROP), Some(1.0), "status {status}");
    }
}

#[test]
fn a_404_is_sampled_like_any_other_read() {
    let mut s = Sampler::new(config(10, 0));
    let e = RequestEvent {
        status_code: 404.0,
        ..event()
    };
    assert_eq!(s.decide(&e, 0, DROP), None);
    assert_eq!(s.decide(&e, 0, KEEP), Some(10.0));
}

#[test]
fn signed_reads_are_never_sampled_but_signed_writes_are() {
    let mut s = Sampler::new(config(10, 0));
    for method in ["GET", "HEAD"] {
        let read = RequestEvent {
            method,
            user_id: "ory-identity-1",
            ..event()
        };
        assert_eq!(s.decide(&read, 0, DROP), Some(1.0), "{method}");
    }
    for method in ["PUT", "DELETE", "POST"] {
        let write = RequestEvent {
            method,
            user_id: "ory-identity-1",
            ..event()
        };
        assert_eq!(s.decide(&write, 0, DROP), None, "{method}");
    }
}

#[test]
fn weights_sum_to_the_true_request_count() {
    // 200k requests to one product in one window, with u from a low-discrepancy
    // sequence so the check is deterministic.
    let (n, every) = (200_000u64, 10u32);
    let mut s = Sampler::new(config(every, 5));
    let (mut weight, mut written) = (0.0, 0u64);
    for i in 0..n {
        let u = (i as f64 * 0.618_033_988_749_895).fract();
        if let Some(w) = s.decide(&event(), 0, u) {
            weight += w;
            written += 1;
        }
    }
    let error = (weight - n as f64).abs() / n as f64;
    assert!(error < 0.01, "weighted count {weight} vs {n}");
    assert!(written < n / 8, "wrote {written} of {n}");
}

#[test]
fn the_product_table_is_bounded() {
    let mut s = Sampler::new(config(10, 1));
    for i in 0..5_000 {
        let name = format!("product-{i}");
        let e = RequestEvent {
            product_id: &name,
            ..event()
        };
        s.decide(&e, 0, DROP);
    }
    // No panic, and a product seen again after a clear gets a fresh allowance.
    assert_eq!(s.decide(&event(), 0, DROP), Some(1.0));
}

#[test]
fn config_is_off_unless_set() {
    assert_eq!(SampleConfig::parse(None, None), config(1, 5));
    assert_eq!(SampleConfig::parse(Some(""), Some("")), config(1, 5));
    assert_eq!(SampleConfig::parse(Some("ten"), Some("x")), config(1, 5));
}

#[test]
fn config_reads_both_values_and_never_divides_by_zero() {
    assert_eq!(SampleConfig::parse(Some("10"), Some("3")), config(10, 3));
    assert_eq!(SampleConfig::parse(Some(" 20 "), None), config(20, 5));
    assert_eq!(SampleConfig::parse(Some("0"), Some("0")), config(1, 0));
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

// ── hash_ip ─────────────────────────────────────────────────────────

#[test]
fn hash_ip_is_deterministic_and_hex() {
    let a = hash_ip("203.0.113.7", "salt");
    assert_eq!(a, hash_ip("203.0.113.7", "salt"));
    assert_eq!(a.len(), 64); // SHA-256 hex
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn hash_ip_salt_changes_output() {
    // Same IP, different salt → different hash (salt actually participates).
    assert_ne!(hash_ip("203.0.113.7", "a"), hash_ip("203.0.113.7", "b"));
}

#[test]
fn hash_ip_empty_ip_stays_empty() {
    // Unknown client → empty, so anonymous requests don't all collapse to one
    // hash of the empty string.
    assert_eq!(hash_ip("", "salt"), "");
}

// ── strip_range_unit ────────────────────────────────────────────────

#[test]
fn strip_range_unit_drops_bytes_prefix() {
    assert_eq!(strip_range_unit("bytes=0-1023"), "0-1023");
    assert_eq!(strip_range_unit("bytes=1024-"), "1024-");
    // Prefix absent → value preserved verbatim (empty stays empty; a non-bytes
    // unit is not silently mangled).
    assert_eq!(strip_range_unit(""), "");
    assert_eq!(strip_range_unit("items=0-5"), "items=0-5");
}

// ── access_key_id ───────────────────────────────────────────────────

#[test]
fn access_key_id_from_authorization_header() {
    let auth = "AWS4-HMAC-SHA256 Credential=STSPRXYABC123/20261116/us-east-1/s3/aws4_request, \
                SignedHeaders=host;x-amz-date, Signature=deadbeef";
    assert_eq!(access_key_id(auth), "STSPRXYABC123");
}

/// The `Authorization` header is not signed, so text a client adds to it must
/// not change the recorded key: only the `Credential=` part multistore verified
/// counts.
#[test]
fn access_key_id_ignores_credential_text_outside_the_credential_part() {
    let auth = "AWS4-HMAC-SHA256 X=Credential=OTHERKEY, \
                Credential=STSPRXYABC123/20261116/us-east-1/s3/aws4_request, \
                SignedHeaders=host;x-amz-date, Signature=deadbeef";
    assert_eq!(access_key_id(auth), "STSPRXYABC123");
}

#[test]
fn access_key_id_empty_when_unsigned_or_malformed() {
    assert_eq!(access_key_id(""), "");
    assert_eq!(access_key_id("Bearer abc"), "");
}
