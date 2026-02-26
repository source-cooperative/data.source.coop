//! Tokio/axum runtime for the S3 proxy gateway.
//!
//! This crate provides concrete implementations of the core traits for a
//! standard server environment using Tokio and axum.
//!
//! - [`client::ServerBackend`] — implements `ProxyBackend` using reqwest + object_store
//! - [`server::run`] — starts the axum HTTP server

pub mod client;
pub mod server;
