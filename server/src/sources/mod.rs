//! Data providers behind small interfaces, so the rest of the code never
//! depends on a vendor. People Data Labs finds people (SRS F5); Apollo finds
//! work contact details (SRS F11). Tests use fakes of these traits.

use std::future::Future;

use serde::{Deserialize, Serialize};

pub mod apollo;
pub mod pdl;

/// One person as a provider returns them, before merging into `person`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonRecord {
    /// The provider's own id for this person (PDL id).
    pub source_id: String,
    pub full_name: String,
    pub current_title: Option<String>,
    pub current_employer: Option<String>,
    /// The current employer's web domain, when the provider gives it.
    #[serde(default)]
    pub current_employer_domain: Option<String>,
    pub location: Option<String>,
    pub linkedin_url: Option<String>,
    pub experience: Vec<ExperienceRecord>,
    /// Work email, when the plan unlocks it. Personal emails are never read
    /// from a provider (SRS N9).
    #[serde(default)]
    pub work_email: Option<String>,
    /// Phone numbers, when the plan unlocks them.
    #[serde(default)]
    pub phones: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperienceRecord {
    pub employer: String,
    #[serde(default)]
    pub employer_domain: Option<String>,
    pub title: Option<String>,
    /// As the provider gives it: "2021-03", "2021" or a full date.
    pub start: Option<String>,
    pub end: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchQuery {
    /// The search as an Elasticsearch query, built from the confirmed brief
    /// (see `plan`).
    pub query: serde_json::Value,
    /// Records wanted, 1 to 100.
    pub size: u32,
    pub scroll_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchPage {
    /// Everyone matching, not just this page.
    pub total: u64,
    pub records: Vec<PersonRecord>,
    pub scroll_token: Option<String>,
    pub credits_used: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContactLookup {
    pub full_name: String,
    pub employer: Option<String>,
    pub linkedin_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkEmail {
    pub email: String,
    pub verified: bool,
}

#[derive(Debug)]
pub enum SourceError {
    /// No API key configured for this provider.
    NotConfigured(&'static str),
    Http {
        status: u16,
        body: String,
    },
    Network(String),
    BadResponse(String),
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured(p) => write!(f, "{p} is not configured (no API key)"),
            Self::Http { status, body } => write!(f, "provider returned {status}: {body}"),
            Self::Network(e) => write!(f, "could not reach provider: {e}"),
            Self::BadResponse(e) => write!(f, "unexpected provider response: {e}"),
        }
    }
}

impl std::error::Error for SourceError {}

pub trait PeopleSource: Send + Sync {
    fn name(&self) -> &'static str;
    /// Credits a search will cost at most, shown before it runs (SRS N11).
    fn estimate_credits(&self, query: &SearchQuery) -> u32;
    fn search(
        &self,
        query: &SearchQuery,
    ) -> impl Future<Output = Result<SearchPage, SourceError>> + Send;
}

pub trait ContactSource: Send + Sync {
    fn name(&self) -> &'static str;
    /// Work email only. Personal addresses are never requested (SRS N9).
    fn find_work_email(
        &self,
        who: &ContactLookup,
    ) -> impl Future<Output = Result<Option<WorkEmail>, SourceError>> + Send;
}

/// Shared by the HTTP clients: turn a response into JSON or a `SourceError`.
async fn read_json(res: reqwest::Response) -> Result<serde_json::Value, SourceError> {
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| SourceError::Network(e.to_string()))?;
    if !status.is_success() {
        let body: String = text.chars().take(300).collect();
        return Err(SourceError::Http {
            status: status.as_u16(),
            body,
        });
    }
    serde_json::from_str(&text).map_err(|e| SourceError::BadResponse(e.to_string()))
}

/// A non-empty string field, or `None`. Masked fields on free plans come back
/// as `true` rather than a string, so they read as unknown.
fn text(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn masked_and_empty_fields_read_as_unknown() {
        let v = json!({"a": "Dubai", "b": true, "c": "  ", "d": null});
        assert_eq!(text(&v, "a").as_deref(), Some("Dubai"));
        assert_eq!(text(&v, "b"), None);
        assert_eq!(text(&v, "c"), None);
        assert_eq!(text(&v, "d"), None);
        assert_eq!(text(&v, "missing"), None);
    }
}
