//! Admin controls (SRS roles table): the kill switches, the client list with
//! its off-limits flag, and the do-not-contact list. Admins only.
//!
//! - Pausing sending stops every email; pausing paid calls stops searches,
//!   ranking and CV assessments. Both are checked before each call or send.
//! - Do-not-contact entries are permanent: an opt-out is honoured for good, so
//!   there is no way to remove one here.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    app::AppState, audit, domain::Client, employer, people::profile_url, roles::clean_text,
    team::Admin,
};

/// Most do-not-contact entries in one page.
pub const DNC_PAGE: i64 = 50;

fn refuse(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, msg.into()).into_response()
}

fn server_error(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "admin request failed");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

// ---------- Controls ----------

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct Controls {
    /// No email goes out while this is on.
    pub sending_paused: bool,
    /// No paid call (search, ranking, CV assessment) is made while this is on.
    pub paid_calls_paused: bool,
    /// First emails each person may send in one Dubai day (0 to 200).
    pub first_emails_per_day: i32,
}

/// GET /api/admin/controls
pub async fn get_controls(State(state): State<AppState>, Admin(user): Admin) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let row: Result<(bool, bool, i32), _> = sqlx::query_as(
        "SELECT sending_paused, paid_calls_paused, first_emails_per_day FROM org WHERE id = $1",
    )
    .bind(user.org_id)
    .fetch_one(pool)
    .await;
    match row {
        Ok((sending_paused, paid_calls_paused, first_emails_per_day)) => Json(Controls {
            sending_paused,
            paid_calls_paused,
            first_emails_per_day,
        })
        .into_response(),
        Err(e) => server_error(e),
    }
}

/// PUT /api/admin/controls
pub async fn put_controls(
    State(state): State<AppState>,
    Admin(user): Admin,
    Json(c): Json<Controls>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !(0..=200).contains(&c.first_emails_per_day) {
        return refuse(
            StatusCode::BAD_REQUEST,
            "First emails a day must be between 0 and 200.",
        );
    }
    let result = async {
        let mut tx = pool.begin().await?;
        sqlx::query(
            "UPDATE org SET sending_paused = $2, paid_calls_paused = $3, first_emails_per_day = $4
             WHERE id = $1",
        )
        .bind(user.org_id)
        .bind(c.sending_paused)
        .bind(c.paid_calls_paused)
        .bind(c.first_emails_per_day)
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::CONTROLS_SET,
            &format!(
                "sending_paused:{} paid_calls_paused:{} first_emails_per_day:{}",
                c.sending_paused, c.paid_calls_paused, c.first_emails_per_day
            ),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => Json(c).into_response(),
        Err(e) => server_error(e),
    }
}

// ---------- Clients ----------

type ClientRow = (Uuid, String, Option<String>, bool);

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct ClientUpdate {
    pub name: String,
    /// The client's web domain, e.g. "example.com". Used to keep their staff out.
    pub domain: String,
    /// Never approach their staff for any role.
    pub off_limits: bool,
}

/// PATCH /api/clients/:id: name, domain and the off-limits flag.
pub async fn update_client(
    State(state): State<AppState>,
    Admin(user): Admin,
    Path(id): Path<Uuid>,
    Json(u): Json<ClientUpdate>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(name) = clean_text(&u.name, 200) else {
        return refuse(StatusCode::BAD_REQUEST, "Enter the client's name.");
    };
    let Some(domain) = employer::normalise_domain(&u.domain) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "Enter the client's web domain, for example example.com.",
        );
    };
    let result = async {
        let mut tx = pool.begin().await?;
        let row: Result<Option<ClientRow>, sqlx::Error> = sqlx::query_as(
            "UPDATE client SET name = $3, domain = $4, off_limits = $5
                 WHERE id = $1 AND org_id = $2
                 RETURNING id, name, domain, off_limits",
        )
        .bind(id)
        .bind(user.org_id)
        .bind(&name)
        .bind(&domain)
        .bind(u.off_limits)
        .fetch_optional(&mut *tx)
        .await;
        let row = match row {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(Err(StatusCode::NOT_FOUND.into_response())),
            Err(e)
                if e.as_database_error()
                    .and_then(|d| d.code())
                    .is_some_and(|c| c == "23505") =>
            {
                return Ok(Err(refuse(
                    StatusCode::CONFLICT,
                    "Another client already has that domain.",
                )))
            }
            Err(e) => return Err(e.into()),
        };
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::CLIENT_UPDATED,
            &format!("client:{id} off_limits:{}", u.off_limits),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(Ok(Client {
            id: row.0,
            name: row.1,
            domain: row.2,
            off_limits: row.3,
        }))
    }
    .await;
    match result {
        Ok(Ok(c)) => Json(c).into_response(),
        Ok(Err(r)) => r,
        Err(e) => server_error(e),
    }
}

// ---------- Do not contact ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum DncReason {
    /// They asked not to be contacted.
    OptOut,
    /// They asked for their data to be erased.
    ErasureRequest,
}

