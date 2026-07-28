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
//! why this works where a lock cannot. Only a genuinely empty or expired entry
//! makes concurrent requests each exchange — a cold isolate, where there is no
//! usable credential to serve and nothing to single-flight *to*.

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

/// A cached credential plus whether someone is already renewing it.
struct Entry {
    creds: Creds,
    /// Set by the request that claimed the renewal for this key, so the others
    /// keep serving `creds` (still valid) instead of piling onto STS. Cleared
    /// when a renewal stores its result, or fails.
    renewing: bool,
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
    // Inside the lead but genuinely still valid, and someone is already on it.
    if entry.creds.expiration > now && entry.renewing {
        return Action::Serve(entry.creds.clone());
    }
    // Either we're first into the lead window, or the credential has actually
    // expired and there is nothing safe left to serve. Claim the slot — when
    // expired this does not gate anyone (they have no usable credential either),
    // it just records that a renewal is under way.
    entry.renewing = true;
    Action::Exchange
}

/// Release the renewal slot after a failed exchange, so the next request retries
/// instead of waiting out the lead window. Retries stay serialized: whoever
/// retries claims the slot again via [`lookup`].
fn release(role_arn: &str, subject: &str) {
    if let Some(entry) = cache().get_mut(&cache_key(role_arn, subject)) {
        entry.renewing = false;
    }
}

/// Cache `creds` as the credentials for `role_arn` assumed as `subject`,
/// releasing the renewal slot.
pub(crate) fn store(role_arn: &str, subject: &str, creds: Creds) {
    cache().insert(
        cache_key(role_arn, subject),
        Entry {
            creds,
            renewing: false,
        },
    );
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
