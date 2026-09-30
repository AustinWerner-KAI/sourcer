//! The Candidates step for a role (SRS F6, F7, F9):
//!
//! - After each pull, Claude ranks the new people against the confirmed brief:
//!   a tier, a score, a reason and a list of things to check. People go from
//!   found straight to ranked, because the known check below runs live.
//! - The known check runs every time the list is shown, so it is never stale:
//!   anyone the team is already working with for another role, has messaged,
//!   or must not contact is flagged. The resourcer still decides.
//! - Shortlist or reject with a reason code. Nothing is sent from here.

use std::sync::Arc;

use anyhow::Context;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{types::Json as SqlJson, PgPool};
use uuid::Uuid;

use crate::{
    ai::{Claude, RankInput, RankJob},
    app::AppState,
    audit,
    auth::CurrentUser,
    domain::{
        CandidacyState, CandidateRow, CandidateTab, CandidatesView, Decision, DecisionAction,
        ReasonCode,
    },
    employer::{self, Verdict},
    jobs,
    searching::{confirmed_brief, role_exists},
    worker::{HandlerFuture, JobHandler},
};

pub const RANK_JOB: &str = "candidates.rank";
/// People sent to Claude in one request.
const BATCH: i64 = 20;
/// Batches per job; the rest are queued as a new job.
const MAX_BATCHES: usize = 10;
/// Most people shown in one list.
pub const MAX_LISTED: i64 = 200;
/// Most past jobs sent per person.
const MAX_JOBS_SENT: usize = 12;

fn refuse(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, msg.into()).into_response()
}

fn server_error(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "candidates request failed");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RankJobPayload {
    pub role_id: Uuid,
}

/// Queue a ranking for the role, unless one is already waiting.
pub async fn queue_rank(pool: &PgPool, org_id: Uuid, role_id: Uuid) -> anyhow::Result<bool> {
    let added = sqlx::query(
        "INSERT INTO job (org_id, kind, payload)
         SELECT $1, $2, $3
         WHERE NOT EXISTS (SELECT 1 FROM job WHERE org_id = $1 AND kind = $2
                             AND status = 'queued' AND payload->>'role_id' = $4)",
    )
    .bind(org_id)
    .bind(RANK_JOB)
    .bind(json!({ "role_id": role_id }))
    .bind(role_id.to_string())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(added > 0)
}

/// Why ranking cannot run for this organisation right now, if it cannot.
async fn rank_blocked(
    pool: &PgPool,
    ai: &Claude,
    org_id: Uuid,
    has_brief: bool,
) -> anyhow::Result<Option<&'static str>> {
    let paused: bool = sqlx::query_scalar("SELECT paid_calls_paused FROM org WHERE id = $1")
        .bind(org_id)
        .fetch_one(pool)
        .await?;
    Ok(if paused {
        Some("Paid calls are paused by an admin, so ranking waits.")
    } else if !ai.configured() {
        Some("Ranking is not set up yet (no AI key).")
    } else if !has_brief {
        Some("Confirm the brief first.")
    } else {
        None
    })
}

/// Ranks a role's unranked people with Claude, in batches.
pub struct RankHandler {
    pub pool: PgPool,
    pub ai: Arc<Claude>,
}

type PersonRow = (
    Uuid,
    Uuid,
    Option<String>,
    Option<String>,
    Option<String>,
    SqlJson<Vec<String>>,
);
type JobRow = (
    Uuid,
    String,
    Option<String>,
    Option<chrono::NaiveDate>,
    Option<chrono::NaiveDate>,
);

