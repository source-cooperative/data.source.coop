//! Tokio/Hyper runtime for the S3 proxy gateway.
//!
//! This crate provides concrete implementations of the core traits for a
//! standard server environment using Tokio and Hyper.
//!
//! - [`client::ServerBackend`] — implements `ProxyBackend` using reqwest + object_store
//! - [`body`] — converts `ProxyResponseBody` to streaming hyper responses
//! - [`server::run`] — starts the Hyper HTTP server

pub mod body;
pub mod client;
pub mod server;
