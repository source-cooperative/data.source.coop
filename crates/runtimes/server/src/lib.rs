//! Tokio/Hyper runtime for the S3 proxy gateway.
//!
//! This crate provides concrete implementations of the core traits for a
//! standard server environment using Tokio and Hyper.
//!
//! - [`body::ServerBody`] — implements `BodyStream` using `http-body-util`
//! - [`client::HyperBackendClient`] — implements `BackendClient` using `reqwest`
//! - [`server::run`] — starts the Hyper HTTP server

pub mod body;
pub mod client;
pub mod server;
