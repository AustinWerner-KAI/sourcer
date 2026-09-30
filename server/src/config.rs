use anyhow::{Context, Result};

use crate::auth::AuthConfig;

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
    /// Microsoft 365 app registration. Sign-in is off until all three are set.
    pub m365_tenant_id: Option<String>,
    pub m365_client_id: Option<String>,
    pub m365_client_secret: Option<String>,
    /// The address people use to reach Sourcer, e.g. `http://localhost:8080`.
    pub public_url: String,
    /// Made admin on their first sign-in.
    pub admin_email: Option<String>,
    /// Anthropic, for drafting briefs (D8). Drafting is off until this is set.
    pub anthropic_api_key: Option<String>,
    /// Optional override of the Claude model.
    pub anthropic_model: Option<String>,
    /// Recruitly, the team's CRM and ATS (D10). Off until this is set.
    pub recruitly_api_key: Option<String>,
    /// Most Recruitly calls per day. Defaults below the plan's limit.
    pub recruitly_daily_cap: Option<i64>,
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
            .field("m365_tenant_id", &self.m365_tenant_id)
            .field("m365_client_id", &self.m365_client_id)
            .field("m365_client_secret", &set(&self.m365_client_secret))
            .field("public_url", &self.public_url)
            .field("admin_email", &self.admin_email)
            .field("anthropic_api_key", &set(&self.anthropic_api_key))
            .field("anthropic_model", &self.anthropic_model)
            .field("recruitly_api_key", &set(&self.recruitly_api_key))
            .field("recruitly_daily_cap", &self.recruitly_daily_cap)
            .finish()
    }
}

/// An optional secret: unset and empty both mean "not configured".
fn secret(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let config = Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?,
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            web_dir: std::env::var("WEB_DIR").ok(),
            pdl_api_key: secret("PDL_API_KEY"),
            apollo_api_key: secret("APOLLO_API_KEY"),
            m365_tenant_id: secret("M365_TENANT_ID"),
            m365_client_id: secret("M365_CLIENT_ID"),
            m365_client_secret: secret("M365_CLIENT_SECRET"),
            public_url: std::env::var("PUBLIC_URL")
                .unwrap_or_else(|_| "http://localhost:8080".into()),
            admin_email: secret("ADMIN_EMAIL"),
            anthropic_api_key: secret("ANTHROPIC_API_KEY"),
            anthropic_model: secret("ANTHROPIC_MODEL"),
            recruitly_api_key: secret("RECRUITLY_API_KEY"),
            recruitly_daily_cap: match secret("RECRUITLY_DAILY_CAP") {
                Some(v) => Some(
                    v.trim()
                        .parse()
                        .context("RECRUITLY_DAILY_CAP must be a whole number")?,
                ),
                None => None,
            },
        };
        config.validate()?;
        Ok(config)
    }

    /// Refuse settings that would weaken sign-in. The tenant must be the Austin
    /// Werner directory id: "common" or "organizations" would let any
    /// Microsoft account in.
    pub fn validate(&self) -> Result<()> {
        /// Microsoft's shared tenant for personal accounts.
        const CONSUMERS: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";
        if let Some(t) = &self.m365_tenant_id {
            let plain = uuid::Uuid::parse_str(t)
                .map(|u| u.hyphenated().to_string() == *t)
                .unwrap_or(false);
            if !plain || t == CONSUMERS {
                anyhow::bail!(
                    "M365_TENANT_ID must be your directory (tenant) ID: a lower-case GUID such as e7dd990b-9a86-456e-8822-dc9473519ce4"
                );
            }
        }
        Ok(())
    }

    /// Sign-in settings, or `None` if the app registration is incomplete.
    pub fn auth(&self) -> Option<AuthConfig> {
        Some(AuthConfig::new(
            self.m365_tenant_id.clone()?,
            self.m365_client_id.clone()?,
            self.m365_client_secret.clone()?,
            &self.public_url,
            self.admin_email.clone(),
        ))
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
            m365_tenant_id: Some("e7dd990b-9a86-456e-8822-dc9473519ce4".into()),
            m365_client_id: Some("c".into()),
            m365_client_secret: Some("ms-secret".into()),
            public_url: "http://localhost:8080".into(),
            admin_email: None,
            anthropic_api_key: Some("sk-ant-secret".into()),
            anthropic_model: None,
            recruitly_api_key: Some("rc-secret".into()),
            recruitly_daily_cap: None,
        };
        let out = format!("{c:?}");
        assert!(
            !out.contains("pw")
                && !out.contains("pdl-secret")
                && !out.contains("ms-secret")
                && !out.contains("sk-ant-secret")
                && !out.contains("rc-secret")
        );
        assert!(out.contains("<set>") && out.contains("<unset>"));
        assert!(c.auth().is_some());
        let incomplete = Config {
            m365_client_secret: None,
            ..c
        };
        assert!(
            incomplete.auth().is_none(),
            "sign-in stays off until all three are set"
        );
    }

    #[test]
    fn tenant_must_be_a_directory_id() {
        let base = Config {
            database_url: "x".into(),
            bind_addr: "x".into(),
            web_dir: None,
            pdl_api_key: None,
            apollo_api_key: None,
            m365_tenant_id: None,
            m365_client_id: None,
            m365_client_secret: None,
            public_url: "http://localhost:8080".into(),
            admin_email: None,
            anthropic_api_key: None,
            anthropic_model: None,
            recruitly_api_key: None,
            recruitly_daily_cap: None,
        };
        assert!(base.validate().is_ok());
        for bad in [
            "common",
            "organizations",
            "consumers",
            "austinwerner.io",
            "9188040d-6c67-4c5b-b112-36a304b66dad",
            "{e7dd990b-9a86-456e-8822-dc9473519ce4}",
            "urn:uuid:e7dd990b-9a86-456e-8822-dc9473519ce4",
        ] {
            let c = Config {
                m365_tenant_id: Some(bad.into()),
                ..base.clone()
            };
            assert!(c.validate().is_err(), "{bad} must be refused");
        }
        let good = Config {
            m365_tenant_id: Some("e7dd990b-9a86-456e-8822-dc9473519ce4".into()),
            ..base
        };
        assert!(good.validate().is_ok());
    }
}
