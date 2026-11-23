//! CLI flags and env vars. Names mirror GOWA (`src/cmd/root.go`) so existing
//! `.env` files keep working. See docs/SPEC.md §7.

use std::net::IpAddr;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "ruwa-gateway",
    version,
    about = "GOWA-compatible WhatsApp REST API"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the REST API server.
    Rest(Box<Config>),
    /// Print the version.
    Version,
}

#[derive(Debug, Clone, Args)]
pub struct Config {
    #[arg(long, env = "APP_PORT", default_value_t = 3000)]
    pub port: u16,

    #[arg(long, env = "APP_HOST", default_value = "0.0.0.0")]
    pub host: IpAddr,

    #[arg(long, env = "APP_DEBUG", default_value_t = false)]
    pub debug: bool,

    /// Name shown as the linked device on the phone.
    #[arg(long, env = "APP_OS", default_value = "Chrome")]
    pub os: String,

    /// Comma-separated `user:pass` pairs.
    #[arg(long = "basic-auth", env = "APP_BASIC_AUTH", value_delimiter = ',')]
    pub basic_auth: Vec<String>,

    /// Allow starting without basic auth on a non-loopback bind.
    #[arg(long, env = "APP_ALLOW_UNAUTHENTICATED", default_value_t = false)]
    pub allow_unauthenticated: bool,

    #[arg(long = "base-path", env = "APP_BASE_PATH", default_value = "")]
    pub base_path: String,

    #[arg(
        long = "cors-allowed-origins",
        env = "APP_CORS_ALLOWED_ORIGINS",
        value_delimiter = ','
    )]
    pub cors_allowed_origins: Vec<String>,

    #[arg(long, env = "WHATSAPP_WEBHOOK", value_delimiter = ',')]
    pub webhook: Vec<String>,

    #[arg(
        long = "webhook-secret",
        env = "WHATSAPP_WEBHOOK_SECRET",
        default_value = "secret"
    )]
    pub webhook_secret: String,

    #[arg(
        long = "webhook-events",
        env = "WHATSAPP_WEBHOOK_EVENTS",
        value_delimiter = ','
    )]
    pub webhook_events: Vec<String>,

    #[arg(
        long = "webhook-ignore-jids",
        env = "WHATSAPP_WEBHOOK_IGNORE_JIDS",
        value_delimiter = ','
    )]
    pub webhook_ignore_jids: Vec<String>,

    #[arg(long = "auto-reply", env = "WHATSAPP_AUTO_REPLY", default_value = "")]
    pub auto_reply: String,

    #[arg(
        long = "auto-mark-read",
        env = "WHATSAPP_AUTO_MARK_READ",
        default_value_t = false
    )]
    pub auto_mark_read: bool,

    #[arg(
        long = "auto-reject-call",
        env = "WHATSAPP_AUTO_REJECT_CALL",
        default_value_t = false
    )]
    pub auto_reject_call: bool,

    #[arg(long = "auto-download-media", env = "WHATSAPP_AUTO_DOWNLOAD_MEDIA", default_value_t = true, action = clap::ArgAction::Set)]
    pub auto_download_media: bool,

    #[arg(long = "account-validation", env = "WHATSAPP_ACCOUNT_VALIDATION", default_value_t = true, action = clap::ArgAction::Set)]
    pub account_validation: bool,

    /// Only SQLite is supported in v1.
    #[arg(
        long = "db-uri",
        env = "DB_URI",
        default_value = "file:storages/ruwa.db"
    )]
    pub db_uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub user: String,
    pub pass: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("invalid APP_BASIC_AUTH entry {0:?}: expected user:pass")]
    InvalidBasicAuth(String),
    #[error(
        "APP_BASIC_AUTH is empty while binding to non-loopback host {0}; set APP_BASIC_AUTH or APP_ALLOW_UNAUTHENTICATED=true"
    )]
    UnauthenticatedPublicBind(IpAddr),
}

impl Config {
    pub fn credentials(&self) -> Result<Vec<Credential>, ConfigError> {
        self.basic_auth
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| match s.split_once(':') {
                Some((u, p)) if !u.is_empty() => Ok(Credential {
                    user: u.to_owned(),
                    pass: p.to_owned(),
                }),
                _ => Err(ConfigError::InvalidBasicAuth(s.to_owned())),
            })
            .collect()
    }

    /// GOWA refuses to start unauthenticated on a public bind.
    pub fn validate(&self) -> Result<Vec<Credential>, ConfigError> {
        let creds = self.credentials()?;
        if creds.is_empty() && !self.host.is_loopback() && !self.allow_unauthenticated {
            return Err(ConfigError::UnauthenticatedPublicBind(self.host));
        }
        Ok(creds)
    }

    /// Base path normalised to `""` or `"/segment"` (no trailing slash).
    pub fn normalized_base_path(&self) -> String {
        let trimmed = self.base_path.trim().trim_matches('/');
        if trimmed.is_empty() {
            String::new()
        } else {
            format!("/{trimmed}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(args: &[&str]) -> Config {
        let mut full = vec!["ruwa-gateway", "rest"];
        full.extend_from_slice(args);
        match Cli::try_parse_from(full).unwrap().command {
            Command::Rest(c) => *c,
            Command::Version => unreachable!(),
        }
    }

    #[test]
    fn refuses_public_bind_without_auth() {
        let c = cfg(&["--host", "0.0.0.0"]);
        assert!(matches!(
            c.validate(),
            Err(ConfigError::UnauthenticatedPublicBind(_))
        ));
    }

    #[test]
    fn allows_loopback_without_auth() {
        assert!(cfg(&["--host", "127.0.0.1"]).validate().unwrap().is_empty());
    }

    #[test]
    fn parses_multiple_credentials() {
        let c = cfg(&["--basic-auth", "a:1,b:2"]);
        assert_eq!(c.validate().unwrap().len(), 2);
    }

    #[test]
    fn rejects_malformed_credential() {
        assert!(cfg(&["--basic-auth", "nocolon"]).validate().is_err());
    }

    #[test]
    fn normalizes_base_path() {
        assert_eq!(
            cfg(&["--base-path", "/gowa/"]).normalized_base_path(),
            "/gowa"
        );
        assert_eq!(cfg(&[]).normalized_base_path(), "");
    }
}
