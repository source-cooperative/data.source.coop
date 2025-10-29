//! Tracing middleware with health check filtering.
//!
//! This module provides custom tracing middleware for actix-web that filters out
//! health check requests from distributed tracing to reduce noise and costs.

use actix_web::{
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    Error,
};
use futures_util::future::LocalBoxFuture;
use std::{
    env,
    future::{ready, Ready},
    rc::Rc,
};

/// Middleware factory for selectively applying tracing to requests.
///
/// This middleware wraps `TracingLogger` from `tracing-actix-web` but filters
/// out specific paths (like health checks) to avoid creating unnecessary spans.
///
/// # Environment Variables
///
/// - `TRACING_SKIP_PATHS`: Comma-separated list of paths to exclude from tracing (default: "/")
///
/// # Examples
///
/// ```rust
/// use actix_web::{App, HttpServer};
/// use source_data_proxy::utils::tracing_middleware::TracingMiddleware;
///
/// HttpServer::new(|| {
///     App::new()
///         .wrap(TracingMiddleware::new())
///         // ... rest of app configuration
/// })
/// ```
pub struct TracingMiddleware {
    skip_paths: Vec<String>,
}

impl TracingMiddleware {
    /// Creates a new `TracingMiddleware` instance.
    ///
    /// Reads skip paths from the `TRACING_SKIP_PATHS` environment variable.
    /// Defaults to skipping "/" (health check endpoint) if not configured.
    pub fn new() -> Self {
        let skip_paths = env::var("TRACING_SKIP_PATHS")
            .unwrap_or_else(|_| "/".to_string())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Self { skip_paths }
    }

    /// Creates a new `TracingMiddleware` instance with custom skip paths.
    ///
    /// # Arguments
    ///
    /// * `skip_paths` - Vector of path strings to exclude from tracing
    ///
    /// # Examples
    ///
    /// ```rust
    /// use source_data_proxy::utils::tracing_middleware::TracingMiddleware;
    ///
    /// let middleware = TracingMiddleware::with_skip_paths(vec![
    ///     "/".to_string(),
    ///     "/health".to_string(),
    /// ]);
    /// ```
    #[allow(dead_code)]
    pub fn with_skip_paths(skip_paths: Vec<String>) -> Self {
        Self { skip_paths }
    }

    /// Checks if a request path should be skipped from tracing.
    #[allow(dead_code)]
    fn should_skip(&self, path: &str) -> bool {
        self.skip_paths.iter().any(|skip| path == skip)
    }
}

impl Default for TracingMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, B> Transform<S, ServiceRequest> for TracingMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type InitError = ();
    type Transform = TracingMiddlewareService<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        let skip_paths = self.skip_paths.clone();
        ready(Ok(TracingMiddlewareService {
            service: Rc::new(service),
            skip_paths,
        }))
    }
}

/// Service implementation for the tracing middleware.
pub struct TracingMiddlewareService<S> {
    service: Rc<S>,
    skip_paths: Vec<String>,
}

impl<S, B> Service<ServiceRequest> for TracingMiddlewareService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let path = req.path().to_string();
        let should_skip = self.skip_paths.iter().any(|skip| path == *skip);

        if should_skip {
            // Skip tracing for this request
            let fut = self.service.call(req);
            Box::pin(async move { fut.await })
        } else {
            // Apply tracing for this request
            let root_span = tracing::info_span!(
                "HTTP request",
                http.method = %req.method(),
                http.target = %req.path(),
                http.status_code = tracing::field::Empty,
                otel.kind = "server",
                otel.status_code = tracing::field::Empty,
            );

            let _enter = root_span.enter();

            // Extract path parameters if available (account_id, repository_id)
            if let Some(path_info) = req.match_info().get("account_id") {
                tracing::Span::current().record("account_id", path_info);
            }
            if let Some(path_info) = req.match_info().get("repository_id") {
                tracing::Span::current().record("repository_id", path_info);
            }

            drop(_enter);

            let fut = self.service.call(req);

            Box::pin(async move {
                let res = fut.await?;

                // Record response status
                root_span.record("http.status_code", res.status().as_u16());
                if res.status().is_server_error() || res.status().is_client_error() {
                    root_span.record("otel.status_code", "ERROR");
                } else {
                    root_span.record("otel.status_code", "OK");
                }

                Ok(res)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_skip() {
        let middleware = TracingMiddleware::with_skip_paths(vec![
            "/".to_string(),
            "/health".to_string(),
            "/metrics".to_string(),
        ]);

        assert!(middleware.should_skip("/"));
        assert!(middleware.should_skip("/health"));
        assert!(middleware.should_skip("/metrics"));
        assert!(!middleware.should_skip("/api/users"));
        assert!(!middleware.should_skip("/test"));
    }

    #[test]
    fn test_default_middleware() {
        temp_env::with_var("TRACING_SKIP_PATHS", Some("/,/health"), || {
            let middleware = TracingMiddleware::new();
            assert!(middleware.should_skip("/"));
            assert!(middleware.should_skip("/health"));
            assert!(!middleware.should_skip("/api"));
        });
    }

    #[test]
    fn test_empty_skip_paths() {
        let middleware = TracingMiddleware::with_skip_paths(vec![]);
        assert!(!middleware.should_skip("/"));
        assert!(!middleware.should_skip("/health"));
    }
}
