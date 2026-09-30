//! People Data Labs Person Search (v5). Every call costs at least one credit,
//! plus one per record returned; at most 100 records per call.

use serde_json::{json, Value};

use crate::employer::normalise_domain;

use super::{
    read_json, text, ExperienceRecord, PeopleSource, PersonRecord, SearchPage, SearchQuery,
    SourceError,
};

pub const MAX_PAGE: u32 = 100;
const DEFAULT_BASE_URL: &str = "https://api.peopledatalabs.com";

pub struct PdlClient {
    http: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
}

impl PdlClient {
    pub fn new(api_key: Option<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn configured(&self) -> bool {
        self.api_key.is_some()
    }

    pub fn with_base_url(api_key: Option<String>, base_url: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

impl PeopleSource for PdlClient {
    fn name(&self) -> &'static str {
        "pdl"
    }

    fn estimate_credits(&self, query: &SearchQuery) -> u32 {
        query.size.clamp(1, MAX_PAGE)
    }

    async fn search(&self, query: &SearchQuery) -> Result<SearchPage, SourceError> {
        let key = self
            .api_key
            .as_deref()
            .ok_or(SourceError::NotConfigured("People Data Labs"))?;
        let mut body = json!({"query": query.query, "size": query.size.clamp(1, MAX_PAGE)});
        if let Some(token) = &query.scroll_token {
            body["scroll_token"] = json!(token);
        }
        let res = self
            .http
            .post(format!("{}/v5/person/search", self.base_url))
            .header("X-Api-Key", key)
            .json(&body)
            .send()
            .await
            .map_err(|e| SourceError::Network(e.without_url().to_string()))?;
        // PDL answers "nothing matched" with 404, and does not charge for it.
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(SearchPage {
                total: 0,
                records: Vec::new(),
                scroll_token: None,
                credits_used: 0,
            });
        }
        parse_search(&read_json(res).await?)
    }
}

/// Map a PDL search response to our records. Pure, so it is tested on fixtures.
pub fn parse_search(v: &Value) -> Result<SearchPage, SourceError> {
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| SourceError::BadResponse("no data array".into()))?;
    let records: Vec<PersonRecord> = data.iter().filter_map(parse_person).collect();
    Ok(SearchPage {
        total: v.get("total").and_then(Value::as_u64).unwrap_or(0),
        scroll_token: text(v, "scroll_token"),
        credits_used: (data.len() as u32).max(1),
        records,
    })
}

fn parse_person(p: &Value) -> Option<PersonRecord> {
    let source_id = text(p, "id")?;
    let experience: Vec<ExperienceRecord> = p
        .get("experience")
        .and_then(Value::as_array)
        .map(|xs| xs.iter().filter_map(parse_experience).collect())
        .unwrap_or_default();
    Some(PersonRecord {
        source_id,
        full_name: text(p, "full_name").unwrap_or_default(),
        current_title: text(p, "job_title"),
        current_employer: text(p, "job_company_name"),
        current_employer_domain: text(p, "job_company_website"),
        location: text(p, "location_name"),
        linkedin_url: text(p, "linkedin_url"),
        work_email: text(p, "work_email").and_then(|e| {
            // Only an address at a company the person works or worked for.
            let firms: Vec<String> = text(p, "job_company_website")
                .into_iter()
                .chain(
                    experience
                        .iter()
                        .filter_map(|x: &ExperienceRecord| x.employer_domain.clone()),
                )
                .filter_map(|d: String| normalise_domain(&d))
                .collect();
            work_email(&e).filter(|e| {
                let domain = e.rsplit('@').next().unwrap_or("");
                firms
                    .iter()
                    .any(|f| domain == f || domain.ends_with(&format!(".{f}")))
            })
        }),
        phones: phones(p),
        experience,
    })
}

/// Free email providers: an address there is personal, whatever field it is in.
const PERSONAL_DOMAINS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "yahoo.com",
    "hotmail.com",
    "outlook.com",
    "live.com",
    "msn.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "gmx.com",
    "mail.com",
    "yandex.com",
    "ymail.com",
    "live.co.uk",
    "pm.me",
    "protonmail.ch",
    "fastmail.com",
    "btinternet.com",
    "qq.com",
    "163.com",
    "mail.ru",
    "web.de",
    "comcast.net",
];

const PERSONAL_BRANDS: &[&str] = &[
    "gmail",
    "yahoo",
    "hotmail",
    "outlook",
    "aol",
    "gmx",
    "yandex",
    "live",
    "icloud",
    "protonmail",
];

