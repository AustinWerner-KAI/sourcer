//! Recruitly, the team's CRM and ATS (D10). A thin client for the calls
//! Sourcer makes: jobs and companies (to start roles), candidate search (the
//! check at shortlist), and creating candidates, pipeline entries and notes
//! (the handover).
//!
//! - The API key goes in the query string, as Recruitly requires, so request
//!   addresses are never logged or shown: errors are built without them.
//! - Every call is counted per organisation per day before it is made, and
//!   refused past the daily cap, so the plan's limit is never reached.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

const DEFAULT_BASE_URL: &str = "https://api.recruitly.io";
/// Below the Professional plan's 10,000 a day, leaving room for Recruitly's own use.
pub const DEFAULT_DAILY_CAP: i64 = 9_000;
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecruitlyError {
    NotConfigured,
    /// Our own daily cap, or Recruitly's limit (429).
    Limit,
    /// The key is wrong, switched off, or lacks permission (401, 403).
    Refused,
    NotFound,
    Http(u16, String),
    Network(String),
    BadResponse(String),
}

impl std::fmt::Display for RecruitlyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("Recruitly is not set up yet (no API key)."),
            Self::Limit => {
                f.write_str("Recruitly's daily call limit is reached. Try again tomorrow.")
            }
            Self::Refused => {
                f.write_str("Recruitly refused the API key. Check it in the settings file.")
            }
            Self::NotFound => f.write_str("Recruitly could not find that record."),
            Self::Http(s, m) if m.is_empty() => write!(f, "Recruitly answered {s}."),
            Self::Http(s, m) => write!(f, "Recruitly answered {s}: {m}"),
            Self::Network(e) => write!(f, "Could not reach Recruitly ({e})."),
            Self::BadResponse(e) => write!(f, "Recruitly sent something unexpected ({e})."),
        }
    }
}

impl std::error::Error for RecruitlyError {}

/// Recruitly's own message, cut short, with the key taken out in case it echoes the address.
fn scrub(message: &str, key: &str) -> String {
    let cleaned = if key.is_empty() {
        message.to_string()
    } else {
        message.replace(key, "[key]")
    };
    cleaned.chars().take(200).collect()
}

type R<T> = Result<T, RecruitlyError>;

pub struct Recruitly {
    http: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    pub daily_cap: i64,
}

impl Recruitly {
    pub fn new(api_key: Option<String>, daily_cap: Option<i64>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL, daily_cap)
    }

    pub fn with_base_url(api_key: Option<String>, base_url: &str, daily_cap: Option<i64>) -> Self {
        Self {
            // No redirects: the key is in the address and must go nowhere else.
            http: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                // Only fails if TLS cannot start; never fall back to a client that follows redirects.
                .expect("Recruitly HTTP client"),
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            daily_cap: daily_cap.unwrap_or(DEFAULT_DAILY_CAP).max(0),
        }
    }

    pub fn configured(&self) -> bool {
        self.api_key.is_some()
    }

    /// Calls for this organisation, counted and capped.
    pub fn session<'a>(&'a self, pool: &'a PgPool, org_id: Uuid) -> Session<'a> {
        Session {
            rc: self,
            pool,
            org_id,
        }
    }
}

/// Calls made today by this organisation.
pub async fn calls_today(pool: &PgPool, org_id: Uuid) -> anyhow::Result<i64> {
    let n: Option<i32> = sqlx::query_scalar(
        "SELECT calls FROM recruitly_usage WHERE org_id = $1 AND day = current_date",
    )
    .bind(org_id)
    .fetch_optional(pool)
    .await?;
    Ok(n.unwrap_or(0) as i64)
}

/// A Recruitly id as used in a path. Ids come from Recruitly, but some pass
/// through the browser, so anything else is refused rather than sent.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub struct Session<'a> {
    rc: &'a Recruitly,
    pool: &'a PgPool,
    org_id: Uuid,
}

