//! The Recruitly link (D10): roles from Recruitly jobs, the Recruitly check at
//! shortlist, and handing shortlisted people over to Recruitly.
//!
//! - A job is read into the new-role form first; nothing is saved until the
//!   resourcer checks it and saves, as with a pasted spec.
//! - Each person is checked once when shortlisted, and again just before they
//!   are sent, so the flag is never stale. A sure match (same LinkedIn or
//!   email) is remembered; a match on name only is shown as "possible".
//! - Marked "do not contact" in Recruitly means never shortlisted or sent.
//! - Sending someone a colleague owns in Recruitly asks first.
//! - A handover saves each step as it succeeds, so a retry never creates a
//!   second Recruitly record, and two clicks never run it twice.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    app::AppState,
    audit,
    auth::CurrentUser,
    candidates::one_row,
    domain::{
        CandidacyState, Handover, HandoverResult, ImportRole, JobPreview, LinkJob, RecruitlyJob,
        RecruitlyStatus, RecruitlyTest,
    },
    employer::{normalise_domain, normalise_name},
    people::profile_url,
    recruitly::{self, CandidateHit, Job, NewCandidate, Recruitly, RecruitlyError, Session},
    roles::{clean_text, role_detail, MAX_SPEC_CHARS},
    team::Admin,
};

/// Work emails searched for per person, at most.
const MAX_EMAILS_SEARCHED: usize = 2;

fn refuse(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, msg.into()).into_response()
}

fn server_error(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "Recruitly request failed");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

fn rc_error(e: &RecruitlyError) -> Response {
    let code = match e {
        RecruitlyError::NotConfigured => StatusCode::SERVICE_UNAVAILABLE,
        RecruitlyError::Limit => StatusCode::TOO_MANY_REQUESTS,
        RecruitlyError::NotFound => StatusCode::NOT_FOUND,
        _ => StatusCode::BAD_GATEWAY,
    };
    tracing::warn!(error = %e, "Recruitly call failed");
    refuse(code, e.to_string())
}

/// A Recruitly error as its own answer; anything else is a server error.
fn any_error(e: anyhow::Error) -> Response {
    match e.downcast_ref::<RecruitlyError>() {
        Some(rc) => rc_error(rc),
        None => server_error(e),
    }
}

fn unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.code())
        .is_some_and(|c| c == "23505")
}

/// The pool, or the answer to give when Sourcer or Recruitly is not set up.
#[allow(clippy::result_large_err)]
fn ready(state: &AppState) -> Result<&PgPool, Response> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| StatusCode::SERVICE_UNAVAILABLE.into_response())?;
    if !state.recruitly.configured() {
        return Err(rc_error(&RecruitlyError::NotConfigured));
    }
    Ok(pool)
}

// ---------- Status ----------

/// GET /api/recruitly/status: whether it is set up and today's calls. No call is made.
pub async fn status(State(state): State<AppState>, user: CurrentUser) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match recruitly::calls_today(pool, user.org_id).await {
        Ok(calls_today) => Json(RecruitlyStatus {
            configured: state.recruitly.configured(),
            calls_today,
            daily_cap: state.recruitly.daily_cap,
        })
        .into_response(),
        Err(e) => server_error(e),
    }
}