impl RankHandler {
    async fn rank_role(&self, org_id: Uuid, role_id: Uuid) -> anyhow::Result<usize> {
        let pool = &self.pool;
        let Some((_, version, lines)) = confirmed_brief(pool, role_id).await? else {
            return Ok(0);
        };
        let mut tried: Vec<Uuid> = Vec::new();
        let mut ranked = 0;
        for _ in 0..MAX_BATCHES {
            // Checked before every paid call, so a pause stops a running job.
            if rank_blocked(pool, &self.ai, org_id, true).await?.is_some() {
                // Left unranked; the screen offers "Rank now" once this is fixed.
                break;
            }
            let mut tx = pool.begin().await?;
            // Locked while Claude ranks them, so a second job skips them.
            let people: Vec<PersonRow> = sqlx::query_as(
                "SELECT c.id, p.id, p.current_title, p.current_employer, p.location, p.skills
                 FROM candidacy c JOIN person p ON p.id = c.person_id
                 WHERE c.role_id = $1 AND c.org_id = $2
                   AND c.state IN ('found', 'known_checked') AND NOT (c.id = ANY($3))
                 ORDER BY c.created_at, c.id
                 LIMIT $4
                 FOR UPDATE OF c SKIP LOCKED",
            )
            .bind(role_id)
            .bind(org_id)
            .bind(&tried)
            .bind(BATCH)
            .fetch_all(&mut *tx)
            .await?;
            if people.is_empty() {
                break;
            }
            tried.extend(people.iter().map(|p| p.0));
            let person_ids: Vec<Uuid> = people.iter().map(|p| p.1).collect();
            let history: Vec<JobRow> = sqlx::query_as(
                "SELECT person_id, employer, title, start_date, end_date FROM employment
                 WHERE person_id = ANY($1) AND org_id = $2
                 ORDER BY end_date DESC NULLS FIRST, start_date DESC NULLS LAST",
            )
            .bind(&person_ids)
            .bind(org_id)
            .fetch_all(&mut *tx)
            .await?;
            let month = |d: Option<chrono::NaiveDate>| d.map(|d| d.format("%Y-%m").to_string());
            let inputs: Vec<RankInput> = people
                .iter()
                .enumerate()
                .map(
                    |(i, (_, person, title, employer, location, skills))| RankInput {
                        // A made-up id: Claude never sees who the person is.
                        id: format!("c{}", i + 1),
                        title: title.clone(),
                        employer: employer.clone(),
                        location: location.clone(),
                        skills: skills.0.clone(),
                        experience: history
                            .iter()
                            .filter(|h| h.0 == *person)
                            .take(MAX_JOBS_SENT)
                            .map(|h| RankJob {
                                title: h.2.clone(),
                                employer: h.1.clone(),
                                start: month(h.3),
                                end: month(h.4),
                            })
                            .collect(),
                    },
                )
                .collect();
            let results = self
                .ai
                .rank(&lines, &inputs)
                .await
                .map_err(|e| anyhow::anyhow!(e))
                .context("ranking failed")?;
            let mut in_batch = 0;
            for r in results {
                let Some(candidacy) =
                    r.id.strip_prefix('c')
                        .and_then(|n| n.parse::<usize>().ok())
                        .and_then(|n| people.get(n.wrapping_sub(1)))
                        .map(|p| p.0)
                else {
                    continue;
                };
                sqlx::query(
                    "UPDATE candidacy SET state = 'ranked', tier = $2, rank = $3, evidence = $4,
                            ranked_at = now(), version = version + 1
                     WHERE id = $1 AND state IN ('found', 'known_checked')",
                )
                .bind(candidacy)
                .bind(r.tier)
                .bind(r.score)
                .bind(json!({"reason": r.reason, "unknowns": r.unknowns, "brief_version": version}))
                .execute(&mut *tx)
                .await?;
                in_batch += 1;
            }
            if in_batch == 0 {
                // Claude ranked no one: stop rather than pay again. "Rank now" retries.
                tracing::warn!(%role_id, "ranking returned nothing usable");
                break;
            }
            // Audited with the batch, so a later failure never leaves unaudited ranks.
            audit::record(
                &mut *tx,
                org_id,
                None,
                audit::action::CANDIDATES_RANKED,
                &format!("role:{role_id} ranked:{in_batch}"),
            )
            .await?;
            tx.commit().await?;
            ranked += in_batch;
        }
        // More than one job's worth, and making progress: carry on in a fresh job.
        if ranked > 0 && tried.len() >= MAX_BATCHES * BATCH as usize {
            queue_rank(pool, org_id, role_id).await?;
        }
        Ok(ranked)
    }
}

