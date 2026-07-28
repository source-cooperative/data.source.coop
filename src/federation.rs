//! Backend federation: exchange the proxy's OIDC identity for a connection's
//! AWS role credentials, cached isolate-wide.
//!
//! This replaces `multistore_oidc_provider::backend_auth::AwsBackendAuth`. That
//! middleware caches credentials behind a `futures::lock::Mutex`, which
//! single-flights concurrent misses on the same role: the first caller runs
//! `AssumeRoleWithWebIdentity` while the rest `.await` the lock.
//!
//! On Cloudflare Workers, awaiting that lock is fatal. A request parked on an
//! in-memory waker has no pending I/O of its own, and the runtime cancels any
//! request whose "code has executed and no events are left in the event loop"
//! with *"your Worker's code had hung and would never generate a response"*.
//! So every request that arrived while a sibling held the lock died at ~5ms
//! with a 500 — including the ones whose credentials were already cached, since
//! the fast path takes the same lock. All of `data.source.coop`'s open data sits
//! behind one connection (one role ARN, hence one lock), so a single cold
//! isolate took out every concurrent read on it.
//!
//! The fix is to keep no async lock on the request path at all. Fresh
//! credentials are read from a plain map under a sync lock that is never held
//! across an `.await`; a miss runs its own exchange. Concurrent misses each do
//! their own `AssumeRoleWithWebIdentity` rather than queueing behind one — a
//! handful of duplicate STS calls per isolate per credential lifetime, which is
//! the price of never parking a request on another request's I/O.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{Duration, Utc};
use multistore::error::ProxyError;
use multistore::middleware::{DispatchContext, Middleware, Next};
use multistore::route_handler::HandlerAction;
use multistore_oidc_provider::exchange::aws::AwsExchange;
use multistore_oidc_provider::exchange::CredentialExchange;
use multistore_oidc_provider::jwt::JwtSigner;
use multistore_oidc_provider::{BackendCredentials, HttpExchange};

/// Treat credentials as stale this long before they actually expire, so one is
/// never handed to a backend request that outlives it. Matches multistore's own
/// refresh lead.
const REFRESH_LEAD_SECS: i64 = 60;

/// Backend option keys consumed by this middleware — stripped once resolved so
/// they never reach the store builder.
const OIDC_OPTION_KEYS: [&str; 3] = ["auth_type", "oidc_role_arn", "oidc_subject"];

type Creds = Arc<BackendCredentials>;

/// Isolate-shared credentials, keyed by [`cache_key`].
///
/// A `std::sync::Mutex` rather than an async one, deliberately: the guard is
/// only ever held for a map get/insert and never across an `.await`, which is
/// what keeps a waiting request from being cancelled as hung. Workers is
/// single-threaded, so the lock is never actually contended.
static CACHE: OnceLock<Mutex<HashMap<String, Creds>>> = OnceLock::new();

/// Cache identity for a federation exchange: the role *and* the subject the
/// assertion is minted for.
///
/// Keying on the role ARN alone (which multistore's `AwsBackendAuth` did) is
/// unsound when two connections point at the same role: the role's trust policy
/// conditions on the assertion's `sub` (`scv1:conn:{id}`), so a connection whose
/// subject that policy would *reject* could instead be served another
/// connection's cached credentials — succeeding where STS would have said no.
/// The subject is part of the identity being cached, so it belongs in the key.
fn cache_key(role_arn: &str, subject: &str) -> String {
    // `\u{1f}` (unit separator) can't occur in an ARN or in a subject, so the
    // join is unambiguous without escaping either half.
    format!("{role_arn}\u{1f}{subject}")
}

fn cache() -> std::sync::MutexGuard<'static, HashMap<String, Creds>> {
    CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        // A poisoned lock means some *other* request panicked mid-map-write.
        // The map is still structurally sound (insert/get are not interruptible
        // in a way that leaves it torn), and refusing to federate over it would
        // turn one panic into a permanently broken isolate.
        .unwrap_or_else(|e| e.into_inner())
}

/// Cached credentials for `role_arn` as `subject`, if still comfortably unexpired.
pub(crate) fn cached(role_arn: &str, subject: &str) -> Option<Creds> {
    let cutoff = Utc::now() + Duration::seconds(REFRESH_LEAD_SECS);
    cache()
        .get(&cache_key(role_arn, subject))
        .filter(|creds| creds.expiration > cutoff)
        .cloned()
}

