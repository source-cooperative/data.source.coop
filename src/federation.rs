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
//! The fix is to keep no async lock on the request path at all. Credentials
//! live in a plain map under a sync lock that is never held across an `.await`,
//! and a request that has nothing usable runs its own exchange rather than
//! queueing behind one.
//!
//! That leaves a duplicate-work window, which [`lookup`] narrows to the case
//! where duplication is unavoidable. A *renewal* is single-flighted without
//! anyone blocking: the credential is still valid inside the refresh lead, so
//! one request claims the exchange while the rest keep serving what they
//! already have. Latecomers never need the refresher's result, which is exactly
//! why this works where a lock cannot.
//!
//! Only a genuinely empty or expired entry makes concurrent requests each
//! exchange, with no cap beyond how many arrive before the first one stores.
//! Collapsing *those* would mean waiting, and the one way to wait on Workers
//! without being cancelled is polling a real timer — which costs about as long
//! as the exchange it avoids, while adding a spin loop and a dependency on the
//! claimant surviving.
//!
//! One cold isolate's burst is small. A deploy or mass eviction is the case to
//! watch: it cools every isolate at once, so the bursts coincide fleet-wide and
//! reach STS together, where throttling would become a 502 for each request in
//! the burst. The mitigation is an L2 tier (Cache API / KV) inside the exchange,
//! not a bigger in-memory cache — that tier is exactly what a deploy discards.
//! See #175, which builds one.
//!
//! Inside [`MIN_SERVE_SECS`] a live claim is deliberately overtaken, so a few
//! exchanges can run at once right at the edge of expiry: nothing safe is left
//! to serve, and the alternatives are making the request wait or failing one
//! that could have succeeded.
//!
//! Upstreamed as developmentseed/multistore#133; delete this module and go back
//! to `AwsBackendAuth` once that releases.

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

/// Never serve a credential with less than this much life left, even while a
/// renewal is in flight: the backend request it signs still has to be sent and
/// answered. Below this the request mints its own instead — S3 rejects an
/// expired credential with `ExpiredToken`, which reaches the caller as a
/// misleading `AccessDenied` rather than a retryable server-side error.
const MIN_SERVE_SECS: i64 = 5;

/// Treat a renewal claim older than this as abandoned and let another request
/// take it. A claimant cancelled between claiming and storing would otherwise
/// leave the key marked as renewing until the credential expires, at which point
/// every concurrent request exchanges at once — the burst the claim exists to
/// prevent. Comfortably above `STS_REQUEST_TIMEOUT`, so a live-but-slow exchange
/// is never mistaken for an abandoned one.
const CLAIM_TIMEOUT_SECS: i64 = 30;

/// Backend option keys consumed by this middleware — stripped once resolved so
/// they never reach the store builder.
const OIDC_OPTION_KEYS: [&str; 3] = ["auth_type", "oidc_role_arn", "oidc_subject"];

type Creds = Arc<BackendCredentials>;

/// A cached credential and the renewal claim on it, if any.
struct Entry {
    creds: Creds,
    /// When a request claimed the renewal of `creds`, so the others keep serving
    /// it (still valid) instead of piling onto STS. `None` when no renewal is in
    /// flight; see [`CLAIM_TIMEOUT_SECS`] for how a stuck claim is reaped.
    renewing_since: Option<chrono::DateTime<Utc>>,
}

/// What a caller should do for a given key.
#[derive(Debug)]
pub(crate) enum Action {
    /// Use these credentials as-is; no exchange needed.
    Serve(Creds),
    /// Nothing usable, or this caller claimed the renewal: run the exchange.
    Exchange,
}

/// Isolate-shared credentials, keyed by [`cache_key`].
///
/// A `std::sync::Mutex` rather than an async one, deliberately: the guard is
/// only ever held for a map get/insert and never across an `.await`, which is
/// what keeps a waiting request from being cancelled as hung. Workers is
/// single-threaded, so the lock is never actually contended.
static CACHE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();

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

fn cache() -> std::sync::MutexGuard<'static, HashMap<String, Entry>> {
    CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        // A poisoned lock means some *other* request panicked mid-map-write.
        // The map is still structurally sound (insert/get are not interruptible
        // in a way that leaves it torn), and refusing to federate over it would
        // turn one panic into a permanently broken isolate.
        .unwrap_or_else(|e| e.into_inner())
}