impl Session<'_> {
    async fn spend(&self) -> R<()> {
        let counted: Result<Option<i32>, _> = sqlx::query_scalar(
            "INSERT INTO recruitly_usage (org_id, day, calls) VALUES ($1, current_date, 1)
             ON CONFLICT (org_id, day) DO UPDATE SET calls = recruitly_usage.calls + 1
             WHERE recruitly_usage.calls < $2
             RETURNING calls",
        )
        .bind(self.org_id)
        .bind(self.rc.daily_cap as i32)
        .fetch_optional(self.pool)
        .await;
        match counted {
            Ok(Some(n)) if (n as i64) <= self.rc.daily_cap => Ok(()),
            Ok(_) => Err(RecruitlyError::Limit),
            Err(e) => Err(RecruitlyError::Network(format!("usage count failed: {e}"))),
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> R<Value> {
        let key = self
            .rc
            .api_key
            .as_deref()
            .ok_or(RecruitlyError::NotConfigured)?;
        self.spend().await?;
        let mut req = self
            .rc
            .http
            .request(method, format!("{}{path}", self.rc.base_url))
            .query(&[("apiKey", key)])
            .query(query);
        if let Some(b) = body {
            req = req.json(b);
        }
        // Never keep the address: it holds the key.
        let res = req
            .send()
            .await
            .map_err(|e| RecruitlyError::Network(e.without_url().to_string()))?;
        let status = res.status().as_u16();
        let text = res
            .text()
            .await
            .map_err(|e| RecruitlyError::Network(e.without_url().to_string()))?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let message = v
            .get("message")
            .or_else(|| v.get("error"))
            .and_then(Value::as_str)
            .map(|m| scrub(m, key))
            .unwrap_or_default();
        match status {
            200..=299 => {}
            401 | 403 => return Err(RecruitlyError::Refused),
            404 => return Err(RecruitlyError::NotFound),
            429 => return Err(RecruitlyError::Limit),
            s => return Err(RecruitlyError::Http(s, message)),
        }
        if v.is_null() {
            return Err(RecruitlyError::BadResponse("not JSON".into()));
        }
        if v.get("success") == Some(&Value::Bool(false)) {
            return Err(RecruitlyError::Http(status, message));
        }
        Ok(v.get("data").cloned().unwrap_or(v))
    }

    async fn get(&self, path: &str, query: &[(&str, &str)]) -> R<Value> {
        self.call(reqwest::Method::GET, path, query, None).await
    }

    async fn post(&self, path: &str, body: &Value) -> R<Value> {
        self.call(reqwest::Method::POST, path, &[], Some(body))
            .await
    }

    /// Who the key belongs to, for the "Test connection" button.
    pub async fn me(&self) -> R<User> {
        let v = self.get("/api/nova/users/me", &[]).await?;
        Ok(user(&v))
    }

    pub async fn users(&self) -> R<Vec<User>> {
        let v = self.get("/api/nova/users", &[]).await?;
        Ok(items(&v).iter().map(user).collect())
    }

    /// Jobs matching the words, newest first. Blank lists the newest jobs.
    pub async fn search_jobs(&self, words: &str) -> R<Vec<JobHit>> {
        let v = self
            .get(
                "/api/nova/jobs/search",
                &[("query", words.trim()), ("size", "20")],
            )
            .await?;
        Ok(items(&v).iter().filter_map(job_hit).collect())
    }

    pub async fn job(&self, id: &str) -> R<Job> {
        if !valid_id(id) {
            return Err(RecruitlyError::NotFound);
        }
        let v = self.get(&format!("/api/nova/jobs/{id}"), &[]).await?;
        job(&v).ok_or_else(|| RecruitlyError::BadResponse("job without an id".into()))
    }

    pub async fn company(&self, id: &str) -> R<Company> {
        if !valid_id(id) {
            return Err(RecruitlyError::NotFound);
        }
        let v = self.get(&format!("/api/nova/companies/{id}"), &[]).await?;
        Ok(Company {
            name: text(&v, "name").unwrap_or_default(),
            domain: text(&v, "domain").or_else(|| text(&v, "website")),
        })
    }

    pub async fn search_candidates(&self, words: &str) -> R<Vec<CandidateHit>> {
        let v = self
            .get(
                "/api/nova/candidates/search",
                &[("query", words.trim()), ("size", "10")],
            )
            .await?;
        Ok(items(&v).iter().filter_map(candidate_hit).collect())
    }

    pub async fn candidate(&self, id: &str) -> R<CandidateHit> {
        if !valid_id(id) {
            return Err(RecruitlyError::NotFound);
        }
        let v = self.get(&format!("/api/nova/candidates/{id}"), &[]).await?;
        candidate_hit(&v)
            .ok_or_else(|| RecruitlyError::BadResponse("candidate without an id".into()))
    }

    /// Returns the new candidate's id.
    pub async fn create_candidate(&self, c: &NewCandidate) -> R<String> {
        let body =
            serde_json::to_value(c).map_err(|e| RecruitlyError::BadResponse(e.to_string()))?;
        let v = self.post("/api/nova/candidates", &body).await?;
        created_id(&v)
    }

    /// Returns the pipeline entry's id.
    pub async fn add_to_pipeline(&self, job_id: &str, candidate_id: &str) -> R<String> {
        if !valid_id(job_id) || !valid_id(candidate_id) {
            return Err(RecruitlyError::NotFound);
        }
        let v = self
            .post(
                &format!("/api/nova/jobs/{job_id}/pipeline"),
                &serde_json::json!({ "candidateId": candidate_id }),
            )
            .await?;
        created_id(&v)
    }

    pub async fn add_note(&self, record_id: &str, note: &str) -> R<()> {
        if !valid_id(record_id) {
            return Err(RecruitlyError::NotFound);
        }
        self.post(
            &format!("/api/nova/journal/{record_id}"),
            &serde_json::json!({ "note": note }),
        )
        .await?;
        Ok(())
    }
}

// ---------- What Sourcer reads from Recruitly ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Option<String>,
    pub name: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobHit {
    pub id: String,
    pub title: String,
    pub reference: Option<String>,
    pub company: Option<String>,
    pub status: Option<String>,
    pub location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub title: String,
    pub reference: Option<String>,
    pub company_id: Option<String>,
    pub company_name: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub pay: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Company {
    pub name: String,
    /// The domain, or failing that the website, as Recruitly holds it.
    pub domain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateHit {
    pub id: String,
    pub name: String,
    pub emails: Vec<String>,
    pub linkedin: Option<String>,
    pub employer: Option<String>,
    pub status: Option<String>,
    pub owner: Option<String>,
    pub owner_id: Option<String>,
    pub last_activity: Option<String>,
    pub placed: bool,
    /// `None` when the response did not say.
    pub do_not_contact: Option<bool>,
}

/// What Sourcer sends to create a candidate. Only work details: no personal
/// email is ever held, so none can be sent.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewCandidate {
    pub first_name: String,
    pub last_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mobile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_in: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_employer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
}

/// A non-empty string, or a number, as text.
fn text(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The records in a list response, wherever Recruitly put them.
fn items(v: &Value) -> Vec<Value> {
    if let Some(a) = v.as_array() {
        return a.clone();
    }
    for key in ["content", "items", "records", "results", "data", "list"] {
        if let Some(a) = v.get(key).and_then(Value::as_array) {
            return a.clone();
        }
    }
    Vec::new()
}

/// The new record's id: `data` as a string, or an object holding `id`.
fn created_id(v: &Value) -> R<String> {
    let id = match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => text(v, "id"),
    };
    id.filter(|s| valid_id(s))
        .ok_or_else(|| RecruitlyError::BadResponse("no id for the new record".into()))
}

fn full_name(v: &Value) -> String {
    let first = text(v, "firstName").unwrap_or_default();
    let last = text(v, "lastName").unwrap_or_default();
    let joined = format!("{first} {last}").trim().to_string();
    if joined.is_empty() {
        text(v, "name")
            .or_else(|| text(v, "fullName"))
            .unwrap_or_default()
    } else {
        joined
    }
}

fn user(v: &Value) -> User {
    User {
        id: text(v, "id"),
        name: full_name(v),
        email: text(v, "email"),
    }
}

fn job_hit(v: &Value) -> Option<JobHit> {
    Some(JobHit {
        id: text(v, "id").filter(|i| valid_id(i))?,
        title: text(v, "title").unwrap_or_else(|| "Untitled job".into()),
        reference: text(v, "reference"),
        company: text(v, "companyName"),
        status: text(v, "statusName"),
        location: location(v),
    })
}

fn location(v: &Value) -> Option<String> {
    match v.get("location") {
        Some(Value::Object(_)) => {
            let l = &v["location"];
            let parts: Vec<String> = ["cityName", "city", "regionName", "countryName", "country"]
                .iter()
                .filter_map(|k| text(l, k))
                .collect();
            let mut seen: Vec<String> = Vec::new();
            for p in parts {
                if !seen.contains(&p) {
                    seen.push(p);
                }
            }
            (!seen.is_empty()).then(|| seen.join(", "))
        }
        _ => text(v, "location"),
    }
}

fn job(v: &Value) -> Option<Job> {
    let hit = job_hit(v)?;
    let pay = {
        let min = text(v, "minPay").filter(|p| p != "0");
        let max = text(v, "maxPay").filter(|p| p != "0");
        let range = match (min, max) {
            (Some(a), Some(b)) if a != b => Some(format!("{a} to {b}")),
            (Some(a), _) | (None, Some(a)) => Some(a),
            _ => None,
        };
        range.map(|r| {
            [Some(r), text(v, "payCurrency"), text(v, "payTenure")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ")
        })
    };
    Some(Job {
        id: hit.id,
        title: hit.title,
        reference: hit.reference,
        company_id: text(v, "companyId").filter(|i| valid_id(i)),
        company_name: hit.company,
        description: text(v, "description"),
        location: hit.location,
        pay,
    })
}

fn candidate_hit(v: &Value) -> Option<CandidateHit> {
    let mut emails: Vec<String> = ["email", "alternateEmail", "workEmail"]
        .iter()
        .filter_map(|k| text(v, k))
        .map(|e| e.to_lowercase())
        .collect();
    emails.dedup();
    Some(CandidateHit {
        id: text(v, "id").filter(|i| valid_id(i))?,
        name: full_name(v),
        emails,
        linkedin: text(v, "linkedIn").or_else(|| text(v, "linkedin")),
        employer: text(v, "currentEmployer"),
        status: text(v, "statusName"),
        owner: text(v, "ownerName"),
        owner_id: text(v, "ownerId"),
        last_activity: text(v, "lastActivityDate").or_else(|| text(v, "updatedOn")),
        placed: v.get("placed").and_then(Value::as_bool).unwrap_or(false),
        do_not_contact: v.get("doNotContact").and_then(Value::as_bool),
    })
}

/// A job description as plain text. Recruitly may hold HTML.
pub fn plain_text(raw: &str) -> String {
    let mut s = raw.replace("\r\n", "\n");
    for (from, to) in [
        ("<br>", "\n"),
        ("<br/>", "\n"),
        ("<br />", "\n"),
        ("</p>", "\n\n"),
        ("</div>", "\n"),
        ("</li>", "\n"),
        ("<li>", "- "),
        ("</h1>", "\n\n"),
        ("</h2>", "\n\n"),
        ("</h3>", "\n\n"),
        ("</h4>", "\n\n"),
        ("</tr>", "\n"),
    ] {
        s = replace_ci(&s, from, to);
    }
    // Drop every other tag.
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    for (from, to) in [
        ("&nbsp;", " "),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&rsquo;", "'"),
        ("&lsquo;", "'"),
        ("&ldquo;", "\""),
        ("&rdquo;", "\""),
        ("&ndash;", "-"),
        ("&mdash;", "-"),
        ("&bull;", "-"),
        ("&amp;", "&"),
    ] {
        out = out.replace(from, to);
    }
    // Tidy spacing: trim lines, at most one blank line in a row.
    let mut lines: Vec<String> = Vec::new();
    for line in out.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() && lines.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        let line = if line.starts_with("- ") || line == "-" {
            line
        } else {
            line.trim_start_matches(['-', ' ']).to_string()
        };
        lines.push(line);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn replace_ci(s: &str, from: &str, to: &str) -> String {
    let lower = s.to_lowercase();
    // Lower-casing can change lengths outside ASCII; fall back to exact matches.
    if lower.len() != s.len() {
        return s.replace(from, to).replace(&from.to_uppercase(), to);
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(at) = lower[i..].find(from) {
        out.push_str(&s[i..i + at]);
        out.push_str(to);
        i += at + from.len();
    }
    out.push_str(&s[i..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn descriptions_become_plain_text() {
        let html = "<h2>About</h2><p>Lead <b>IAM</b> &amp; PAM.</p><ul><li>CyberArk</li><li>Okta</li></ul><P>Dubai&nbsp;based</P>";
        assert_eq!(
            plain_text(html),
            "About\n\nLead IAM & PAM.\n\n- CyberArk\n- Okta\nDubai based"
        );
        assert_eq!(plain_text("Plain\n\n\n\ntext  here"), "Plain\n\ntext here");
    }

    #[test]
    fn list_responses_are_read_wherever_the_records_are() {
        for v in [
            json!([{"id": "a1", "title": "X"}]),
            json!({"content": [{"id": "a1", "title": "X"}]}),
            json!({"items": [{"id": "a1", "title": "X"}]}),
        ] {
            assert_eq!(items(&v).len(), 1);
        }
        assert!(items(&json!({"total": 0})).is_empty());
    }

    #[test]
    fn jobs_read_with_pay_and_location() {
        let v = json!({"id": "j1", "title": "Senior IAM Engineer", "reference": "J-1042",
            "companyId": "c9", "companyName": "Client A", "description": "<p>Spec</p>",
            "location": {"cityName": "Dubai", "countryName": "United Arab Emirates"},
            "minPay": 30000, "maxPay": 40000, "payCurrency": "AED", "payTenure": "Monthly"});
        let j = job(&v).unwrap();
        assert_eq!(j.company_id.as_deref(), Some("c9"));
        assert_eq!(j.location.as_deref(), Some("Dubai, United Arab Emirates"));
        assert_eq!(j.pay.as_deref(), Some("30000 to 40000 AED Monthly"));
        let bad = json!({"id": "../x", "title": "Y"});
        assert!(job(&bad).is_none(), "odd ids are refused");
    }

    #[test]
    fn candidates_read_their_emails_and_flags() {
        let v = json!({"id": "c1", "firstName": "Sample", "lastName": "Person",
            "email": "Sample@Example.com", "linkedIn": "https://www.linkedin.com/in/sample-person/",
            "statusName": "Interviewing", "ownerName": "Teo", "placed": false, "doNotContact": true});
        let c = candidate_hit(&v).unwrap();
        assert_eq!(c.name, "Sample Person");
        assert_eq!(c.emails, ["sample@example.com"]);
        assert_eq!(c.do_not_contact, Some(true));
        assert_eq!(
            candidate_hit(&json!({"id": 42, "name": "N"})).unwrap().id,
            "42"
        );
    }

    #[test]
    fn new_candidates_send_only_what_is_known() {
        let c = NewCandidate {
            first_name: "Sample".into(),
            last_name: "Person".into(),
            email: None,
            mobile: None,
            linked_in: Some("https://www.linkedin.com/in/x".into()),
            job_title: None,
            current_employer: None,
            owner_id: None,
        };
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({"firstName": "Sample", "lastName": "Person", "linkedIn": "https://www.linkedin.com/in/x"})
        );
        assert_eq!(created_id(&json!("abc-1")).unwrap(), "abc-1");
        assert_eq!(created_id(&json!({"id": "abc-2"})).unwrap(), "abc-2");
        assert!(created_id(&json!({})).is_err());
    }

    #[test]
    fn errors_never_show_the_key() {
        let echoed = "Bad request for /api/candidates?apiKey=sk-secret-123&query=x";
        let e = RecruitlyError::Http(400, scrub(echoed, "sk-secret-123"));
        assert!(!e.to_string().contains("sk-secret-123"));
        assert!(e.to_string().contains("[key]"));
        assert_eq!(scrub(&"x".repeat(500), "k").len(), 200);
    }
}
