//! # s3-proxy-core
//!
//! Runtime-agnostic core library for the S3 proxy gateway.
//!
//! This crate defines the trait abstractions that allow the proxy to run on
//! multiple runtimes (Tokio/Hyper for containers, Cloudflare Workers for edge)
//! without either runtime leaking into the core logic.
//!
//! ## Key Abstractions
//!
//! - [`response_body::ProxyResponseBody`] — concrete response body type (Stream, Bytes, Empty)
//! - [`backend::ProxyBackend`] — create object stores and send raw HTTP requests
//! - [`config::ConfigProvider`] — retrieve bucket/role/credential configuration from any backend
//! - [`auth`] — SigV4 request verification and credential resolution
//! - [`s3::request`] — parse incoming S3 API requests into typed operations
//! - [`s3::response`] — serialize S3 XML responses
//! - [`proxy::ProxyHandler`] — the main request handler that ties everything together

pub mod auth;
#[cfg(feature = "axum")]
pub mod axum;
pub mod backend;
pub mod config;
pub mod error;
pub mod maybe_send;
pub mod proxy;
pub mod resolver;
pub mod response_body;
pub mod s3;
pub mod types;