/// Decide what a caller should do for `role_arn` as `subject`, claiming the
/// renewal slot when this caller is the one that must run the exchange.
///
/// Not a lock: no caller ever waits on another. A credential inside the refresh
/// lead has not actually expired, so everyone but the claimant keeps using it
/// and only the claimant pays for the exchange. Without this, the whole lead
/// window is a miss for *every* concurrent request, and a busy isolate renews
/// with a burst of simultaneous `AssumeRoleWithWebIdentity` calls (which AWS may
/// then throttle into 502s) once per credential lifetime.
pub(crate) fn lookup(role_arn: &str, subject: &str) -> Action {
    let now = Utc::now();
    let mut map = cache();
    let Some(entry) = map.get_mut(&cache_key(role_arn, subject)) else {
        return Action::Exchange;
    };
    if entry.creds.expiration > now + Duration::seconds(REFRESH_LEAD_SECS) {
        return Action::Serve(entry.creds.clone());
    }
    // Inside the lead, someone is already on it, and there is enough life left
    // for the backend request this signs to complete.
    let claimed = entry
        .renewing_since
        .is_some_and(|at| now < at + Duration::seconds(CLAIM_TIMEOUT_SECS));
    if claimed && entry.creds.expiration > now + Duration::seconds(MIN_SERVE_SECS) {
        return Action::Serve(entry.creds.clone());
    }
    // Either we're first into the lead window, the previous claim went stale, or
    // the credential is now inside MIN_SERVE_SECS and too near expiry to hand
    // out.
    //
    // That last case deliberately ignores a live claim, so in the final
    // MIN_SERVE_SECS a claimant can be overtaken and several exchanges run at
    // once. Intended: nothing safe is left to serve, so the alternatives are
    // making the request wait (which the module docs rule out) or failing one
    // that could have succeeded. Bounded to the requests arriving in that window.
    entry.renewing_since = Some(now);
    Action::Exchange
}

/// Release the renewal claim after a failed exchange, so the next request
/// retries instead of coasting on a credential nobody is renewing.
///
/// Retries are serialized only while the credential is still servable: the next
/// request claims via [`lookup`] and the rest keep serving. Once it drops below
/// [`MIN_SERVE_SECS`] there is nothing safe to serve, so every concurrent
/// request exchanges — which is the correct behaviour, not a lapse.
fn release(role_arn: &str, subject: &str) {
    if let Some(entry) = cache().get_mut(&cache_key(role_arn, subject)) {
        entry.renewing_since = None;
    }
}

/// Cache `creds` as the credentials for `role_arn` assumed as `subject`,
/// releasing the renewal claim.
pub(crate) fn store(role_arn: &str, subject: &str, creds: Creds) {
    cache().insert(
        cache_key(role_arn, subject),
        Entry {
            creds,
            renewing_since: None,
        },
    );
}

/// Mark a renewal as claimed at `at`. Test-only seam for the abandoned-claim
/// path, which is otherwise only reachable by a request dying mid-exchange.
#[cfg(test)]
pub(crate) fn mark_renewing_since(role_arn: &str, subject: &str, at: chrono::DateTime<Utc>) {
    if let Some(entry) = cache().get_mut(&cache_key(role_arn, subject)) {
        entry.renewing_since = Some(at);
    }
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
    /// assertion when [`lookup`] says this request must.
    async fn credentials(&self, role_arn: &str, subject: &str) -> Result<Creds, ProxyError> {
        match lookup(role_arn, subject) {
            Action::Serve(creds) => return Ok(creds),
            Action::Exchange => {}
        }
        let token = self
            .signer
            .sign(subject, &self.issuer, &self.audience, &[])
            .inspect_err(|_| release(role_arn, subject))?;
        let mut exchange = AwsExchange::new(role_arn.to_string());
        // Name the assumed-role session after the subject so CloudTrail
        // attributes each exchange to the originating connection.
        exchange.session_name = session_name(subject);
        let creds = match exchange.exchange(&self.http, &token).await {
            Ok(creds) => Arc::new(creds),
            Err(e) => {
                release(role_arn, subject);
                return Err(e.into());
            }
        };
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
