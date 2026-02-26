mod oidc;
mod output;
mod sts;

use clap::{Parser, Subcommand, ValueEnum};

#[cfg(feature = "staging")]
mod defaults {
    pub const ISSUER: &str = "https://auth.staging.source.coop";
    pub const CLIENT_ID: &str = "c445cc61-9884-44a8-b051-8d8f7273ffc1";
    pub const PROXY_URL: &str = "https://staging.data.source.coop";
    pub const ROLE_ARN: &str = "source-coop-user";
}

#[cfg(not(feature = "staging"))]
mod defaults {
    pub const ISSUER: &str = "https://auth.source.coop";
    pub const CLIENT_ID: &str = "d037d00b-09c7-4815-ac39-2a0b9fae40c6";
    pub const PROXY_URL: &str = "https://data.source.coop";
    pub const ROLE_ARN: &str = "source-coop-user";
}

#[derive(Parser)]
#[command(name = "source-coop", about = "Source Cooperative CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Authenticate via OIDC and obtain temporary S3 credentials
    Login(LoginArgs),
}

#[derive(Parser)]
struct LoginArgs {
    /// OIDC issuer URL
    #[arg(long, env = "SOURCE_OIDC_ISSUER", default_value = defaults::ISSUER)]
    issuer: String,

    /// OAuth2 client ID
    #[arg(long, env = "SOURCE_OIDC_CLIENT_ID", default_value = defaults::CLIENT_ID)]
    client_id: String,

    /// S3 proxy URL for STS
    #[arg(long, env = "SOURCE_PROXY_URL", default_value = defaults::PROXY_URL)]
    proxy_url: String,

    /// Role ARN to assume
    #[arg(long, env = "SOURCE_ROLE_ARN", default_value = defaults::ROLE_ARN)]
    role_arn: String,

    /// Output format
    #[arg(long, default_value = "credential-process")]
    format: OutputFormat,

    /// Session duration in seconds
    #[arg(long)]
    duration: Option<u64>,

    /// OAuth2 scopes
    #[arg(long, default_value = "openid")]
    scope: String,

    /// Local callback port (0 for random available port)
    #[arg(long, default_value = "0")]
    port: u16,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    /// AWS credential_process JSON format
    CredentialProcess,
    /// Shell export statements
    Env,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Login(args) => {
            if let Err(e) = run_login(args).await {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
    }
}

async fn run_login(args: LoginArgs) -> Result<(), String> {
    // 1. OIDC Discovery
    eprintln!("Discovering OIDC endpoints...");
    let endpoints = oidc::discover(&args.issuer).await?;

    // 2. Browser-based OIDC login
    let id_token = oidc::login(&endpoints, &args.client_id, &args.scope, args.port).await?;
    eprintln!("Authentication successful.");

    // 3. STS credential exchange
    eprintln!("Exchanging token for credentials...");
    let creds = sts::assume_role(&args.proxy_url, &args.role_arn, &id_token, args.duration).await?;

    // 4. Output
    match args.format {
        OutputFormat::CredentialProcess => output::print_credential_process(&creds),
        OutputFormat::Env => output::print_env(&creds),
    }

    Ok(())
}
