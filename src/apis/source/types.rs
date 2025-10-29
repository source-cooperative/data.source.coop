//! Data structures and types for the Source Cooperative API.
//!
//! This module contains all the data types, enums, and structures used to interact
//! with the Source Cooperative platform, including products, accounts, permissions,
//! and storage configurations.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Repository access permissions for products.
///
/// Defines the level of access a user or account has to a specific product.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RepositoryPermission {
    /// Read-only access to the product data
    #[serde(rename = "read")]
    Read,
    /// Read and write access to the product data
    #[serde(rename = "write")]
    Write,
}

/// Product visibility levels that control who can discover and access the product.
///
/// This determines how the product appears in listings and search results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProductVisibility {
    /// Product is visible to everyone and appears in public listings
    #[serde(rename = "public")]
    Public,
    /// Product is not listed publicly but can be accessed with direct link
    #[serde(rename = "unlisted")]
    Unlisted,
    /// Product access is restricted to specific users or groups
    #[serde(rename = "restricted")]
    Restricted,
}

/// Data access modes that define how users can access the product's data.
///
/// This controls the business model and access patterns for the product.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProductDataMode {
    /// Data is freely accessible to anyone
    #[serde(rename = "open")]
    Open,
    /// Data requires a subscription to access
    #[serde(rename = "subscription")]
    Subscription,
    /// Data is private and only accessible to authorized users
    #[serde(rename = "private")]
    Private,
}

/// Supported storage backend types for product data mirrors.
///
/// Each product can have multiple mirrors across different storage providers
/// for redundancy and performance optimization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StorageType {
    /// Amazon S3 compatible storage
    #[serde(rename = "s3")]
    S3,
    /// Microsoft Azure Blob Storage
    #[serde(rename = "azure")]
    Azure,
    /// Google Cloud Storage
    #[serde(rename = "gcs")]
    Gcs,
    /// MinIO object storage
    #[serde(rename = "minio")]
    Minio,
    /// Ceph distributed storage
    #[serde(rename = "ceph")]
    Ceph,
}

/// Account types in the Source Cooperative system.
///
/// Different account types have different capabilities and metadata structures.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AccountType {
    /// Individual user account
    #[serde(rename = "individual")]
    Individual,
    /// Organization or group account
    #[serde(rename = "organization")]
    Organization,
}

/// Domain verification status for account domains.
///
/// Used to track the verification state of custom domains associated with accounts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DomainStatus {
    /// Domain has not been verified
    #[serde(rename = "unverified")]
    Unverified,
    /// Domain verification is in progress
    #[serde(rename = "pending")]
    Pending,
    /// Domain has been successfully verified
    #[serde(rename = "verified")]
    Verified,
}

/// Methods available for domain verification.
///
/// Different verification methods provide different levels of security and ease of use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum VerificationMethod {
    /// DNS-based verification using TXT records
    #[serde(rename = "dns")]
    Dns,
    /// HTML-based verification using meta tags
    #[serde(rename = "html")]
    Html,
    /// File-based verification using uploaded files
    #[serde(rename = "file")]
    File,
}

/// API key credentials for authenticating with the Source API.
///
/// Contains the access key ID and secret access key used for API authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct APIKey {
    /// The access key ID for API authentication
    pub access_key_id: String,
    /// The secret access key for API authentication
    pub secret_access_key: String,
}

/// Represents a product in the Source Cooperative system.
///
/// A product is the main entity that contains data and metadata, similar to a repository
/// in traditional version control systems. Products can have multiple storage mirrors
/// for redundancy and performance optimization.
///
/// # Examples
///
/// ```rust
/// use serde_json;
///
/// let json = r#"{
///   "product_id": "example-product",
///   "account_id": "example-account",
///   "title": "Example Product",
///   "description": "An example product",
///   "created_at": "2023-01-01T00:00:00Z",
///   "updated_at": "2023-01-01T00:00:00Z",
///   "visibility": "public",
///   "disabled": false,
///   "data_mode": "open",
///   "featured": 0,
///   "metadata": { ... },
///   "account": { ... }
/// }"#;
/// let product: SourceProduct = serde_json::from_str(json)?;
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProduct {
    /// Unique identifier for the product (3-40 chars, lowercase, alphanumeric with hyphens)
    pub product_id: String,

    /// ID of the account that owns this product
    pub account_id: String,

    /// Human-readable title of the product
    pub title: String,

    /// Detailed description of the product
    pub description: String,

    /// ISO 8601 timestamp when the product was created
    pub created_at: String,

    /// ISO 8601 timestamp when the product was last updated
    pub updated_at: String,

    /// Visibility level of the product
    pub visibility: ProductVisibility,

    /// Whether the product is disabled
    pub disabled: bool,

    /// Data access mode for the product
    pub data_mode: ProductDataMode,

    /// Featured status (0 = not featured, 1 = featured)
    pub featured: i32,

    /// Product metadata including mirrors, tags, and roles
    pub metadata: SourceProductMetadata,

    /// Optional account information
    pub account: Option<SourceProductAccount>,
}

/// Metadata for a product including mirrors, tags, and roles.
///
/// Contains all the configuration and organizational information for a product
/// that doesn't fit into the main product fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProductMetadata {
    /// Map of mirror names to mirror configurations
    pub mirrors: HashMap<String, SourceProductMirror>,

    /// Name of the primary mirror (key in the mirrors map)
    pub primary_mirror: String,

    /// Optional list of tags associated with the product
    pub tags: Option<Vec<String>>,
}