impl JobHandler for RankHandler {
    fn kind(&self) -> &'static str {
        RANK_JOB
    }

    fn handle<'a>(&'a self, job: &'a jobs::Job) -> HandlerFuture<'a> {
        Box::pin(async move {
            let p: RankJobPayload =
                serde_json::from_value(job.payload.clone()).context("bad rank payload")?;
            self.rank_role(job.org_id, p.role_id).await?;
            Ok(())
        })
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    version: i32,
    state: CandidacyState,
    full_name: String,
    current_title: Option<String>,
    current_employer: Option<String>,
    location: Option<String>,
    linkedin_url: Option<String>,
    tier: Option<String>,
    rank: Option<i32>,
    evidence: Option<serde_json::Value>,
    employer_unknown: bool,
    reason: Option<ReasonCode>,
    has_email: bool,
    has_phone: bool,
    dnc: bool,
    other_state: Option<String>,
    other_title: Option<String>,
    last_out: Option<chrono::DateTime<chrono::Utc>>,
}

/// Everything the list shows, including the live known check. `{filter}` and
/// `{order}` are fixed strings chosen in code, never user input.
const ROW_SELECT: &str = "
SELECT c.id, c.version, c.state, p.full_name, p.current_title, p.current_employer,
       p.location, p.linkedin_url, c.tier, c.rank, c.evidence, c.employer_unknown, c.reason,
       EXISTS (SELECT 1 FROM contact k WHERE k.person_id = p.id AND k.kind = 'work_email') AS has_email,
       EXISTS (SELECT 1 FROM contact k WHERE k.person_id = p.id AND k.kind = 'phone') AS has_phone,
       (p.opted_out OR EXISTS (
          SELECT 1 FROM do_not_contact d
          WHERE d.org_id = c.org_id AND (
            d.identifier = p.linkedin_url
            OR d.identifier IN (SELECT lower(k.value) FROM contact k WHERE k.person_id = p.id)
            OR d.identifier IN (SELECT regexp_replace(k.value, '[^0-9+]', '', 'g')
                                FROM contact k WHERE k.person_id = p.id AND k.kind = 'phone')))
       ) AS dnc,
       o.state::text AS other_state, o.title AS other_title, t.last_out
FROM candidacy c
JOIN person p ON p.id = c.person_id
LEFT JOIN LATERAL (
  SELECT c2.state, r.title FROM candidacy c2 JOIN role r ON r.id = c2.role_id
  WHERE c2.person_id = c.person_id AND c2.role_id <> c.role_id
    AND c2.state IN ('shortlisted', 'drafted', 'approved', 'contacted', 'replied', 'no_reply', 'handed_to_ats')
  ORDER BY array_position(ARRAY['handed_to_ats', 'replied', 'contacted', 'no_reply', 'approved',
                                'drafted', 'shortlisted']::candidacy_state[], c2.state)
  LIMIT 1
) o ON true
LEFT JOIN LATERAL (
  SELECT max(tt.at) AS last_out FROM touch tt WHERE tt.person_id = c.person_id AND tt.direction = 'out'
) t ON true
WHERE c.org_id = $1 AND {filter}
ORDER BY {order}
LIMIT $3";