impl DncReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::OptOut => "opt_out",
            Self::ErasureRequest => "erasure_request",
        }
    }
    fn parse(s: &str) -> Self {
        if s == "erasure_request" {
            Self::ErasureRequest
        } else {
            Self::OptOut
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct DncEntry {
    /// A lower-case email, a LinkedIn address or a phone number.
    pub identifier: String,
    pub reason: DncReason,
    /// e.g. "2 Oct 2026".
    pub added: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct DncList {
    pub entries: Vec<DncEntry>,
    #[ts(type = "number")]
    pub total: i64,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct NewDnc {
    /// An email, a LinkedIn address or a phone number, as typed.
    pub identifier: String,
    pub reason: DncReason,
}

/// The form the known check matches on: lower-case email, LinkedIn as
/// "linkedin.com/in/handle", or a phone as digits and "+".
pub fn dnc_identifier(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.chars().count() > 320 {
        return None;
    }
    if s.contains('@') {
        let e = s.to_lowercase();
        let ok = !e.contains(char::is_whitespace)
            && e.split_once('@')
                .is_some_and(|(a, d)| !a.is_empty() && d.contains('.'));
        return ok.then_some(e);
    }
    if s.to_lowercase().contains("linkedin.com") {
        return profile_url(s);
    }
    let phone: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect();
    let digits = phone.chars().filter(char::is_ascii_digit).count();
    let only_phone_chars = s
        .chars()
        .all(|c| c.is_ascii_digit() || " +-().".contains(c));
    (only_phone_chars && (7..=15).contains(&digits)).then_some(phone)
}

#[derive(Debug, Deserialize)]
pub struct DncQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub page: i64,
}

/// GET /api/admin/do-not-contact?q=&page=: newest first, 50 a page.
pub async fn list_dnc(
    State(state): State<AppState>,
    Admin(user): Admin,
    Query(q): Query<DncQuery>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let words: String = q.q.trim().to_lowercase().chars().take(100).collect();
    let like = format!(
        "%{}%",
        words
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let page = q.page.clamp(0, 10_000);
    let result = async {
        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM do_not_contact WHERE org_id = $1 AND identifier LIKE $2",
        )
        .bind(user.org_id)
        .bind(&like)
        .fetch_one(pool)
        .await?;
        let rows: Vec<(String, String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
            "SELECT identifier, reason, created_at FROM do_not_contact
             WHERE org_id = $1 AND identifier LIKE $2
             ORDER BY created_at DESC, identifier LIMIT $3 OFFSET $4",
        )
        .bind(user.org_id)
        .bind(&like)
        .bind(DNC_PAGE)
        .bind(page * DNC_PAGE)
        .fetch_all(pool)
        .await?;
        anyhow::Ok(DncList {
            entries: rows
                .into_iter()
                .map(|(identifier, reason, at)| DncEntry {
                    identifier,
                    reason: DncReason::parse(&reason),
                    added: at.format("%-d %b %Y").to_string(),
                })
                .collect(),
            total,
        })
    }
    .await;
    match result {
        Ok(l) => Json(l).into_response(),
        Err(e) => server_error(e),
    }
}

/// POST /api/admin/do-not-contact: added for good. Adding it twice is fine.
pub async fn add_dnc(
    State(state): State<AppState>,
    Admin(user): Admin,
    Json(n): Json<NewDnc>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(identifier) = dnc_identifier(&n.identifier) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "Enter an email, a LinkedIn profile address or a phone number.",
        );
    };
    let result = async {
        let mut tx = pool.begin().await?;
        sqlx::query(
            "INSERT INTO do_not_contact (org_id, identifier, reason) VALUES ($1, $2, $3)
             ON CONFLICT (org_id, identifier) DO NOTHING",
        )
        .bind(user.org_id)
        .bind(&identifier)
        .bind(n.reason.as_str())
        .execute(&mut *tx)
        .await?;
        // Nothing more goes to them: drafts and approved emails stop now.
        // Their candidacies go back to the shortlist, where the flag shows.
        sqlx::query(
            "WITH stopped AS (
               UPDATE outreach o SET status = 'stopped', stop_reason = 'Do not contact',
                      version = version + 1, updated_at = now()
               WHERE o.org_id = $1 AND o.status IN ('draft', 'approved', 'active')
                 AND (lower(o.to_email) = $2 OR EXISTS (
                       SELECT 1 FROM candidacy c JOIN person p ON p.id = c.person_id
                       LEFT JOIN contact k ON k.person_id = p.id
                       WHERE c.id = o.candidacy_id
                         AND (p.linkedin_url = $2 OR lower(k.value) = $2
                              OR regexp_replace(k.value, '[^0-9+]', '', 'g') = $2)))
               RETURNING o.candidacy_id)
             UPDATE candidacy SET state = 'shortlisted', sequence_approved_by = NULL,
                    sequence_approved_at = NULL, version = version + 1
             WHERE id IN (SELECT candidacy_id FROM stopped) AND state IN ('drafted', 'approved')",
        )
        .bind(user.org_id)
        .bind(&identifier)
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::DNC_ADDED,
            // The audit keeps the fact, not the address.
            &format!("reason:{}", n.reason.as_str()),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => (
            StatusCode::CREATED,
            Json(DncEntry {
                identifier,
                reason: n.reason,
                added: chrono::Utc::now().format("%-d %b %Y").to_string(),
            }),
        )
            .into_response(),
        Err(e) => server_error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_take_the_form_the_check_matches() {
        assert_eq!(
            dnc_identifier(" Sam@Mail.Example ").as_deref(),
            Some("sam@mail.example")
        );
        assert_eq!(
            dnc_identifier("https://www.linkedin.com/in/sam-sample/").as_deref(),
            Some("linkedin.com/in/sam-sample")
        );
        assert_eq!(
            dnc_identifier("+971 50 123 4567").as_deref(),
            Some("+971501234567")
        );
        assert_eq!(
            dnc_identifier("(020) 7946-0018").as_deref(),
            Some("02079460018")
        );
        for bad in [
            "",
            "sam",
            "@",
            "sam@nodot",
            "12",
            "call me",
            "linkedin.com/company/x",
        ] {
            assert_eq!(dnc_identifier(bad), None, "{bad}");
        }
    }
}