/// Configuration for a storage mirror of a product.
///
/// Each product can have multiple mirrors across different storage providers
/// for redundancy and performance optimization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProductMirror {
    /// Type of storage backend used for this mirror
    pub storage_type: StorageType,

    /// ID of the data connection configuration
    pub connection_id: String,

    /// Storage prefix/path for this mirror
    pub prefix: String,

    /// Storage-specific configuration options
    pub config: SourceProductMirrorConfig,

    /// Whether this is the primary mirror for the product
    pub is_primary: bool,
}

/// Storage-specific configuration options for a mirror.
///
/// Different storage backends require different configuration parameters.
/// All fields are optional and only relevant for specific storage types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProductMirrorConfig {
    /// AWS region for S3/GCS storage
    pub region: Option<String>,

    /// Bucket name for S3/GCS storage
    pub bucket: Option<String>,

    /// Container name for Azure Blob Storage
    pub container: Option<String>,

    /// Custom endpoint URL for MinIO/Ceph storage
    pub endpoint: Option<String>,
}

/// Account information associated with a product.
///
/// Contains the account details of the product owner, including profile information,
/// contact details, and organizational metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProductAccount {
    /// Unique identifier for the account
    pub account_id: String,

    /// Type of account (individual or organization)
    #[serde(rename = "type")]
    pub account_type: AccountType,

    /// Display name of the account
    pub name: String,

    /// Identity provider ID (only for individual accounts)
    pub identity_id: Option<String>,

    /// Public metadata visible to other users
    pub metadata_public: SourceProductAccountMetadataPublic,

    /// Email addresses associated with the account
    pub emails: Option<Vec<SourceAccountEmail>>,

    /// ISO 8601 timestamp when the account was created
    pub created_at: String,

    /// ISO 8601 timestamp when the account was last updated
    pub updated_at: String,

    /// Whether the account is disabled
    pub disabled: bool,

    /// Account capability flags
    pub flags: Vec<String>,

    /// Private metadata not visible to other users
    pub metadata_private: Option<HashMap<String, serde_json::Value>>,
}

/// Domain verification information for an account.
///
/// Tracks the verification status and process for custom domains associated with accounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountDomain {
    /// The domain name being verified
    pub domain: String,

    /// Current verification status of the domain
    pub status: DomainStatus,

    /// Method used for verification (if applicable)
    pub verification_method: Option<VerificationMethod>,

    /// Token used for verification (if applicable)
    pub verification_token: Option<String>,

    /// ISO 8601 timestamp when verification was completed
    pub verified_at: Option<String>,

    /// ISO 8601 timestamp when domain was added
    pub created_at: String,

    /// ISO 8601 timestamp when verification expires (if applicable)
    pub expires_at: Option<String>,
}

/// Email address information for an account.
///
/// Tracks email addresses associated with an account, including verification status
/// and primary email designation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceAccountEmail {
    /// The email address
    pub address: String,

    /// Whether the email address has been verified
    pub verified: bool,

    /// ISO 8601 timestamp when verification was completed
    pub verified_at: Option<String>,

    /// Whether this is the primary email address for the account
    pub is_primary: bool,

    /// ISO 8601 timestamp when the email was added
    pub added_at: String,
}

/// Public metadata for an account.
///
/// Information that is visible to other users and can be displayed in public profiles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProductAccountMetadataPublic {
    /// Optional biographical information
    pub bio: Option<String>,

    /// Verified domains associated with the account
    pub domains: Option<Vec<AccountDomain>>,

    /// Geographic location of the account holder
    pub location: Option<String>,

    /// Owner account ID (for organizational accounts)
    pub owner_account_id: Option<String>,

    /// List of admin account IDs (for organizational accounts)
    pub admin_account_ids: Option<Vec<String>>,

    /// List of member account IDs (for organizational accounts)
    pub member_account_ids: Option<Vec<String>>,
}

/// Details about a data connection configuration.
///
/// Contains provider-specific information about how to connect to storage backends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataConnectionDetails {
    /// Storage provider type (e.g., "s3", "az")
    pub provider: String,
    /// Cloud region for the storage service
    pub region: Option<String>,
    /// Base prefix for all data stored through this connection
    pub base_prefix: Option<String>,
    /// S3 bucket name (for S3-compatible providers)
    pub bucket: Option<String>,
    /// Azure storage account name (for Azure)
    pub account_name: Option<String>,
    /// Azure container name (for Azure)
    pub container_name: Option<String>,
}

/// Authentication configuration for a data connection.
///
/// Defines how to authenticate with the storage backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataConnectionAuthentication {
    /// Type of authentication (e.g., "s3_local", "iam_role")
    #[serde(rename = "type")]
    pub auth_type: String,
    /// Access key ID for credential-based authentication
    pub access_key_id: Option<String>,
    /// Secret access key for credential-based authentication
    pub secret_access_key: Option<String>,
}

/// Configuration for connecting to external data storage.
///
/// A data connection defines how products can access external storage backends
/// like S3, Azure Blob Storage, or other object storage systems.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataConnection {
    /// Unique identifier for this data connection
    pub data_connection_id: String,
    /// Human-readable name for the connection
    pub name: String,
    /// Template for generating storage prefixes
    pub prefix_template: String,
    /// Whether this connection only allows read operations
    pub read_only: bool,
    /// List of data modes that can use this connection
    pub allowed_data_modes: Vec<String>,
    /// Optional flag required on accounts to use this connection
    pub required_flag: Option<String>,
    /// Provider-specific connection details
    pub details: DataConnectionDetails,
    /// Authentication configuration for the connection
    pub authentication: Option<DataConnectionAuthentication>,
}

/// List of products with pagination support.
///
/// Used for API responses that return multiple products.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProductList {
    /// List of products in this page
    pub products: Vec<SourceProduct>,
    /// Token for fetching the next page of results
    pub next: Option<String>,
}
