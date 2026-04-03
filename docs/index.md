# Source Cooperative Data Proxy

Documentation for the Source Cooperative data proxy — a read-only proxy built as a Cloudflare Worker in Rust that translates Source Cooperative URL paths into requests against cloud storage backends.

## Architecture Decisions

This documentation includes the RFC and ADRs that define the proxy's re-architecture:

- **[RFC-001](adrs/rfc-001.md)** — The overarching re-architecture proposal
- **[ADR-001](adrs/001-s3-credentials.md)** through **[ADR-007](adrs/007-configuration.md)** — Individual decisions on credentials, runtime, language, authentication, authorization, storage, and configuration