/// A plausible work address, lower-cased; `None` for anything else, including
/// addresses at free email providers. Personal emails are never kept (SRS N9).
pub fn work_email(raw: &str) -> Option<String> {
    let e = raw.trim().to_lowercase();
    let (local, domain) = e.split_once('@')?;
    let ok = !local.is_empty()
        && domain.contains('.')
        && !e.contains(char::is_whitespace)
        && !PERSONAL_DOMAINS.contains(&domain)
        // Country versions, such as yahoo.co.uk or hotmail.fr.
        && !PERSONAL_BRANDS
            .iter()
            .any(|b| domain.starts_with(&format!("{b}.")));
    ok.then_some(e)
}

/// Mobile first, then other numbers; at most three, no repeats.
fn phones(p: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let listed = p
        .get("phone_numbers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string));
    for n in text(p, "mobile_phone").into_iter().chain(listed) {
        let n = n.trim().to_string();
        if n.chars().filter(char::is_ascii_digit).count() >= 7 && !out.contains(&n) {
            out.push(n);
        }
    }
    out.truncate(3);
    out
}

fn parse_experience(x: &Value) -> Option<ExperienceRecord> {
    let employer = x.get("company").and_then(|c| text(c, "name"))?;
    Some(ExperienceRecord {
        employer,
        employer_domain: x.get("company").and_then(|c| text(c, "website")),
        title: x.get("title").and_then(|t| text(t, "name")),
        start: text(x, "start_date"),
        end: text(x, "end_date"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    /// Synthetic data only. Real candidate records never go in the repo.
    fn fixture() -> Value {
        json!({
          "status": 200,
          "total": 245,
          "scroll_token": "abc123",
          "data": [
            {
              "id": "pdl-1",
              "full_name": "alex example",
              "job_title": "senior security engineer",
              "job_company_name": "examplepay",
              "job_company_website": "examplepay.com",
              "location_name": true,
              "linkedin_url": "linkedin.com/in/alex-example",
              "work_email": "Alex.Example@ExamplePay.com",
              "personal_emails": ["alex.personal@gmail.com"],
              "emails": [{"address": "alex.personal@gmail.com", "type": "personal"}],
              "mobile_phone": "+1 212 555 0100",
              "phone_numbers": ["+1 212 555 0100", "+1 646 555 0199"],
              "experience": [
                {"company": {"name": "examplepay", "website": "examplepay.com"}, "title": {"name": "senior security engineer"}, "start_date": "2022-01", "end_date": null},
                {"company": {"name": "samplebank"}, "title": {"name": "iam engineer"}, "start_date": "2018", "end_date": "2021-12"},
                {"company": null, "title": {"name": "contractor"}}
              ]
            },
            {"full_name": "no id, skipped"},
            {"id": "pdl-2", "full_name": "sam sample", "job_title": true, "experience": true,
             "work_email": true, "mobile_phone": true, "phone_numbers": true, "personal_emails": true}
          ]
        })
    }

    #[test]
    fn parses_records_and_skips_bad_ones() {
        let page = parse_search(&fixture()).unwrap();
        assert_eq!(page.total, 245);
        assert_eq!(page.scroll_token.as_deref(), Some("abc123"));
        assert_eq!(page.credits_used, 3, "PDL charges per record returned");
        assert_eq!(page.records.len(), 2);

        let a = &page.records[0];
        assert_eq!(a.source_id, "pdl-1");
        assert_eq!(a.current_employer.as_deref(), Some("examplepay"));
        assert_eq!(a.current_employer_domain.as_deref(), Some("examplepay.com"));
        assert_eq!(
            a.experience[0].employer_domain.as_deref(),
            Some("examplepay.com")
        );
        assert_eq!(a.location, None, "masked on the free tier");
        assert_eq!(a.experience.len(), 2, "entry without a company is dropped");
        assert_eq!(a.experience[1].end.as_deref(), Some("2021-12"));

        assert_eq!(a.work_email.as_deref(), Some("alex.example@examplepay.com"));
        let mut other = fixture();
        other["data"][0]["work_email"] = json!("alex@some-other-firm.com");
        let page = parse_search(&other).unwrap();
        assert_eq!(
            page.records[0].work_email, None,
            "not at a company they work for"
        );
        assert_eq!(a.phones, ["+1 212 555 0100", "+1 646 555 0199"]);
        let all = format!("{a:?}");
        assert!(!all.contains("personal"), "personal emails are never read");

        let b = &page.records[1];
        assert_eq!(
            (b.work_email.as_deref(), b.phones.len()),
            (None, 0),
            "masked on free plans"
        );
        assert_eq!(b.current_title, None);
        assert!(b.experience.is_empty());
    }

    #[test]
    fn only_real_work_emails_are_kept() {
        assert_eq!(work_email(" A@Firm.io ").as_deref(), Some("a@firm.io"));
        assert_eq!(
            work_email("a@mail.firm.com").as_deref(),
            Some("a@mail.firm.com")
        );
        assert_eq!(work_email("a@me.firm.io").as_deref(), Some("a@me.firm.io"));
        for bad in [
            "someone@gmail.com",
            "x@me.com",
            "x@yahoo.co.uk",
            "x@hotmail.fr",
            "no-at-sign",
            "a b@firm.io",
            "@firm.io",
            "a@firm",
        ] {
            assert_eq!(work_email(bad), None, "{bad}");
        }
    }

    #[test]
    fn empty_result_still_costs_a_credit() {
        let page = parse_search(&json!({"data": [], "total": 0})).unwrap();
        assert_eq!((page.records.len(), page.credits_used), (0, 1));
    }

    #[test]
    fn missing_data_is_an_error() {
        assert!(matches!(
            parse_search(&json!({"error": "x"})),
            Err(SourceError::BadResponse(_))
        ));
    }

    #[test]
    fn estimate_is_clamped_to_one_page() {
        let c = PdlClient::new(None);
        let q = |size| SearchQuery {
            query: json!({}),
            size,
            scroll_token: None,
        };
        assert_eq!(c.estimate_credits(&q(0)), 1);
        assert_eq!(c.estimate_credits(&q(30)), 30);
        assert_eq!(c.estimate_credits(&q(500)), 100);
    }

    #[tokio::test]
    async fn no_key_means_no_call() {
        let c = PdlClient::with_base_url(None, "http://127.0.0.1:9");
        let q = SearchQuery {
            query: json!({"match_all": {}}),
            size: 5,
            scroll_token: None,
        };
        assert!(matches!(
            c.search(&q).await,
            Err(SourceError::NotConfigured(_))
        ));
    }

    type Seen = Arc<Mutex<Option<(String, Value)>>>;

    async fn fake_pdl(
        State(seen): State<Seen>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let key = headers
            .get("x-api-key")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        *seen.lock().unwrap() = Some((key, body));
        Json(fixture())
    }

    #[tokio::test]
    async fn sends_key_query_and_clamped_size() {
        let seen: Seen = Arc::default();
        let app = Router::new()
            .route("/v5/person/search", post(fake_pdl))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let c = PdlClient::with_base_url(Some("test-key".into()), &format!("http://{addr}/"));
        let q = SearchQuery {
            query: json!({"match_all": {}}),
            size: 500,
            scroll_token: Some("t1".into()),
        };
        let page = c.search(&q).await.unwrap();
        assert_eq!(page.records.len(), 2);

        let (key, body) = seen.lock().unwrap().clone().unwrap();
        assert_eq!(key, "test-key");
        assert_eq!(body["size"], 100);
        assert_eq!(body["query"], json!({"match_all": {}}));
        assert_eq!(body["scroll_token"], "t1");
    }

    #[tokio::test]
    async fn no_match_is_empty_and_free() {
        let app = Router::new().route(
            "/v5/person/search",
            post(|| async {
                (
                    axum::http::StatusCode::NOT_FOUND,
                    "{\"error\": \"no records\"}",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let c = PdlClient::with_base_url(Some("k".into()), &format!("http://{addr}"));
        let q = SearchQuery {
            query: json!({}),
            size: 1,
            scroll_token: None,
        };
        let page = c.search(&q).await.unwrap();
        assert_eq!(
            (page.total, page.records.len(), page.credits_used),
            (0, 0, 0)
        );
    }

    #[tokio::test]
    async fn provider_error_is_reported_without_the_key() {
        let app = Router::new().route(
            "/v5/person/search",
            post(|| async { (axum::http::StatusCode::PAYMENT_REQUIRED, "out of credits") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let c = PdlClient::with_base_url(Some("secret-key".into()), &format!("http://{addr}"));
        let q = SearchQuery {
            query: json!({}),
            size: 1,
            scroll_token: None,
        };
        let err = c.search(&q).await.unwrap_err();
        assert!(matches!(err, SourceError::Http { status: 402, .. }));
        assert!(!err.to_string().contains("secret-key"));
    }
}
