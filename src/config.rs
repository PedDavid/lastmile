//! Command-line flags, each also settable through a `LASTMILE_*` environment variable.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{CommandFactory, Parser};
use jiff::SignedDuration;
use url::Url;

use crate::duration;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "lastmile",
    version,
    about = "The packages a household is expecting, and the codes to collect them"
)]
pub struct Config {
    /// Address for the UI.
    #[arg(long, env = "LASTMILE_LISTEN", default_value = "0.0.0.0:8080")]
    pub listen: SocketAddr,

    /// Address for /metrics, /healthz and /readyz (no auth).
    #[arg(long, env = "LASTMILE_METRICS_LISTEN", default_value = "0.0.0.0:9090")]
    pub metrics_listen: SocketAddr,

    /// Public URL of the UI, e.g. https://parcels.example.com. Used for the
    /// OIDC redirect and the Origin check on POSTs.
    #[arg(
        long,
        env = "LASTMILE_PUBLIC_URL",
        default_value = "http://localhost:8080"
    )]
    pub public_url: Url,

    /// SQLite database file. Its directory must be writable (WAL files sit
    /// next to it).
    #[arg(long, env = "LASTMILE_DATABASE", default_value = "/data/lastmile.db")]
    pub database: PathBuf,

    /// A package with no news for this long needs attention.
    #[arg(long, env = "LASTMILE_STALE_AFTER", default_value = "7d", value_parser = parse_duration)]
    pub stale_after: SignedDuration,

    /// Delivered packages are archived this long after delivery.
    #[arg(long, env = "LASTMILE_ARCHIVE_AFTER", default_value = "7d", value_parser = parse_duration)]
    pub archive_after: SignedDuration,

    /// The text of a message (personal data) is deleted this long after it
    /// came in; the package it updated and the inbox result stay.
    #[arg(long, env = "LASTMILE_MESSAGE_RETENTION", default_value = "30d", value_parser = parse_duration)]
    pub message_retention: SignedDuration,

    #[command(flatten)]
    pub auth: AuthConfig,

    /// Serve sample packages from a temporary in-memory database. Uses
    /// --insecure-dev-auth unless OIDC is configured. For trying out the UI only.
    #[arg(long, env = "LASTMILE_DEMO")]
    pub demo: bool,

    /// Log as JSON.
    #[arg(long, env = "LASTMILE_LOG_JSON")]
    pub log_json: bool,
}

#[derive(clap::Args, Debug, Clone)]
pub struct AuthConfig {
    /// OIDC issuer URL.
    #[arg(long, env = "LASTMILE_OIDC_ISSUER")]
    pub oidc_issuer: Option<Url>,

    #[arg(long, env = "LASTMILE_OIDC_CLIENT_ID")]
    pub oidc_client_id: Option<String>,

    /// File containing the OIDC client secret.
    #[arg(long, env = "LASTMILE_OIDC_CLIENT_SECRET_FILE")]
    pub oidc_client_secret_file: Option<PathBuf>,

    /// Space-separated scopes to request besides `openid`.
    #[arg(
        long,
        env = "LASTMILE_OIDC_SCOPES",
        default_value = "profile email groups"
    )]
    pub oidc_scopes: String,

    /// ID token claim holding the user's groups or roles.
    #[arg(long, env = "LASTMILE_ADMIN_CLAIM", default_value = "groups")]
    pub admin_claim: String,

    /// Value of the admin claim that grants admin rights.
    #[arg(long, env = "LASTMILE_ADMIN_VALUE", default_value = "lastmile-admins")]
    pub admin_value: String,

    /// Session lifetime.
    #[arg(long, env = "LASTMILE_SESSION_TTL", default_value = "30d", value_parser = parse_duration)]
    pub session_ttl: SignedDuration,

    /// File containing at least 64 random bytes (raw or base64) used to
    /// encrypt session cookies. Random per process if unset, which logs
    /// everyone out on restart.
    #[arg(long, env = "LASTMILE_COOKIE_KEY_FILE")]
    pub cookie_key_file: Option<PathBuf>,

    /// Skip OIDC and treat every visitor as an admin. Never use in production.
    #[arg(long, env = "LASTMILE_INSECURE_DEV_AUTH")]
    pub insecure_dev_auth: bool,
}

fn parse_duration(s: &str) -> Result<SignedDuration, String> {
    duration::parse(s).map_err(|e| e.to_string())
}

impl Config {
    /// Parses flags and environment, and exits with a usage error when they
    /// are inconsistent.
    pub fn load() -> Config {
        let cfg = Config::parse();
        if let Err(e) = cfg.check() {
            Config::command()
                .error(clap::error::ErrorKind::ArgumentConflict, e)
                .exit();
        }
        cfg
    }

    /// Checks between flags that clap cannot express.
    pub fn check(&self) -> Result<(), String> {
        if self.auth.session_ttl.is_negative() || self.auth.session_ttl.is_zero() {
            return Err("--session-ttl must be positive".into());
        }
        for (flag, d) in [
            ("--stale-after", self.stale_after),
            ("--archive-after", self.archive_after),
            ("--message-retention", self.message_retention),
        ] {
            if d.is_negative() || d.is_zero() {
                return Err(format!("{flag} must be positive"));
            }
        }
        Ok(())
    }

    /// `scheme://host[:port]` of the public URL, as sent in `Origin` headers.
    pub fn public_origin(&self) -> String {
        self.public_url.origin().ascii_serialization()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse() {
        let cfg = Config::try_parse_from(["lastmile"]).unwrap();
        assert!(cfg.check().is_ok());
        assert_eq!(cfg.public_origin(), "http://localhost:8080");
        assert_eq!(cfg.database, PathBuf::from("/data/lastmile.db"));
    }

    #[test]
    fn retention_must_be_positive() {
        let cfg = Config::try_parse_from(["lastmile", "--message-retention", "0d"]).unwrap();
        assert_eq!(
            cfg.check().unwrap_err(),
            "--message-retention must be positive"
        );
    }

    #[test]
    fn session_ttl_must_be_positive() {
        let cfg = Config::try_parse_from(["lastmile", "--session-ttl", "0s"]).unwrap();
        assert!(cfg.check().is_err());
    }
}