/// What the team already knows about someone, in words, or `None`.
fn known(
    other_state: Option<&str>,
    other_title: Option<&str>,
    last_out: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<String> {
    let doing = other_state.map(|s| match s {
        "handed_to_ats" => "handed to the ATS",
        "replied" => "replied",
        "contacted" | "no_reply" => "contacted",
        "approved" | "drafted" => "being approached",
        _ => "shortlisted",
    });
    match (last_out, doing, other_title) {
        (Some(at), _, Some(title)) => Some(format!(
            "Known: contacted {}, {} for {title}",
            at.format("%-d %b %Y"),
            doing.unwrap_or("contacted")
        )),
        (Some(at), _, None) => Some(format!("Known: contacted {}", at.format("%-d %b %Y"))),
        (None, Some(d), Some(title)) => Some(format!("Known: {d} for {title}")),
        _ => None,
    }
}

impl From<Row> for CandidateRow {
    fn from(r: Row) -> Self {
        let evidence = r.evidence.unwrap_or_default();
        let known = known(
            r.other_state.as_deref(),
            r.other_title.as_deref(),
            r.last_out,
        );
        CandidateRow {
            id: r.id,
            version: r.version,
            state: r.state,
            name: r.full_name,
            title: r.current_title,
            employer: r.current_employer,
            location: r.location,
            linkedin_url: r.linkedin_url,
            tier: r.tier,
            score: r.rank,
            reason: evidence["reason"].as_str().map(String::from),
            unknowns: evidence["unknowns"]
                .as_array()
                .map(|xs| {
                    xs.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            known,
            do_not_contact: r.dnc,
            employer_unknown: r.employer_unknown,
            has_work_email: r.has_email,
            has_phone: r.has_phone,
            reject_reason: r.reason,
        }
    }
}

async fn rows_for_role(
    pool: &PgPool,
    org_id: Uuid,
    role_id: Uuid,
    tab: CandidateTab,
) -> anyhow::Result<Vec<CandidateRow>> {
    let (states, order) = match tab {
        CandidateTab::Review => (
            "'found', 'known_checked', 'ranked'",
            "c.rank DESC NULLS LAST, c.created_at, c.id",
        ),
        CandidateTab::Shortlisted => (
            "'shortlisted'",
            "c.rank DESC NULLS LAST, c.decided_at DESC, c.id",
        ),
        CandidateTab::Rejected => ("'rejected'", "c.decided_at DESC NULLS LAST, c.id"),
    };
    let sql = ROW_SELECT
        .replace(
            "{filter}",
            &format!("c.role_id = $2 AND c.state IN ({states})"),
        )
        .replace("{order}", order);
    let rows: Vec<Row> = sqlx::query_as(&sql)
        .bind(org_id)
        .bind(role_id)
        .bind(MAX_LISTED)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(CandidateRow::from).collect())
}

async fn one_row(pool: &PgPool, org_id: Uuid, candidacy: Uuid) -> anyhow::Result<CandidateRow> {
    let sql = ROW_SELECT
        .replace("{filter}", "c.id = $2")
        .replace("{order}", "c.id");
    let row: Row = sqlx::query_as(&sql)
        .bind(org_id)
        .bind(candidacy)
        .bind(1i64)
        .fetch_one(pool)
        .await?;
    Ok(row.into())
}

async fn view(
    state: &AppState,
    pool: &PgPool,
    org_id: Uuid,
    role_id: Uuid,
    tab: CandidateTab,
) -> anyhow::Result<CandidatesView> {
    let brief = confirmed_brief(pool, role_id).await?;
    let blocked = rank_blocked(pool, &state.ai, org_id, brief.is_some()).await?;
    let (to_review, shortlisted, rejected, unranked): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE state IN ('found', 'known_checked', 'ranked')),
                count(*) FILTER (WHERE state = 'shortlisted'),
                count(*) FILTER (WHERE state = 'rejected'),
                count(*) FILTER (WHERE state IN ('found', 'known_checked'))
         FROM candidacy WHERE role_id = $1 AND org_id = $2",
    )
    .bind(role_id)
    .bind(org_id)
    .fetch_one(pool)
    .await?;
    let ranking: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM job WHERE org_id = $1 AND kind = $2
                          AND status IN ('queued', 'running') AND payload->>'role_id' = $3)",
    )
    .bind(org_id)
    .bind(RANK_JOB)
    .bind(role_id.to_string())
    .fetch_one(pool)
    .await?;
    Ok(CandidatesView {
        tab,
        brief_version: brief.map(|b| b.1),
        to_review,
        shortlisted,
        rejected,
        unranked,
        ranking,
        rank_blocked: blocked.map(String::from),
        people: rows_for_role(pool, org_id, role_id, tab).await?,
    })
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub tab: CandidateTab,
}

/// GET /api/roles/:id/candidates?tab=review|shortlisted|rejected
pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(role_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> Response {
    let Some(pool) = state.pool.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match role_exists(&pool, user.org_id, role_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return server_error(e),
    }
    match view(&state, &pool, user.org_id, role_id, q.tab).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => server_error(e),
    }
}

/// POST /api/roles/:id/candidates/rank: rank anyone still unranked.
pub async fn rank_now(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(role_id): Path<Uuid>,
) -> Response {
    let Some(pool) = state.pool.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match role_exists(&pool, user.org_id, role_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return server_error(e),
    }
    let has_brief = match confirmed_brief(&pool, role_id).await {
        Ok(b) => b.is_some(),
        Err(e) => return server_error(e),
    };
    match rank_blocked(&pool, &state.ai, user.org_id, has_brief).await {
        Ok(Some(why)) => return refuse(StatusCode::CONFLICT, why),
        Ok(None) => {}
        Err(e) => return server_error(e),
    }
    if let Err(e) = queue_rank(&pool, user.org_id, role_id).await {
        return server_error(e);
    }
    match view(&state, &pool, user.org_id, role_id, CandidateTab::Review).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => server_error(e),
    }
}

