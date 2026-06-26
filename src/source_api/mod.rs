//! Source Cooperative API integration: minting the proxy's auth token
//! ([`auth`]), fetching + edge-caching API responses ([`cache`]), the response
//! types ([`types`]), and resolving a product into a multistore bucket
//! ([`registry`]).

pub mod auth;
pub mod cache;
pub mod registry;
pub mod types;

pub(crate) use auth::ApiAuth;
pub(crate) use registry::SourceCoopRegistry;