/// POST /api/recruitly/test: one call, to show whose key it is. Admins only.
pub async fn test(State(state): State<AppState>, Admin(user): Admin) -> Response {
    let pool = match ready(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match state.recruitly.session(pool, user.org_id).me().await {
        Ok(me) => Json(RecruitlyTest {
            connected_as: if me.name.is_empty() {
                me.email.unwrap_or_else(|| "an unnamed user".into())
            } else {
                me.name
            },
        })
        .into_response(),
        Err(e) => rc_error(&e),
    }
}

// ---------- Jobs and roles ----------

#[derive(Debug, Deserialize)]
pub struct JobsQuery {
    #[serde(default)]
    pub q: String,
}

/// GET /api/recruitly/jobs?q=: newest jobs matching the words. One call.
pub async fn jobs(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<JobsQuery>,
) -> Response {
    let pool = match ready(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let words: String = q.q.trim().chars().take(100).collect();
    let hits = match state
        .recruitly
        .session(pool, user.org_id)
        .search_jobs(&words)
        .await
    {
        Ok(h) => h,
        Err(e) => return rc_error(&e),
    };
    let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
    let made: Result<Vec<(String, Uuid)>, _> = sqlx::query_as(
        "SELECT recruitly_job_id, id FROM role WHERE org_id = $1 AND recruitly_job_id = ANY($2)",
    )
    .bind(user.org_id)
    .bind(&ids)
    .fetch_all(pool)
    .await;
    let made = match made {
        Ok(m) => m,
        Err(e) => return server_error(e),
    };
    Json(
        hits.into_iter()
            .map(|h| RecruitlyJob {
                role_id: made.iter().find(|m| m.0 == h.id).map(|m| m.1),
                id: h.id,
                title: h.title,
                reference: h.reference,
                company: h.company,
                status: h.status,
                location: h.location,
            })
            .collect::<Vec<_>>(),
    )
    .into_response()
}

/// What the screens call a job: "Senior IAM Engineer (J-1042)".
fn job_label(job: &Job) -> String {
    let title: String = job.title.chars().take(150).collect();
    match &job.reference {
        Some(r) => format!("{title} ({})", r.chars().take(40).collect::<String>()),
        None => title,
    }
}

/// The job as a spec: the description as plain text, then location, pay and
/// the Recruitly reference.
pub fn spec_from(job: &Job) -> String {
    let mut out = job
        .description
        .as_deref()
        .map(recruitly::plain_text)
        .unwrap_or_default();
    let facts: Vec<String> = [
        job.location.as_ref().map(|l| format!("Location: {l}")),
        job.remote.then(|| "Remote working: yes".to_string()),
        job.experience.as_ref().map(|e| format!("Level: {e}")),
        job.employment.as_ref().map(|e| format!("Type: {e}")),
        (!job.skills.is_empty()).then(|| format!("Skills: {}", job.skills.join(", "))),
        job.pay.as_ref().map(|p| format!("Pay: {p}")),
        job.reference
            .as_ref()
            .map(|r| format!("Recruitly job: {r}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !facts.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&facts.join("\n"));
    }
    out.chars().take(MAX_SPEC_CHARS).collect()
}

/// The Sourcer client for a Recruitly company: by Recruitly id, then by web
/// domain, then by name.
async fn find_client(
    pool: &PgPool,
    org_id: Uuid,
    company_id: Option<&str>,
    domain: Option<&str>,
    name: Option<&str>,
) -> anyhow::Result<Option<Uuid>> {
    let clients: Vec<(Uuid, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, name, domain, recruitly_company_id FROM client WHERE org_id = $1",
    )
    .bind(org_id)
    .fetch_all(pool)
    .await?;
    if let Some(cid) = company_id {
        if let Some(c) = clients.iter().find(|c| c.3.as_deref() == Some(cid)) {
            return Ok(Some(c.0));
        }
    }
    if let Some(d) = domain.and_then(normalise_domain) {
        if let Some(c) = clients.iter().find(|c| c.2.as_deref() == Some(d.as_str())) {
            return Ok(Some(c.0));
        }
    }
    if let Some(n) = name.map(normalise_name).filter(|n| !n.is_empty()) {
        if let Some(c) = clients.iter().find(|c| normalise_name(&c.1) == n) {
            return Ok(Some(c.0));
        }
    }
    Ok(None)
}

/// GET /api/recruitly/jobs/:id: the job read into the new-role form. Two calls.
pub async fn job_preview(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(job_id): Path<String>,
) -> Response {
    let pool = match ready(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let s = state.recruitly.session(pool, user.org_id);
    let job = match s.job(&job_id).await {
        Ok(j) => j,
        Err(e) => return rc_error(&e),
    };
    let company = match &job.company_id {
        Some(cid) => match s.company(cid).await {
            Ok(c) => Some(c),
            Err(RecruitlyError::NotFound) => None,
            Err(e) => return rc_error(&e),
        },
        None => None,
    };
    let company_name = company
        .as_ref()
        .map(|c| c.name.clone())
        .filter(|n| !n.is_empty())
        .or_else(|| job.company_name.clone());
    let company_domain = company
        .as_ref()
        .and_then(|c| c.domain.as_deref())
        .and_then(normalise_domain);
    let client_id = match find_client(
        pool,
        user.org_id,
        job.company_id.as_deref(),
        company_domain.as_deref(),
        company_name.as_deref(),
    )
    .await
    {
        Ok(c) => c,
        Err(e) => return server_error(e),
    };
    let role_id: Result<Option<Uuid>, _> =
        sqlx::query_scalar("SELECT id FROM role WHERE org_id = $1 AND recruitly_job_id = $2")
            .bind(user.org_id)
            .bind(&job.id)
            .fetch_optional(pool)
            .await;
    let role_id = match role_id {
        Ok(r) => r,
        Err(e) => return server_error(e),
    };
    Json(JobPreview {
        label: job_label(&job),
        title: job.title.chars().take(200).collect(),
        spec_text: spec_from(&job),
        id: job.id,
        company_name,
        company_domain,
        client_id,
        role_id,
    })
    .into_response()
}

/// POST /api/roles/from-recruitly: save the checked form as a role linked to the job.
pub async fn import_role(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(new): Json<ImportRole>,
) -> Response {
    let pool = match ready(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let Some(title) = clean_text(&new.title, 200) else {
        return refuse(StatusCode::BAD_REQUEST, "Enter the role title.");
    };
    if new.spec_text.chars().count() > MAX_SPEC_CHARS {
        return refuse(
            StatusCode::BAD_REQUEST,
            "The job spec is too long. Keep it under 30,000 characters.",
        );
    }
    if !recruitly::valid_id(&new.job_id) {
        return refuse(StatusCode::BAD_REQUEST, "Choose a Recruitly job.");
    }
    // Read again, so the link and label are Recruitly's, not the browser's.
    let job = match state
        .recruitly
        .session(pool, user.org_id)
        .job(&new.job_id)
        .await
    {
        Ok(j) => j,
        Err(e) => return rc_error(&e),
    };
    let result = async {
        let mut tx = pool.begin().await?;
        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO role (org_id, client_id, title, owner_id, spec_text, recruitly_job_id, recruitly_job_label)
             SELECT $1, c.id, $3, $4, $5, $6, $7 FROM client c WHERE c.id = $2 AND c.org_id = $1
             RETURNING id",
        )
        .bind(user.org_id)
        .bind(new.client_id)
        .bind(&title)
        .bind(user.id)
        .bind(new.spec_text.trim())
        .bind(&job.id)
        .bind(job_label(&job))
        .fetch_optional(&mut *tx)
        .await;
        let id = match inserted {
            Ok(Some(id)) => id,
            Ok(None) => return Ok(Err(refuse(StatusCode::BAD_REQUEST, "Choose a client."))),
            Err(e) if unique_violation(&e) => {
                return Ok(Err(refuse(
                    StatusCode::CONFLICT,
                    "A role already uses that Recruitly job.",
                )))
            }
            Err(e) => return Err(e.into()),
        };
        // Remember the Recruitly company, so the next job for it finds this client.
        if let Some(cid) = &job.company_id {
            sqlx::query(
                "UPDATE client SET recruitly_company_id = $3
                 WHERE id = $1 AND org_id = $2 AND recruitly_company_id IS NULL
                   AND NOT EXISTS (SELECT 1 FROM client o WHERE o.org_id = $2 AND o.recruitly_company_id = $3)",
            )
            .bind(new.client_id)
            .bind(user.org_id)
            .bind(cid)
            .execute(&mut *tx)
            .await?;
        }
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::ROLE_CREATED,
            &format!("role:{id}"),
        )
        .await?;
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::ROLE_IMPORTED,
            &format!("role:{id} recruitly_job:{}", job.id),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(Ok(id))
    }
    .await;
    match result {
        Ok(Ok(id)) => match role_detail(pool, user.org_id, id).await {
            Ok(Some(d)) => (StatusCode::CREATED, Json(d)).into_response(),
            Ok(None) => StatusCode::NOT_FOUND.into_response(),
            Err(e) => server_error(e),
        },
        Ok(Err(r)) => r,
        Err(e) => server_error(e),
    }
}

/// PUT /api/roles/:id/recruitly: link a role to a Recruitly job (or unlink).
/// An empty spec is filled from the job.
pub async fn link_job(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(role_id): Path<Uuid>,
    Json(link): Json<LinkJob>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let role: Result<Option<(Option<String>, Option<String>)>, _> = sqlx::query_as(
        "SELECT r.spec_text, c.recruitly_company_id FROM role r LEFT JOIN client c ON c.id = r.client_id
         WHERE r.id = $1 AND r.org_id = $2",
    )
    .bind(role_id)
    .bind(user.org_id)
    .fetch_optional(pool)
    .await;
    let (spec, client_company) = match role {
        Ok(Some(r)) => r,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return server_error(e),
    };
    let (job_id, label, fill) = match link.job_id {
        None => (None, None, None),
        Some(id) => {
            if let Err(r) = ready(&state) {
                return r;
            }
            let job = match state.recruitly.session(pool, user.org_id).job(&id).await {
                Ok(j) => j,
                Err(e) => return rc_error(&e),
            };
            if let (Some(ours), Some(theirs)) = (&client_company, &job.company_id) {
                if ours != theirs {
                    return refuse(
                        StatusCode::CONFLICT,
                        format!(
                            "That Recruitly job is for {}, not this role's client.",
                            job.company_name.as_deref().unwrap_or("another company")
                        ),
                    );
                }
            }
            let empty = spec.as_deref().map(str::trim).unwrap_or("").is_empty();
            let fill = empty.then(|| spec_from(&job));
            (Some(job.id.clone()), Some(job_label(&job)), fill)
        }
    };
    let result = async {
        let mut tx = pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE role SET recruitly_job_id = $3, recruitly_job_label = $4,
                    spec_text = COALESCE($5, spec_text)
             WHERE id = $1 AND org_id = $2",
        )
        .bind(role_id)
        .bind(user.org_id)
        .bind(&job_id)
        .bind(&label)
        .bind(&fill)
        .execute(&mut *tx)
        .await;
        match updated {
            Ok(_) => {}
            Err(e) if unique_violation(&e) => {
                return Ok(Err(refuse(
                    StatusCode::CONFLICT,
                    "That Recruitly job is already linked to another role.",
                )))
            }
            Err(e) => return Err(e.into()),
        }
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::ROLE_JOB_LINKED,
            &format!(
                "role:{role_id} recruitly_job:{}",
                job_id.as_deref().unwrap_or("none")
            ),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(Ok(()))
    }
    .await;
    match result {
        Ok(Ok(())) => match role_detail(pool, user.org_id, role_id).await {
            Ok(Some(d)) => Json(d).into_response(),
            Ok(None) => StatusCode::NOT_FOUND.into_response(),
            Err(e) => server_error(e),
        },
        Ok(Err(r)) => r,
        Err(e) => server_error(e),
    }
}

// ---------- The check ----------

/// What the check found, for the caller to act on.
#[derive(Debug, Clone, Default)]
pub struct Checked {
    /// A sure match: same LinkedIn or email.
    pub id: Option<String>,
    pub dnc: bool,
    pub owner_id: Option<String>,
    pub owner: Option<String>,
    pub status: Option<String>,
    /// No sure match, but someone with the same name.
    pub possible: bool,
}

enum Found {
    Sure(Box<CandidateHit>),
    Maybe(Vec<CandidateHit>),
    Nobody,
}

/// "2026-09-12T10:00:00Z" or milliseconds since 1970 as "12 Sep 2026".
fn day(raw: &str) -> Option<String> {
    if let Ok(d) = chrono::NaiveDate::parse_from_str(raw.get(..10)?, "%Y-%m-%d") {
        return Some(d.format("%-d %b %Y").to_string());
    }
    let ms: i64 = raw.parse().ok()?;
    chrono::DateTime::from_timestamp_millis(ms).map(|t| t.format("%-d %b %Y").to_string())
}

/// The flag the list shows, in words.
fn note(found: &Found) -> Option<String> {
    match found {
        Found::Sure(h) => {
            let parts: Vec<String> = [
                h.owner.as_ref().map(|o| format!("owned by {o}")),
                h.status.clone(),
                h.placed.then(|| "placed".to_string()),
                h.last_activity
                    .as_deref()
                    .and_then(day)
                    .map(|d| format!("last activity {d}")),
            ]
            .into_iter()
            .flatten()
            .collect();
            Some(if parts.is_empty() {
                "In Recruitly".to_string()
            } else {
                format!("In Recruitly: {}", parts.join(" · "))
            })
        }
        Found::Maybe(hits) if hits.len() == 1 => {
            let at = hits[0]
                .employer
                .as_ref()
                .map(|e| format!(" at {e}"))
                .unwrap_or_default();
            Some(format!(
                "Possible match in Recruitly: {}{at}. Check before contact.",
                hits[0].name
            ))
        }
        Found::Maybe(hits) => Some(format!(
            "{} possible matches in Recruitly by name. Check before contact.",
            hits.len()
        )),
        Found::Nobody => None,
    }
}

async fn find(
    s: &Session<'_>,
    name: &str,
    linkedin: Option<&str>,
    emails: &[String],
) -> Result<Found, RecruitlyError> {
    let same_person = |h: &CandidateHit| {
        let li = h.linkedin.as_deref().and_then(profile_url);
        (li.is_some() && li.as_deref() == linkedin) || h.emails.iter().any(|e| emails.contains(e))
    };
    let mut queries: Vec<String> = Vec::new();
    if let Some(handle) = linkedin.and_then(|l| l.strip_prefix("linkedin.com/in/")) {
        queries.push(handle.to_string());
    }
    queries.extend(emails.iter().take(MAX_EMAILS_SEARCHED).cloned());
    queries.push(name.to_string());
    let wanted = normalise_name(name);
    let mut maybe: Vec<CandidateHit> = Vec::new();
    for q in queries.iter().filter(|q| !q.trim().is_empty()) {
        let hits = s.search_candidates(q).await?;
        if let Some(h) = hits.iter().find(|h| same_person(h)) {
            let mut h = h.clone();
            // Duplicate records of the same person: one saying "do not contact" is enough.
            if hits
                .iter()
                .any(|o| same_person(o) && o.do_not_contact == Some(true))
            {
                h.do_not_contact = Some(true);
            }
            return Ok(Found::Sure(Box::new(h)));
        }
        for h in hits {
            if !wanted.is_empty()
                && normalise_name(&h.name) == wanted
                && !maybe.iter().any(|m| m.id == h.id)
            {
                maybe.push(h);
            }
        }
    }
    Ok(if maybe.is_empty() {
        Found::Nobody
    } else {
        Found::Maybe(maybe)
    })
}

/// Check one person in Recruitly and remember what was found. On failure the
/// person is marked "not checked" and the error is returned.
pub async fn check_person(
    pool: &PgPool,
    rc: &Recruitly,
    org_id: Uuid,
    actor: Option<Uuid>,
    person_id: Uuid,
) -> anyhow::Result<Checked> {
    let (name, linkedin): (String, Option<String>) =
        sqlx::query_as("SELECT full_name, linkedin_url FROM person WHERE id = $1 AND org_id = $2")
            .bind(person_id)
            .bind(org_id)
            .fetch_one(pool)
            .await?;
    let emails: Vec<String> = sqlx::query_scalar(
        "SELECT lower(value) FROM contact WHERE person_id = $1 AND org_id = $2 AND kind = 'work_email'
         ORDER BY value",
    )
    .bind(person_id)
    .bind(org_id)
    .fetch_all(pool)
    .await?;
    let s = rc.session(pool, org_id);
    let found = match find(&s, &name, linkedin.as_deref(), &emails).await {
        Ok(f) => f,
        Err(e) => {
            sqlx::query(
                "UPDATE person SET recruitly_check_failed = true, recruitly_checked_at = now() WHERE id = $1 AND org_id = $2",
            )
            .bind(person_id)
            .bind(org_id)
            .execute(pool)
            .await?;
            return Err(e.into());
        }
    };
    let mut checked = Checked {
        possible: matches!(found, Found::Maybe(_)),
        ..Checked::default()
    };
    let mut dnc_said: Option<bool> = None;
    if let Found::Sure(h) = &found {
        let mut h = h.clone();
        // Search results may not say "do not contact"; the record does. If
        // it cannot be read, the check has failed: never assume contactable.
        if h.do_not_contact.is_none() {
            match s.candidate(&h.id).await {
                Ok(full) if full.do_not_contact.is_some() => {
                    h.do_not_contact = full.do_not_contact;
                    h.owner_id = h.owner_id.or(full.owner_id);
                    h.owner = h.owner.or(full.owner);
                }
                // The record does not say: treat the check as failed, never as contactable.
                Ok(_) => {
                    sqlx::query(
                        "UPDATE person SET recruitly_check_failed = true, recruitly_checked_at = now() WHERE id = $1 AND org_id = $2",
                    )
                    .bind(person_id)
                    .bind(org_id)
                    .execute(pool)
                    .await?;
                    return Err(RecruitlyError::BadResponse(
                        "the record does not say whether they can be contacted".into(),
                    )
                    .into());
                }
                Err(e) => {
                    sqlx::query(
                        "UPDATE person SET recruitly_check_failed = true, recruitly_checked_at = now() WHERE id = $1 AND org_id = $2",
                    )
                    .bind(person_id)
                    .bind(org_id)
                    .execute(pool)
                    .await?;
                    return Err(e.into());
                }
            }
        }
        dnc_said = h.do_not_contact;
        checked = Checked {
            id: Some(h.id.clone()),
            dnc: h.do_not_contact.unwrap_or(false),
            owner_id: h.owner_id.clone(),
            owner: h.owner.clone(),
            status: h.status.clone(),
            possible: false,
        };
    }
    let what = match &found {
        Found::Sure(_) => "sure",
        Found::Maybe(_) => "possible",
        Found::Nobody => "none",
    };
    let mut tx = pool.begin().await?;
    // "Do not contact" changes only when Recruitly said so for a sure match;
    // no match, or a match that did not say, keeps what was known.
    let dnc_known = match &found {
        Found::Sure(_) => dnc_said,
        _ => None,
    };
    sqlx::query(
        "UPDATE person SET recruitly_id = COALESCE($2, recruitly_id), recruitly_owner_id = $3,
                recruitly_note = $4,
                -- Recruitly can clear its own flag only on the record it set it on.
                -- A different record (a duplicate) can add the flag, never remove it.
                recruitly_dnc = CASE
                    WHEN $5 IS NULL THEN recruitly_dnc
                    WHEN recruitly_id IS NOT NULL AND recruitly_id IS DISTINCT FROM $2 THEN recruitly_dnc OR $5
                    ELSE $5 END,
                recruitly_check_failed = false, recruitly_checked_at = now()
         WHERE id = $1 AND org_id = $6",
    )
    .bind(person_id)
    .bind(&checked.id)
    .bind(&checked.owner_id)
    .bind(note(&found))
    .bind(dnc_known)
    .bind(org_id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        org_id,
        actor,
        audit::action::PERSON_RECRUITLY_CHECKED,
        &format!("person:{person_id} found:{what}"),
    )
    .await?;
    tx.commit().await?;
    Ok(checked)
}

/// POST /api/candidates/:id/recruitly-check
pub async fn check_again(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(candidacy): Path<Uuid>,
) -> Response {
    let pool = match ready(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let person: Result<Option<Uuid>, _> =
        sqlx::query_scalar("SELECT person_id FROM candidacy WHERE id = $1 AND org_id = $2")
            .bind(candidacy)
            .bind(user.org_id)
            .fetch_optional(pool)
            .await;
    let person = match person {
        Ok(Some(p)) => p,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return server_error(e),
    };
    if let Err(e) = check_person(pool, &state.recruitly, user.org_id, Some(user.id), person).await {
        return any_error(e);
    }
    match one_row(pool, user.org_id, candidacy).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => server_error(e),
    }
}

// ---------- Handover ----------

/// The Recruitly user with this email, if any. One call.
async fn recruitly_user(s: &Session<'_>, email: &str) -> Option<String> {
    let email = email.trim().to_lowercase();
    s.users()
        .await
        .ok()?
        .into_iter()
        .find(|u| u.email.as_deref().map(str::to_lowercase).as_deref() == Some(email.as_str()))
        .and_then(|u| u.id)
}

/// "Sample Person" as first and last names. One word is sent as the last name
/// too, as Recruitly asks for both.
fn split_name(full: &str) -> (String, String) {
    let words: Vec<&str> = full.split_whitespace().collect();
    match words.as_slice() {
        [] => ("Unknown".into(), "Unknown".into()),
        [one] => (one.to_string(), one.to_string()),
        [first @ .., last] => (first.join(" "), last.to_string()),
    }
}

/// How long a handover may run before another request may take it over.
/// Well past the worst case (a dozen calls at the 15-second timeout).
const HOLD_MINUTES: i32 = 10;

/// Ask before going on. `key` names the situation, so a confirmation given
/// for one situation never passes another.
fn ask(key: impl Into<String>, text: impl Into<String>) -> Response {
    Json(HandoverResult {
        candidate: None,
        confirm: Some(text.into()),
        confirm_key: Some(key.into()),
    })
    .into_response()
}

/// POST /api/candidates/:id/handover
pub async fn handover(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(candidacy): Path<Uuid>,
    Json(h): Json<Handover>,
) -> Response {
    let pool = match ready(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };
    type Row = (
        Uuid,
        CandidacyState,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
    );
    let row: Result<Option<Row>, _> = sqlx::query_as(
        "SELECT c.person_id, c.state, r.recruitly_job_id, r.recruitly_job_label, r.title, cl.name
         FROM candidacy c JOIN role r ON r.id = c.role_id LEFT JOIN client cl ON cl.id = r.client_id
         WHERE c.id = $1 AND c.org_id = $2",
    )
    .bind(candidacy)
    .bind(user.org_id)
    .fetch_optional(pool)
    .await;
    let (person_id, st, job_id, job_label, role_title, client_name) = match row {
        Ok(Some(r)) => r,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return server_error(e),
    };
    use CandidacyState::*;
    if !matches!(
        st,
        Shortlisted | Drafted | Approved | Contacted | Replied | NoReply | HandedToAts
    ) {
        return refuse(StatusCode::CONFLICT, "Shortlist them first.");
    }
    let confirmed = |key: &str| h.confirmed.iter().any(|k| k == key);
    let done = || async {
        match one_row(pool, user.org_id, candidacy).await {
            Ok(r) => Json(HandoverResult {
                candidate: Some(r),
                confirm: None,
                confirm_key: None,
            })
            .into_response(),
            Err(e) => server_error(e),
        }
    };
    let prior: Result<Option<(Option<String>, bool, bool)>, _> = sqlx::query_as(
        "SELECT candidate_id, create_attempted, done_at IS NOT NULL
         FROM recruitly_handover WHERE candidacy_id = $1",
    )
    .bind(candidacy)
    .fetch_optional(pool)
    .await;
    let (prior_id, attempted) = match prior {
        Ok(Some((_, _, true))) => return done().await,
        Ok(Some((id, attempted, false))) => (id, attempted),
        Ok(None) => (None, false),
        Err(e) => return server_error(e),
    };
    let s = state.recruitly.session(pool, user.org_id);
    // Checked again just before sending, so the flag is never stale.
    if prior_id.is_none() {
        let checked = match check_person(
            pool,
            &state.recruitly,
            user.org_id,
            Some(user.id),
            person_id,
        )
        .await
        {
            Ok(c) => c,
            Err(e) => return any_error(e),
        };
        if checked.dnc {
            return refuse(
                StatusCode::CONFLICT,
                "Marked do not contact in Recruitly. Not sent.",
            );
        }
        if checked.id.is_none() && attempted && !confirmed("again") {
            // An earlier try may have made the record even though its answer
            // was lost; Recruitly's search can lag behind.
            return ask(
                "again",
                "An earlier try may already have added this person to Recruitly, but Sourcer cannot find them there yet. Check Recruitly first. Add them again anyway?",
            );
        }
        // A new record next to someone of the same name would be a duplicate.
        if checked.possible && !confirmed("namesake") {
            return ask(
                "namesake",
                "Recruitly has someone with the same name. Check they are a different person. Add as a new record anyway?",
            );
        }
        if let Some(owner_id) = &checked.owner_id {
            let key = format!("owner:{owner_id}");
            if !confirmed(&key) && recruitly_user(&s, &user.email).await.as_ref() != Some(owner_id)
            {
                let owner = checked.owner.as_deref().unwrap_or("A colleague");
                let status = checked
                    .status
                    .as_ref()
                    .map(|st| format!(" ({st})"))
                    .unwrap_or_default();
                let target = job_label.as_deref().unwrap_or("Recruitly");
                return ask(
                    key,
                    format!("{owner} owns this person in Recruitly{status}. Add them to {target} anyway?"),
                );
            }
        }
    }
    match one_row(pool, user.org_id, candidacy).await {
        Ok(r) if r.do_not_contact => {
            return refuse(
                StatusCode::CONFLICT,
                "On the do-not-contact list. Not sent.",
            )
        }
        Ok(_) => {}
        Err(e) => return server_error(e),
    }
    // Hold the handover while it runs. Every later write names this hold, so
    // a run that was taken over can no longer change anything.
    let token = Uuid::new_v4();
    type Claim = (Option<String>, Option<String>, bool, bool);
    let claim: Result<Option<Claim>, _> = sqlx::query_as(
        "INSERT INTO recruitly_handover (candidacy_id, org_id, by_user, claim_token) VALUES ($1, $2, $3, $4)
         ON CONFLICT (candidacy_id) DO UPDATE SET claimed_at = now(), by_user = $3, claim_token = $4
           WHERE recruitly_handover.done_at IS NULL
             AND recruitly_handover.claimed_at < now() - make_interval(mins => $5)
         RETURNING candidate_id, pipeline_id, noted, note_attempted",
    )
    .bind(candidacy)
    .bind(user.org_id)
    .bind(user.id)
    .bind(token)
    .bind(HOLD_MINUTES)
    .fetch_optional(pool)
    .await;
    let (candidate_id, pipeline_id, noted, note_attempted) = match claim {
        Ok(Some(c)) => c,
        Ok(None) => {
            return refuse(
                StatusCode::CONFLICT,
                "Already being added. Try again in a minute.",
            )
        }
        Err(e) => return server_error(e),
    };
    // One step of the handover, written only while this request holds it.
    let step = |sql: &'static str| sqlx::query(sql).bind(candidacy).bind(token);
    let steps = async {
        // 1. The Recruitly candidate: the one found, or a new one.
        let known: Option<String> =
            sqlx::query_scalar("SELECT recruitly_id FROM person WHERE id = $1 AND org_id = $2")
                .bind(person_id)
                .bind(user.org_id)
                .fetch_one(pool)
                .await?;
        let rc_id = match candidate_id.or(known) {
            Some(id) => id,
            None => {
                type P = (String, Option<String>, Option<String>, Option<String>);
                let (name, title, employer, linkedin): P = sqlx::query_as(
                    "SELECT full_name, current_title, current_employer, linkedin_url FROM person WHERE id = $1 AND org_id = $2",
                )
                .bind(person_id)
                .bind(user.org_id)
                .fetch_one(pool)
                .await?;
                let contact = |kind: &'static str| {
                    sqlx::query_scalar::<_, String>(
                        "SELECT value FROM contact WHERE person_id = $1 AND kind = $2::contact_kind AND org_id = $3
                         ORDER BY created_at LIMIT 1",
                    )
                    .bind(person_id)
                    .bind(kind)
                    .bind(user.org_id)
                    .fetch_optional(pool)
                };
                let (first, last) = split_name(&name);
                let new = NewCandidate {
                    first_name: first,
                    last_name: last,
                    email: contact("work_email").await?,
                    mobile: contact("phone").await?,
                    linked_in: linkedin.map(|l| format!("https://www.{l}")),
                    job_title: title,
                    current_employer: employer,
                    owner_id: recruitly_user(&s, &user.email).await,
                };
                // Marked first: if the answer is lost, a retry asks before making another.
                let marked = step(
                    "UPDATE recruitly_handover SET create_attempted = true
                     WHERE candidacy_id = $1 AND claim_token = $2",
                )
                .execute(pool)
                .await?
                .rows_affected();
                if marked == 0 {
                    anyhow::bail!(RecruitlyError::Http(409, "taken over by another request".into()));
                }
                let id = s.create_candidate(&new).await?;
                sqlx::query("UPDATE person SET recruitly_id = COALESCE(recruitly_id, $2) WHERE id = $1 AND org_id = $3")
                    .bind(person_id)
                    .bind(&id)
                    .bind(user.org_id)
                    .execute(pool)
                    .await?;
                id
            }
        };
        step("UPDATE recruitly_handover SET candidate_id = $3 WHERE candidacy_id = $1 AND claim_token = $2")
            .bind(rc_id.clone())
            .execute(pool)
            .await?;
        // 2. Into the linked job's pipeline.
        if let (Some(job), None) = (&job_id, &pipeline_id) {
            let entry = match s.add_to_pipeline(job, &rc_id).await {
                Ok(id) => id,
                // Already in that job's pipeline: nothing to add.
                Err(RecruitlyError::Http(400 | 409, m)) if m.to_lowercase().contains("already") => {
                    "existing".to_string()
                }
                Err(e) => return Err(e.into()),
            };
            step("UPDATE recruitly_handover SET pipeline_id = $3 WHERE candidacy_id = $1 AND claim_token = $2")
                .bind(entry)
                .execute(pool)
                .await?;
        }
        // 3. A note saying where they came from and why. Tried once only: a
        // lost answer means a missing note rather than two.
        if !noted && !note_attempted {
            let (tier, rank, evidence): (Option<String>, Option<i32>, Option<serde_json::Value>) =
                sqlx::query_as("SELECT tier, rank, evidence FROM candidacy WHERE id = $1 AND org_id = $2")
                    .bind(candidacy)
                    .bind(user.org_id)
                    .fetch_one(pool)
                    .await?;
            let evidence = evidence.unwrap_or_default();
            let mut text = format!(
                "From Sourcer: shortlisted by {} for {role_title}{}.",
                user.name,
                client_name
                    .as_ref()
                    .map(|c| format!(" at {c}"))
                    .unwrap_or_default()
            );
            if let (Some(t), Some(r)) = (&tier, rank) {
                text.push_str(&format!(" Tier {t}, score {r}."));
            }
            if let Some(why) = evidence["reason"].as_str() {
                text.push_str(&format!(" {}", why.replace("**", "")));
            }
            let unknowns: Vec<&str> = evidence["unknowns"]
                .as_array()
                .map(|u| u.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            if !unknowns.is_empty() {
                text.push_str(&format!(" To check: {}.", unknowns.join(", ")));
            }
            step("UPDATE recruitly_handover SET note_attempted = true WHERE candidacy_id = $1 AND claim_token = $2")
                .execute(pool)
                .await?;
            if let Err(e) = s.add_note(&rc_id, &text).await {
                // Sure it was never stored (not sent, or a clear refusal): allow a retry.
                let never_stored = matches!(
                    e,
                    RecruitlyError::NotConfigured
                        | RecruitlyError::Limit
                        | RecruitlyError::Refused
                        | RecruitlyError::NotFound
                        | RecruitlyError::Http(400..=499, _)
                );
                if never_stored {
                    step("UPDATE recruitly_handover SET note_attempted = false WHERE candidacy_id = $1 AND claim_token = $2")
                        .execute(pool)
                        .await?;
                }
                return Err(e.into());
            }
            step("UPDATE recruitly_handover SET noted = true WHERE candidacy_id = $1 AND claim_token = $2")
                .execute(pool)
                .await?;
        }
        let mut tx = pool.begin().await?;
        let finished = step(
            "UPDATE recruitly_handover SET done_at = now() WHERE candidacy_id = $1 AND claim_token = $2",
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if finished == 0 {
            anyhow::bail!(RecruitlyError::Http(409, "taken over by another request".into()));
        }
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::CANDIDATE_HANDED_OVER,
            &format!("candidacy:{candidacy} recruitly:{rc_id}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(())
    }
    .await;
    match steps {
        Ok(()) => done().await,
        Err(e) => {
            // Let a retry start at once; finished steps are kept.
            let _ = step(
                "UPDATE recruitly_handover SET claimed_at = 'epoch' WHERE candidacy_id = $1 AND claim_token = $2",
            )
            .execute(pool)
            .await;
            any_error(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_split_for_recruitly() {
        assert_eq!(
            split_name("Sample Person"),
            ("Sample".into(), "Person".into())
        );
        assert_eq!(
            split_name(" Mary Ann  Smith "),
            ("Mary Ann".into(), "Smith".into())
        );
        assert_eq!(split_name("Cher"), ("Cher".into(), "Cher".into()));
    }

    #[test]
    fn dates_read_both_ways() {
        assert_eq!(day("2026-09-12T10:00:00Z").as_deref(), Some("12 Sep 2026"));
        assert_eq!(day("1757671200000").as_deref(), Some("12 Sep 2025"));
        assert_eq!(day("soon"), None);
    }

    #[test]
    fn specs_carry_location_pay_and_reference() {
        let job = Job {
            id: "j1".into(),
            title: "Senior IAM Engineer".into(),
            reference: Some("J-1042".into()),
            company_id: None,
            company_name: None,
            description: Some("<p>Lead IAM.</p>".into()),
            location: Some("Dubai".into()),
            pay: Some("30,000 to 40,000 AED monthly".into()),
            experience: Some("Senior Level".into()),
            employment: Some("Permanent".into()),
            remote: false,
            skills: vec!["IAM".into(), "Okta".into()],
        };
        assert_eq!(
            spec_from(&job),
            "Lead IAM.\n\nLocation: Dubai\nLevel: Senior Level\nType: Permanent\nSkills: IAM, Okta\nPay: 30,000 to 40,000 AED monthly\nRecruitly job: J-1042"
        );
        assert_eq!(job_label(&job), "Senior IAM Engineer (J-1042)");
    }
}
