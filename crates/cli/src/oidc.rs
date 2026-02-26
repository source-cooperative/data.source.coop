use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use tokio::net::TcpListener;
use url::Url;

#[derive(Debug)]
pub struct OidcEndpoints {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
}

/// Fetch OIDC discovery document and extract endpoints.
pub async fn discover(issuer: &str) -> Result<OidcEndpoints, String> {
    let discovery_url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );

    let resp = reqwest::get(&discovery_url)
        .await
        .map_err(|e| format!("Failed to fetch OIDC discovery document: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("OIDC discovery returned status {}", resp.status()));
    }

    let doc: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse OIDC discovery document: {e}"))?;

    let authorization_endpoint = doc["authorization_endpoint"]
        .as_str()
        .ok_or("Missing authorization_endpoint in discovery document")?
        .to_string();

    let token_endpoint = doc["token_endpoint"]
        .as_str()
        .ok_or("Missing token_endpoint in discovery document")?
        .to_string();

    Ok(OidcEndpoints {
        authorization_endpoint,
        token_endpoint,
    })
}

/// Run the browser-based OAuth2 Authorization Code flow with PKCE.
/// Opens the user's browser to the OIDC provider, waits for the callback,
/// and returns the `id_token`.
pub async fn login(
    endpoints: &OidcEndpoints,
    client_id: &str,
    scope: &str,
    port: u16,
) -> Result<String, String> {
    let pkce = generate_pkce();
    let state: String = URL_SAFE_NO_PAD.encode(rand::thread_rng().gen::<[u8; 16]>());

    // Bind local callback server
    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .map_err(|e| format!("Failed to bind local server: {e}"))?;

    let local_addr = listener
        .local_addr()
        .map_err(|e| format!("Failed to get local address: {e}"))?;
    let redirect_uri = format!("http://127.0.0.1:{}/callback", local_addr.port());

    // Build authorization URL
    let mut auth_url = Url::parse(&endpoints.authorization_endpoint)
        .map_err(|e| format!("Invalid authorization endpoint URL: {e}"))?;
    auth_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", scope)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state);

    eprintln!("Opening browser for authentication...");
    if open::that(auth_url.as_str()).is_err() {
        eprintln!(
            "Could not open browser automatically. Please open this URL:\n{}",
            auth_url
        );
    }

    // Wait for callback
    let (code, received_state) = wait_for_callback(&listener).await?;

    if received_state != state {
        return Err("State mismatch — possible CSRF attack".to_string());
    }

    // Exchange code for tokens
    exchange_code(
        &endpoints.token_endpoint,
        &code,
        &redirect_uri,
        client_id,
        &pkce.verifier,
    )
    .await
}

/// Accept a single HTTP request on the callback listener, extract `code` and `state`.
async fn wait_for_callback(listener: &TcpListener) -> Result<(String, String), String> {
    let (stream, _) = listener
        .accept()
        .await
        .map_err(|e| format!("Failed to accept callback connection: {e}"))?;

    let std_stream = stream
        .into_std()
        .map_err(|e| format!("Failed to convert stream: {e}"))?;
    std_stream
        .set_nonblocking(false)
        .map_err(|e| format!("Failed to set blocking: {e}"))?;

    let mut reader = BufReader::new(&std_stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| format!("Failed to read request: {e}"))?;

    let path = request_line
        .split_whitespace()
        .nth(1)
        .ok_or("Invalid HTTP request")?;

    let url = Url::parse(&format!("http://localhost{path}"))
        .map_err(|e| format!("Failed to parse callback URL: {e}"))?;

    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();

    if let Some(error) = params.get("error") {
        let desc = params
            .get("error_description")
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        let html = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
             <html><body><h1>Authentication Failed</h1><p>{error}{desc}</p>\
             <p>You can close this tab.</p></body></html>"
        );
        let _ = (&std_stream).write_all(html.as_bytes());
        return Err(format!("Authentication error: {error}{desc}"));
    }

    let code = params
        .get("code")
        .ok_or("No authorization code in callback")?
        .clone();
    let received_state = params.get("state").ok_or("No state in callback")?.clone();

    let html = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
        <html><body><h1>Authentication Successful</h1>\
        <p>You can close this tab and return to your terminal.</p></body></html>";
    (&std_stream)
        .write_all(html.as_bytes())
        .map_err(|e| format!("Failed to send response: {e}"))?;

    Ok((code, received_state))
}

/// Exchange authorization code for tokens, return the `id_token`.
async fn exchange_code(
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    code_verifier: &str,
) -> Result<String, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .map_err(|e| format!("Token exchange request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Token exchange failed (HTTP {status}): {body}"));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {e}"))?;

    body["id_token"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "No id_token in token response".to_string())
}

struct Pkce {
    verifier: String,
    challenge: String,
}

fn generate_pkce() -> Pkce {
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.gen()).collect();
    let verifier = URL_SAFE_NO_PAD.encode(&bytes);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

    Pkce {
        verifier,
        challenge,
    }
}
