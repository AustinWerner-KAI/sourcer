use anyhow::{Context, Result};

/// Runtime configuration, read from environment variables only.
/// Secrets (API keys) are never logged and never sent to the browser.
#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    /// Built web app to serve at `/` (the Docker image sets this). None in development.
    pub web_dir: Option<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("database_url", &"<redacted>")
            .field("bind_addr", &self.bind_addr)
            .field("web_dir", &self.web_dir)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?,
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            web_dir: std::env::var("WEB_DIR").ok(),
        })
    }
}
