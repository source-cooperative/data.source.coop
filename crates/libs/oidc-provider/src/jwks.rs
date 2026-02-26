//! JWKS response generation — expose the proxy's public key as a JWK set.

use base64::Engine;
use rsa::traits::PublicKeyParts;
use rsa::RsaPublicKey;

/// Generate a JWKS JSON response containing the proxy's RSA public key.
///
/// This is served at the JWKS URI referenced by the OIDC discovery document,
/// allowing relying parties (cloud providers) to verify JWTs signed by the proxy.
pub fn jwks_json(public_key: &RsaPublicKey, kid: &str) -> String {
    let b64 = &base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let n = public_key.n();
    let e = public_key.e();

    let n_b64 = b64.encode(n.to_bytes_be());
    let e_b64 = b64.encode(e.to_bytes_be());

    let jwks = serde_json::json!({
        "keys": [{
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "kid": kid,
            "n": n_b64,
            "e": e_b64,
        }]
    });

    jwks.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::RsaPrivateKey;

    #[test]
    fn jwks_contains_expected_fields() {
        let mut rng = rand::rngs::OsRng;
        let private_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public_key: &RsaPublicKey = private_key.as_ref();

        let json_str = jwks_json(public_key, "my-kid");
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let keys = parsed["keys"].as_array().unwrap();
        assert_eq!(keys.len(), 1);

        let key = &keys[0];
        assert_eq!(key["kty"], "RSA");
        assert_eq!(key["alg"], "RS256");
        assert_eq!(key["use"], "sig");
        assert_eq!(key["kid"], "my-kid");
        assert!(key["n"].as_str().unwrap().len() > 10);
        assert!(key["e"].as_str().unwrap().len() > 0);
    }

    #[test]
    fn jwks_roundtrips_through_rsa() {
        use rsa::BigUint;

        let mut rng = rand::rngs::OsRng;
        let private_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public_key: &RsaPublicKey = private_key.as_ref();

        let json_str = jwks_json(public_key, "k1");
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let key = &parsed["keys"][0];
        let n_b64 = key["n"].as_str().unwrap();
        let e_b64 = key["e"].as_str().unwrap();

        let b64 = &base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let n = BigUint::from_bytes_be(&b64.decode(n_b64).unwrap());
        let e = BigUint::from_bytes_be(&b64.decode(e_b64).unwrap());

        let reconstructed = RsaPublicKey::new(n, e).unwrap();
        assert_eq!(&reconstructed, public_key);
    }
}
