//! Apollo People Match: work email for one known person (SRS F11). Used for
//! shortlisted people only. Personal emails are never requested (SRS N9).
//! Switched on when `APOLLO_API_KEY` is set (SRS D7).

use serde_json::{json, Value};

use super::{read_json, text, ContactLookup, ContactSource, SourceError, WorkEmail};

const DEFAULT_BASE_URL: &str = "https://api.apollo.io";

pub struct ApolloClient {
    http: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
}

impl ApolloClient {
    pub fn new(api_key: Option<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: Option<String>, base_url: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

impl ContactSource for ApolloClient {
    fn name(&self) -> &'static str {
        "apollo"
    }

    async fn find_work_email(&self, who: &ContactLookup) -> Result<Option<WorkEmail>, SourceError> {
        let key = self
            .api_key
            .as_deref()
            .ok_or(SourceError::NotConfigured("Apollo"))?;
        let body = json!({
            "name": who.full_name,
            "organization_name": who.employer,
            "linkedin_url": who.linkedin_url,
            "reveal_personal_emails": false,
        });
        let res = self
            .http
            .post(format!("{}/api/v1/people/match", self.base_url))
            .header("X-Api-Key", key)
            .json(&body)
            .send()
            .await
            .map_err(|e| SourceError::Network(e.without_url().to_string()))?;
        Ok(parse_match(&read_json(res).await?))
    }
}

/// Pull a usable work email from a People Match response, if there is one.
pub fn parse_match(v: &Value) -> Option<WorkEmail> {
    let person = v.get("person")?;
    let email = text(person, "email")?;
    // Apollo returns a placeholder when the email is locked on the plan.
    if email.contains("not_unlocked") || !email.contains('@') {
        return None;
    }
    Some(WorkEmail {
        verified: text(person, "email_status").as_deref() == Some("verified"),
        email,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_verified_email() {
        let v = json!({"person": {"email": "alex@examplepay.com", "email_status": "verified"}});
        assert_eq!(
            parse_match(&v),
            Some(WorkEmail {
                email: "alex@examplepay.com".into(),
                verified: true
            })
        );
    }

    #[test]
    fn locked_missing_or_no_match_is_none() {
        assert_eq!(
            parse_match(&json!({"person": {"email": "email_not_unlocked@domain.com"}})),
            None
        );
        assert_eq!(parse_match(&json!({"person": {"email": null}})), None);
        assert_eq!(parse_match(&json!({"person": null})), None);
        assert_eq!(parse_match(&json!({})), None);
    }

    #[test]
    fn unverified_email_is_marked_so() {
        let v = json!({"person": {"email": "sam@samplebank.com", "email_status": "guessed"}});
        assert!(!parse_match(&v).unwrap().verified);
    }

    #[tokio::test]
    async fn no_key_means_no_call() {
        let c = ApolloClient::with_base_url(None, "http://127.0.0.1:9");
        let who = ContactLookup {
            full_name: "Alex Example".into(),
            employer: None,
            linkedin_url: None,
        };
        assert!(matches!(
            c.find_work_email(&who).await,
            Err(SourceError::NotConfigured(_))
        ));
    }
}
