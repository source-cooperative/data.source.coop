//! OpenID Connect discovery document generation.

/// Generate an OpenID Connect discovery document JSON.
///
/// Cloud providers fetch this from `{issuer}/.well-known/openid-configuration`
/// to find the JWKS URI where they can retrieve the proxy's public key.
pub fn openid_configuration_json(issuer: &str, jwks_uri: &str) -> String {
    let doc = serde_json::json!({
        "issuer": issuer,
        "jwks_uri": jwks_uri,
        "response_types_supported": ["id_token"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
    });

    doc.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_doc_has_required_fields() {
        let json_str = openid_configuration_json(
            "https://proxy.example.com",
            "https://proxy.example.com/.well-known/jwks.json",
        );
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(parsed["issuer"], "https://proxy.example.com");
        assert_eq!(
            parsed["jwks_uri"],
            "https://proxy.example.com/.well-known/jwks.json"
        );
        assert_eq!(
            parsed["id_token_signing_alg_values_supported"],
            serde_json::json!(["RS256"])
        );
    }
}