/// Cache `creds` as the credentials for `role_arn` assumed as `subject`.
pub(crate) fn store(role_arn: &str, subject: &str, creds: Creds) {
    cache().insert(cache_key(role_arn, subject), creds);
}

/// Sanitize an OIDC subject into an AWS `RoleSessionName` (`[\w+=,.@-]{2,64}`).
///
/// The proxy's per-connection subject (`scv1:conn:{id}`) contains `:`, which STS
/// rejects. This is only a CloudTrail attribution label — the role's trust policy
/// gates on the JWT `sub`/`aud`, not the session name — so a truncation collision
/// is cosmetic. Falls back to `s3-proxy` when sanitizing leaves fewer than the two
/// characters STS requires.
pub(crate) fn session_name(subject: &str) -> String {
    let name: String = subject
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "+=,.@-_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if name.len() < 2 {
        "s3-proxy".to_string()
    } else {
        name
    }
}

/// Middleware that resolves `auth_type=oidc` backend options into real AWS
/// credentials via `AssumeRoleWithWebIdentity`.
///
/// A no-op for connections without `auth_type=oidc` (public buckets served
/// unsigned), which is every request that never reaches the exchange below.
pub(crate) struct AwsFederation<H: HttpExchange> {
    signer: JwtSigner,
    http: H,
    issuer: String,
    audience: String,
}

impl<H: HttpExchange> AwsFederation<H> {
    pub(crate) fn new(signer: JwtSigner, http: H, issuer: String, audience: String) -> Self {
        Self {
            signer,
            http,
            issuer,
            audience,
        }
    }

    /// Cached credentials for `role_arn`, minting and exchanging a fresh
    /// assertion on a miss.
    async fn credentials(&self, role_arn: &str, subject: &str) -> Result<Creds, ProxyError> {
        if let Some(creds) = cached(role_arn, subject) {
            return Ok(creds);
        }
        let token = self
            .signer
            .sign(subject, &self.issuer, &self.audience, &[])?;
        let mut exchange = AwsExchange::new(role_arn.to_string());
        // Name the assumed-role session after the subject so CloudTrail
        // attributes each exchange to the originating connection.
        exchange.session_name = session_name(subject);
        let creds: Creds = Arc::new(exchange.exchange(&self.http, &token).await?);
        store(role_arn, subject, creds.clone());
        Ok(creds)
    }
}

impl<H: HttpExchange> Middleware for AwsFederation<H> {
    async fn handle<'a>(
        &'a self,
        mut ctx: DispatchContext<'a>,
        next: Next<'a>,
    ) -> Result<HandlerAction, ProxyError> {
        // Read everything needed out of the borrowed config first, so the
        // borrow ends before the config is taken and rewritten below.
        let target = ctx.bucket_config.as_deref().and_then(|config| {
            (config.option("auth_type") == Some("oidc")).then(|| {
                (
                    config.backend_type.clone(),
                    config.option("oidc_role_arn").map(str::to_string),
                    config
                        .option("oidc_subject")
                        .unwrap_or("s3-proxy")
                        .to_string(),
                )
            })
        });
        let Some((backend_type, role_arn, subject)) = target else {
            return next.run(ctx).await;
        };

        // STS credentials only sign S3 requests.
        if backend_type != "s3" {
            return Err(ProxyError::ConfigError(format!(
                "OIDC backend auth not yet supported for backend_type '{backend_type}'"
            )));
        }
        let role_arn = role_arn.ok_or_else(|| {
            ProxyError::ConfigError(
                "auth_type=oidc requires 'oidc_role_arn' in backend_options".into(),
            )
        })?;

        let creds = self.credentials(&role_arn, &subject).await?;

        // `apply_to` sets the credential option keys and clears `skip_signature`
        // so the backend request is signed.
        let mut resolved = ctx
            .bucket_config
            .take()
            .expect("bucket_config present: `target` is Some only when it is")
            .into_owned();
        creds.apply_to(&mut resolved);
        for key in OIDC_OPTION_KEYS {
            resolved.backend_options.remove(key);
        }
        ctx.bucket_config = Some(Cow::Owned(resolved));

        next.run(ctx).await
    }
}