/// POST /api/candidates/:id/decide: shortlist, reject with a reason, or reconsider.
pub async fn decide(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(candidacy): Path<Uuid>,
    Json(d): Json<Decision>,
) -> Response {
    let Some(pool) = state.pool.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let current: Result<Option<(Uuid, Uuid, CandidacyState, i32)>, _> = sqlx::query_as(
        "SELECT role_id, person_id, state, version FROM candidacy WHERE id = $1 AND org_id = $2",
    )
    .bind(candidacy)
    .bind(user.org_id)
    .fetch_optional(&pool)
    .await;
    let (role_id, person_id, from, version) = match current {
        Ok(Some(c)) => c,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return server_error(e),
    };
    if version != d.version {
        return refuse(
            StatusCode::CONFLICT,
            "Someone else changed this person. Reload to see the latest.",
        );
    }
    let (to, reason, action) = match d.action {
        DecisionAction::Shortlist => (
            CandidacyState::Shortlisted,
            None,
            audit::action::CANDIDATE_SHORTLISTED,
        ),
        DecisionAction::Reject => {
            let Some(r) = d.reason else {
                return refuse(StatusCode::BAD_REQUEST, "Choose a reason to reject.");
            };
            (
                CandidacyState::Rejected,
                Some(r),
                audit::action::CANDIDATE_REJECTED,
            )
        }
        DecisionAction::Reconsider => (
            CandidacyState::Ranked,
            None,
            audit::action::CANDIDATE_RECONSIDERED,
        ),
    };
    // Reconsider only undoes a rejection; ranking is the ranker's job.
    let allowed = from.can_move_to(to)
        && (d.action != DecisionAction::Reconsider || from == CandidacyState::Rejected);
    if !allowed {
        let why = if matches!(from, CandidacyState::Found | CandidacyState::KnownChecked) {
            "Not ranked yet. Rank first, then decide."
        } else {
            "That is not possible from where this person is now. Reload to see the latest."
        };
        return refuse(StatusCode::CONFLICT, why);
    }
    if to == CandidacyState::Shortlisted {
        // People move jobs, and opt out: check again now.
        match employer::check_person(&pool, user.org_id, role_id, person_id).await {
            Ok(Verdict::LockedOut) => {
                return refuse(
                    StatusCode::CONFLICT,
                    "Now works at a company that is locked out for this role.",
                )
            }
            Ok(_) => {}
            Err(e) => return server_error(e),
        }
        match one_row(&pool, user.org_id, candidacy).await {
            Ok(r) if r.do_not_contact => {
                return refuse(StatusCode::CONFLICT, "On the do-not-contact list.")
            }
            Ok(_) => {}
            Err(e) => return server_error(e),
        }
    }
    let result = async {
        let mut tx = pool.begin().await?;
        let changed = sqlx::query(
            "UPDATE candidacy SET state = $3, reason = $4, decided_by = $5, decided_at = now(),
                    version = version + 1
             WHERE id = $1 AND version = $2",
        )
        .bind(candidacy)
        .bind(version)
        .bind(to)
        .bind(reason)
        .bind(user.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed == 0 {
            return anyhow::Ok(false);
        }
        let why = reason
            .and_then(|r| serde_json::to_value(r).ok())
            .and_then(|v| v.as_str().map(|r| format!(" reason:{r}")))
            .unwrap_or_default();
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            action,
            &format!("candidacy:{candidacy}{why}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(true)
    }
    .await;
    match result {
        Ok(true) => match one_row(&pool, user.org_id, candidacy).await {
            Ok(r) => Json(r).into_response(),
            Err(e) => server_error(e),
        },
        Ok(false) => refuse(
            StatusCode::CONFLICT,
            "Someone else changed this person. Reload to see the latest.",
        ),
        Err(e) => server_error(e),
    }
}
