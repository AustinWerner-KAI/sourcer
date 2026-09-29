use anyhow::{Context, Result};

/// Runtime configuration, read from environment variables only.
/// Secrets (API keys) are never logged and never sent to the browser.
#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    /// Built web app to serve at `/` (the Docker image sets this). None in development.
    pub web_dir: Option<String>,
    /// People Data Labs. Searches are refused until this is set.
    pub pdl_api_key: Option<String>,
    /// Apollo. Contact lookups are refused until this is set.
    pub apollo_api_key: Option<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = |k: &Option<String>| if k.is_some() { "<set>" } else { "<unset>" };
        f.debug_struct("Config")
            .field("database_url", &"<redacted>")
            .field("bind_addr", &self.bind_addr)
            .field("web_dir", &self.web_dir)
            .field("pdl_api_key", &set(&self.pdl_api_key))
            .field("apollo_api_key", &set(&self.apollo_api_key))
            .finish()
    }
}

/// An optional secret: unset and empty both mean "not configured".
fn secret(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?,
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            web_dir: std::env::var("WEB_DIR").ok(),
            pdl_api_key: secret("PDL_API_KEY"),
            apollo_api_key: secret("APOLLO_API_KEY"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_secrets() {
        let c = Config {
            database_url: "postgres://u:pw@h/db".into(),
            bind_addr: "x".into(),
            web_dir: None,
            pdl_api_key: Some("pdl-secret".into()),
            apollo_api_key: None,
        };
        let out = format!("{c:?}");
        assert!(!out.contains("pw") && !out.contains("pdl-secret"));
        assert!(out.contains("<set>") && out.contains("<unset>"));
    }
}
