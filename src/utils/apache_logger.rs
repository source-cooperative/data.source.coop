use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::{Error, HttpMessage};
use chrono::Local;
use futures::future::{ok, Ready};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::utils::auth::UserIdentity;

/// Public struct to enable the middleware in your app
pub struct ApacheLogger;

impl<S, B> Transform<S, ServiceRequest> for ApacheLogger
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Transform = ApacheLoggerMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(ApacheLoggerMiddleware { service })
    }
}

/// Middleware implementation that handles request logging in Apache log format
pub struct ApacheLoggerMiddleware<S> {
    pub service: S, // Make the field public if you need access to it
}

impl<S, B> Service<ServiceRequest> for ApacheLoggerMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + 'static>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        // Capture the start time
        let start_time = Local::now();
        let user_identity = req
            .extensions_mut()
            .get_mut::<UserIdentity>()
            .map(|identity| identity.clone()) // If the value exists, clone it
            .unwrap_or(UserIdentity { api_key: None }); // Otherwise, provide a default value

        let fut = self.service.call(req);

        Box::pin(async move {
            // Format the time in Apache style: 10/Oct/2000:13:55:36 -0700
            let formatted_time = start_time.format("%d/%b/%Y:%H:%M:%S %z").to_string();

            let res = fut.await?;
            let method = res.request().method().clone();
            let path = res.request().uri().clone();
            let status = res.response().status();

            let client_ip = res
                .request()
                .connection_info()
                .realip_remote_addr()
                .unwrap_or("-")
                .to_string();

            println!(
                "{} - {} [{}] \"{} {} HTTP/1.1\" {} 0",
                client_ip,
                match &user_identity.api_key {
                    Some(api_key) => api_key.account_id.clone(), // Safely access account_id
                    None => "default_account_id".to_string(),
                },
                formatted_time,
                method,
                path,
                status.as_u16()
            );

            Ok(res)
        })
    }
}
