//! The Search step for a role (SRS F5, N11, N14): count the matches for each
//! location, then pull the chosen number of people in the background.
//!
//! - Only a confirmed brief is searched, and only when the role has a client.
//! - Counting costs one credit per location; pulling costs one per person.
//! - The same key never counts or pulls twice, so a repeated press is free.
//! - A pull must come from a count of the brief as it is now confirmed.
//! - Pulls of more than `CONFIRM_ABOVE` people need a second confirmation.

use std::sync::Arc;

use anyhow::Context;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{types::Json as SqlJson, PgPool};
use uuid::Uuid;

use crate::{
    app::AppState,
    audit,
    auth::CurrentUser,
    domain::{
        BriefLines, CountLocation, CountRequest, CountView, PullRequest, PullView, SearchState,
        Widen,
    },
    employer::{self, Company},
    jobs, plan,
    search::{run_search, SearchRequest},
    sources::{pdl::MAX_PAGE, PeopleSource, SearchQuery},
    worker::{HandlerFuture, JobHandler},
};

pub const PULL_JOB: &str = "search.pull";
/// Pulls bigger than this need the resourcer to confirm a second time.
pub const CONFIRM_ABOVE: u32 = 50;
const MAX_KEY_CHARS: usize = 100;
/// Most people already found for a role that a pull leaves out by id.
const MAX_EXCLUDED: usize = 5_000;

pub(crate) fn refuse(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, msg.into()).into_response()
}

pub(crate) fn server_error(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "search request failed");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

pub(crate) fn valid_key(k: &str) -> bool {
    !k.trim().is_empty() && k.len() <= MAX_KEY_CHARS
}

/// A location as counted, with the exact query that was counted, so the pull
/// searches for the same people.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Counted {
    pub label: String,
    pub total: i64,
    pub query: Value,
}

/// The role's latest confirmed brief: (id, version, lines).
pub(crate) async fn confirmed_brief(
    pool: &PgPool,
    role_id: Uuid,
) -> anyhow::Result<Option<(Uuid, i32, BriefLines)>> {
    let row: Option<crate::roles::BriefRow> = sqlx::query_as(&format!(
        "SELECT {} FROM brief
         WHERE role_id = $1 AND confirmed_at IS NOT NULL ORDER BY version DESC LIMIT 1",
        crate::roles::BRIEF_COLUMNS
    ))
    .bind(role_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(crate::roles::BriefRow::lines))
}

/// Why this role cannot be searched right now, if it cannot.
pub(crate) async fn blocked(
    state: &AppState,
    pool: &PgPool,
    org_id: Uuid,
    role_id: Uuid,
    has_brief: bool,
) -> anyhow::Result<Option<&'static str>> {
    let (closed, paused, has_client): (bool, bool, bool) = sqlx::query_as(
        "SELECT r.closed_at IS NOT NULL, o.paid_calls_paused, r.client_id IS NOT NULL
         FROM role r JOIN org o ON o.id = r.org_id WHERE r.id = $1 AND r.org_id = $2",
    )
    .bind(role_id)
    .bind(org_id)
    .fetch_one(pool)
    .await?;
    Ok(if closed {
        Some("This role is closed. Reopen it on the Roles page to search again.")
    } else if paused {
        Some("Paid searches are paused by an admin. Nothing was spent.")
    } else if !state.pdl.configured() {
        Some("People Data Labs is not set up yet (no key). Nothing was spent.")
    } else if !has_client {
        Some("Choose the client for this role first, so their staff are kept out.")
    } else if !has_brief {
        Some("Confirm the brief first.")
    } else {
        None
    })
}

pub(crate) async fn role_exists(
    pool: &PgPool,
    org_id: Uuid,
    role_id: Uuid,
) -> anyhow::Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM role WHERE id = $1 AND org_id = $2)")
            .bind(role_id)
            .bind(org_id)
            .fetch_one(pool)
            .await?,
    )
}

async fn search_state(
    state: &AppState,
    pool: &PgPool,
    org_id: Uuid,
    role_id: Uuid,
) -> anyhow::Result<SearchState> {
    let brief = confirmed_brief(pool, role_id).await?;
    let blocked = blocked(state, pool, org_id, role_id, brief.is_some()).await?;
    let unconfirmed_edits: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM brief WHERE role_id = $1 AND confirmed_at IS NULL)",
    )
    .bind(role_id)
    .fetch_one(pool)
    .await?;
    let locations = match &brief {
        Some((_, _, lines)) => plan::plan(lines, &[])
            .into_iter()
            .map(|s| s.label)
            .collect(),
        None => Vec::new(),
    };

    type CountRow = (Uuid, Uuid, i32, i64, SqlJson<Vec<Counted>>, i32, bool);
    let count: Option<CountRow> = sqlx::query_as(
        "SELECT c.id, c.brief_id, b.version, extract(epoch FROM c.created_at)::bigint,
                c.locations, c.credits_used, EXISTS (SELECT 1 FROM pull p WHERE p.count_id = c.id)
         FROM search_count c JOIN brief b ON b.id = c.brief_id
         WHERE c.role_id = $1 AND c.org_id = $2 AND c.locations IS NOT NULL AND c.search_id IS NULL
         ORDER BY c.created_at DESC LIMIT 1",
    )
    .bind(role_id)
    .bind(org_id)
    .fetch_optional(pool)
    .await?;
    let count = count.map(
        |(id, brief_id, version, at, locs, credits, pulled)| CountView {
            id,
            brief_version: version,
            counted_at: at,
            stale: brief.as_ref().map(|b| b.0) != Some(brief_id),
            locations: locs
                .0
                .into_iter()
                .map(|c| CountLocation {
                    label: c.label,
                    total: c.total,
                })
                .collect(),
            credits_used: credits,
            pulled,
        },
    );

    type PullRow = (
        Uuid,
        Uuid,
        i32,
        i32,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        Vec<String>,
        Option<String>,
    );
    // The latest press. A press on several wider searches makes one pull per
    // search in one transaction, so they share a time and are shown together.
    let pull: Option<PullRow> = sqlx::query_as(
        "WITH ps AS (
           SELECT * FROM pull WHERE role_id = $1 AND org_id = $2
             AND created_at = (SELECT max(created_at) FROM pull WHERE role_id = $1 AND org_id = $2)
         ), rs AS (SELECT r.* FROM run r WHERE r.pull_id IN (SELECT id FROM ps))
         SELECT (SELECT id FROM ps ORDER BY id LIMIT 1),
                (SELECT count_id FROM ps ORDER BY id LIMIT 1),
                (SELECT sum(requested) FROM ps)::int, (SELECT sum(locations) FROM ps)::int,
                (SELECT coalesce(sum(records_pulled) FILTER (WHERE finished_at IS NOT NULL), 0) FROM rs),
                (SELECT coalesce(sum(new_candidates), 0) FROM rs),
                (SELECT coalesce(sum(left_out), 0) FROM rs),
                (SELECT coalesce(sum(unknown_employer), 0) FROM rs),
                (SELECT coalesce(sum(credits_used), 0) FROM rs),
                (SELECT count(finished_at) FROM rs),
                (SELECT coalesce(sum(credits_used) FILTER (WHERE finished_at IS NULL), 0) FROM rs),
                ARRAY(SELECT j.payload->>'location' FROM job j
                      WHERE j.kind = $3 AND j.status = 'failed'
                        AND j.payload->>'pull_id' IN (SELECT id::text FROM ps) ORDER BY 1),
                (SELECT string_agg(e.name, ' and ' ORDER BY e.slot)
                 FROM ps JOIN extra_search e ON e.id = ps.search_id)
         WHERE EXISTS (SELECT 1 FROM ps)",
    )
    .bind(role_id)
    .bind(org_id)
    .bind(PULL_JOB)
    .fetch_optional(pool)
    .await?;
    let pull = pull.map(
        |(
            id,
            count_id,
            requested,
            locations,
            pulled,
            new,
            left_out,
            unknown,
            credits,
            done,
            unsaved,
            failed_locations,
            search_name,
        )| {
            let failed = !failed_locations.is_empty();
            PullView {
                id,
                count_id,
                requested,
                pulled: pulled as i32,
                new_candidates: new as i32,
                already: (pulled - new - left_out).max(0) as i32,
                left_out: left_out as i32,
                unknown_employer: unknown as i32,
                credits_used: credits as i32,
                locations,
                locations_done: done as i32,
                // Every location has either finished or given up.
                done: done as i32 + failed_locations.len() as i32 >= locations,
                failed,
                failed_locations,
                credits_unsaved: unsaved as i32,
                search_name,
            }
        },
    );

    let credits_this_month: i64 = sqlx::query_scalar(
        "SELECT (SELECT coalesce(sum(credits_used), 0) FROM run
                 WHERE org_id = $1 AND created_at >= date_trunc('month', now()))
              + (SELECT coalesce(sum(credits_used), 0) FROM search_count
                 WHERE org_id = $1 AND created_at >= date_trunc('month', now()))",
    )
    .bind(org_id)
    .fetch_one(pool)
    .await?;

    // Queued or running pull jobs for this role, from any search.
    let pulling: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM job WHERE org_id = $1 AND kind = $2
                          AND status IN ('queued', 'running') AND payload->>'role_id' = $3::text)",
    )
    .bind(org_id)
    .bind(PULL_JOB)
    .bind(role_id)
    .fetch_one(pool)
    .await?;
    let tighten_round: i16 = match &brief {
        Some((id, _, _)) => {
            sqlx::query_scalar("SELECT tighten_round FROM brief WHERE id = $1")
                .bind(id)
                .fetch_one(pool)
                .await?
        }
        None => 0,
    };
    let more = match &brief {
        Some((brief_id, _, lines)) => crate::more::views(pool, org_id, *brief_id, lines).await?,
        None => Vec::new(),
    };

    Ok(SearchState {
        brief_version: brief.as_ref().map(|b| b.1),
        lines: brief.map(|b| b.2),
        unconfirmed_edits,
        locations,
        blocked: blocked.map(String::from),
        count,
        pull,
        pulling,
        more,
        tighten_round: tighten_round.into(),
        credits_this_month,
    })
}

/// GET /api/roles/:id/search
pub async fn get_search(
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
    match search_state(&state, &pool, user.org_id, role_id).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => server_error(e),
    }
}

/// POST /api/roles/:id/search/count: one credit per location.
pub async fn count(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(role_id): Path<Uuid>,
    Json(req): Json<CountRequest>,
) -> Response {
    let Some(pool) = state.pool.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !valid_key(&req.key) {
        return refuse(StatusCode::BAD_REQUEST, "Missing request key.");
    }
    match role_exists(&pool, user.org_id, role_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return server_error(e),
    }
    let brief = match confirmed_brief(&pool, role_id).await {
        Ok(b) => b,
        Err(e) => return server_error(e),
    };
    match blocked(&state, &pool, user.org_id, role_id, brief.is_some()).await {
        Ok(Some(why)) => return refuse(StatusCode::CONFLICT, why),
        Ok(None) => {}
        Err(e) => return server_error(e),
    }
    let Some((brief_id, _, lines)) = brief else {
        return refuse(StatusCode::CONFLICT, "Confirm the brief first.");
    };
    if !state.search_limit.allow(user.id) {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many searches in a short time. Wait a few minutes, then try again.",
        );
    }

    do_count(
        &state,
        &pool,
        &user,
        role_id,
        brief_id,
        None,
        &req.key,
        |locked| plan::plan(&lines, locked),
    )
    .await
}

/// Which search a count is for: the brief's own (`None`) or a wider one,
/// with the version of its widening.
pub(crate) type CountOf = Option<(Uuid, i32)>;

/// Count each place of a search, once per key, and store it. A wider search
/// counts only people not already found for the role, so it says how many
/// are new.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn do_count(
    state: &AppState,
    pool: &PgPool,
    user: &CurrentUser,
    role_id: Uuid,
    brief_id: Uuid,
    of: CountOf,
    key: &str,
    searches: impl FnOnce(&[Company]) -> Vec<plan::LocationSearch>,
) -> Response {
    let user_id = user.id;
    let org_id = user.org_id;
    // Reserve the key first, so a second press waits instead of paying again.
    let reserved: Result<Option<Uuid>, _> = sqlx::query_scalar(
        "INSERT INTO search_count (org_id, role_id, brief_id, created_by, idempotency_key,
                                   search_id, search_version)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (org_id, idempotency_key) DO NOTHING RETURNING id",
    )
    .bind(org_id)
    .bind(role_id)
    .bind(brief_id)
    .bind(user_id)
    .bind(key)
    .bind(of.map(|o| o.0))
    .bind(of.map(|o| o.1))
    .fetch_optional(pool)
    .await;
    let count_id = match reserved {
        Ok(Some(id)) => id,
        Ok(None) => {
            let done: Result<Option<bool>, _> = sqlx::query_scalar(
                "SELECT locations IS NOT NULL FROM search_count
                 WHERE org_id = $1 AND idempotency_key = $2 AND role_id = $3",
            )
            .bind(org_id)
            .bind(key)
            .bind(role_id)
            .fetch_optional(pool)
            .await;
            return match done {
                Ok(Some(true)) => state_response(state, pool, org_id, role_id).await,
                Ok(_) => refuse(StatusCode::CONFLICT, "Still counting. Wait a moment."),
                Err(e) => server_error(e),
            };
        }
        Err(e) => return server_error(e),
    };

    let spent = std::sync::atomic::AtomicU32::new(0);
    let result = async {
        let locked_out = employer::locked_out(pool, org_id, role_id).await?;
        let found = match of {
            Some(_) => found_ids(pool, org_id, role_id).await?,
            None => Vec::new(),
        };
        let mut counted = Vec::new();
        let mut credits = 0u32;
        for search in searches(&locked_out) {
            let page = state
                .pdl
                .search(&SearchQuery {
                    query: excluding(search.query.clone(), &found),
                    size: 1,
                    scroll_token: None,
                })
                .await
                .map_err(|e| anyhow::anyhow!(e))
                .context("count failed")?;
            credits += page.credits_used;
            spent.store(credits, std::sync::atomic::Ordering::SeqCst);
            counted.push(Counted {
                label: search.label,
                total: page.total as i64,
                query: search.query,
            });
        }
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE search_count SET locations = $2, credits_used = $3 WHERE id = $1")
            .bind(count_id)
            .bind(SqlJson(&counted))
            .bind(credits as i32)
            .execute(&mut *tx)
            .await?;
        let of_search = of.map_or(String::new(), |(id, v)| format!(" search:{id} v{v}"));
        audit::record(
            &mut *tx,
            org_id,
            Some(user_id),
            audit::action::SEARCH_COUNTED,
            &format!("count:{count_id} role:{role_id} credits:{credits}{of_search}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => state_response(state, pool, org_id, role_id).await,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "count failed");
            // Keep what was spent before the failure in this month's total;
            // the count itself stays unfinished and is never offered for a pull.
            let spent = spent.load(std::sync::atomic::Ordering::SeqCst) as i32;
            let _ = if spent > 0 {
                sqlx::query("UPDATE search_count SET credits_used = $2 WHERE id = $1")
                    .bind(count_id)
                    .bind(spent)
                    .execute(pool)
                    .await
            } else {
                sqlx::query("DELETE FROM search_count WHERE id = $1")
                    .bind(count_id)
                    .execute(pool)
                    .await
            };
            refuse(
                StatusCode::BAD_GATEWAY,
                "People Data Labs could not count just now. Try again in a minute.",
            )
        }
    }
}

/// The search step as JSON, after a change.
pub(crate) async fn state_response(
    state: &AppState,
    pool: &PgPool,
    org_id: Uuid,
    role_id: Uuid,
) -> Response {
    match search_state(state, pool, org_id, role_id).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => server_error(e),
    }
}

/// People already found for this role, by provider id, newest first.
pub(crate) async fn found_ids(
    pool: &PgPool,
    org_id: Uuid,
    role_id: Uuid,
) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT p.pdl_id FROM candidacy c JOIN person p ON p.id = c.person_id
         WHERE c.role_id = $1 AND c.org_id = $2 AND p.pdl_id IS NOT NULL
         ORDER BY p.last_seen DESC NULLS LAST, p.id LIMIT $3",
    )
    .bind(role_id)
    .bind(org_id)
    .bind(MAX_EXCLUDED as i64)
    .fetch_all(pool)
    .await?)
}

/// The query, leaving out people already found.
pub(crate) fn excluding(query: Value, found: &[String]) -> Value {
    if found.is_empty() {
        query
    } else {
        serde_json::json!({"bool": {"must": [query], "must_not": [{"terms": {"id": found}}]}})
    }
}

/// POST /api/roles/:id/search/pull: queue one background search per location.
pub async fn pull(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(role_id): Path<Uuid>,
    Json(req): Json<PullRequest>,
) -> Response {
    let Some(pool) = state.pool.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !valid_key(&req.key) {
        return refuse(StatusCode::BAD_REQUEST, "Missing request key.");
    }
    match role_exists(&pool, user.org_id, role_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return server_error(e),
    }
    let brief = match confirmed_brief(&pool, role_id).await {
        Ok(b) => b,
        Err(e) => return server_error(e),
    };
    match blocked(&state, &pool, user.org_id, role_id, brief.is_some()).await {
        Ok(Some(why)) => return refuse(StatusCode::CONFLICT, why),
        Ok(None) => {}
        Err(e) => return server_error(e),
    }
    let Some((brief_id, _, _)) = brief else {
        return refuse(StatusCode::CONFLICT, "Confirm the brief first.");
    };
    type CountedRow = (Uuid, Option<SqlJson<Vec<Counted>>>);
    let count: Result<Option<CountedRow>, _> = sqlx::query_as(
        "SELECT brief_id, locations FROM search_count
         WHERE id = $1 AND role_id = $2 AND org_id = $3 AND search_id IS NULL",
    )
    .bind(req.count_id)
    .bind(role_id)
    .bind(user.org_id)
    .fetch_optional(&pool)
    .await;
    let counted = match count {
        Ok(Some((b, Some(locs)))) if b == brief_id => locs.0,
        Ok(Some((_, None))) => {
            return refuse(
                StatusCode::CONFLICT,
                "That count did not finish. Count again.",
            )
        }
        Ok(Some(_)) => {
            return refuse(
                StatusCode::CONFLICT,
                "The brief changed since this count. Count again first.",
            )
        }
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return server_error(e),
    };

    let mut chosen: Vec<(&Counted, u32)> = Vec::new();
    for p in req.picks.iter().filter(|p| p.size > 0) {
        let Some(c) = counted.iter().find(|c| c.label == p.location) else {
            return refuse(StatusCode::BAD_REQUEST, "That location was not counted.");
        };
        if chosen.iter().any(|(x, _)| x.label == c.label) {
            return refuse(StatusCode::BAD_REQUEST, "Each location once.");
        }
        let most = (c.total.max(0) as u64).min(MAX_PAGE as u64) as u32;
        if p.size > most {
            return refuse(
                StatusCode::BAD_REQUEST,
                format!("{} has {most} to pull at most.", c.label),
            );
        }
        chosen.push((c, p.size));
    }
    let requested: u32 = chosen.iter().map(|(_, n)| n).sum();
    if requested == 0 {
        return refuse(StatusCode::BAD_REQUEST, "Choose how many to pull.");
    }
    if requested > CONFIRM_ABOVE && !req.confirmed {
        return refuse(
            StatusCode::BAD_REQUEST,
            format!("Pulling more than {CONFIRM_ABOVE} people needs a second confirmation."),
        );
    }

    let result = async {
        let mut tx = pool.begin().await?;
        let pull_id: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO pull (org_id, role_id, brief_id, count_id, created_by, requested, locations, idempotency_key)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT DO NOTHING RETURNING id",
        )
        .bind(user.org_id)
        .bind(role_id)
        .bind(brief_id)
        .bind(req.count_id)
        .bind(user.id)
        .bind(requested as i32)
        .bind(chosen.len() as i32)
        .bind(&req.key)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(pull_id) = pull_id else {
            // Pressed twice: the first press already queued everything. Any
            // other pull of this count would pay for the same people again.
            let same_press: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pull WHERE org_id = $1 AND idempotency_key = $2)",
            )
            .bind(user.org_id)
            .bind(&req.key)
            .fetch_one(&mut *tx)
            .await?;
            return anyhow::Ok(same_press);
        };
        for (c, size) in &chosen {
            let payload = serde_json::to_value(PullJob {
                pull_id,
                role_id,
                brief_id,
                actor_id: Some(user.id),
                location: c.label.clone(),
                query: c.query.clone(),
                size: *size,
                search_id: None,
                widen: None,
            })?;
            sqlx::query("INSERT INTO job (org_id, kind, payload) VALUES ($1, $2, $3)")
                .bind(user.org_id)
                .bind(PULL_JOB)
                .bind(payload)
                .execute(&mut *tx)
                .await?;
        }
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::SEARCH_PULLED,
            &format!("pull:{pull_id} role:{role_id} requested:{requested}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(true)
    }
    .await;
    match result {
        Ok(true) => get_search(State(state), user, Path(role_id)).await,
        Ok(false) => refuse(
            StatusCode::CONFLICT,
            "This count has already been pulled. Count again to search for more.",
        ),
        Err(e) => server_error(e),
    }
}

/// One location of a pull, as queued for the worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullJob {
    pub pull_id: Uuid,
    pub role_id: Uuid,
    pub brief_id: Uuid,
    pub actor_id: Option<Uuid>,
    pub location: String,
    pub query: Value,
    pub size: u32,
    /// The wider search this is for, and its widening as it was pulled.
    #[serde(default)]
    pub search_id: Option<Uuid>,
    #[serde(default)]
    pub widen: Option<Widen>,
}

/// Runs queued pulls against a people source.
pub struct PullHandler<S> {
    pub pool: PgPool,
    pub source: Arc<S>,
}

impl<S: PeopleSource + 'static> JobHandler for PullHandler<S> {
    fn kind(&self) -> &'static str {
        PULL_JOB
    }

    fn handle<'a>(&'a self, job: &'a jobs::Job) -> HandlerFuture<'a> {
        Box::pin(async move {
            let p: PullJob =
                serde_json::from_value(job.payload.clone()).context("bad pull payload")?;
            // Search with today's locked-out companies, not those at count
            // time, so a client marked off-limits since is never paid for.
            let brief: crate::roles::BriefRow = sqlx::query_as(&format!(
                "SELECT {} FROM brief WHERE id = $1 AND org_id = $2",
                crate::roles::BRIEF_COLUMNS
            ))
            .bind(p.brief_id)
            .bind(job.org_id)
            .fetch_one(&self.pool)
            .await?;
            let (_, _, brief_lines) = brief.lines();
            let lines = match &p.widen {
                Some(w) => crate::more::apply(&brief_lines, w).1,
                None => brief_lines,
            };
            let locked = employer::locked_out(&self.pool, job.org_id, p.role_id).await?;
            let base = plan::plan(&lines, &locked)
                .into_iter()
                .find(|s| s.label == p.location)
                .map(|s| s.query)
                .unwrap_or(p.query);
            // Never pay again for people already found for this role.
            let found = found_ids(&self.pool, job.org_id, p.role_id).await?;
            let query = excluding(base, &found);
            let req = SearchRequest {
                org_id: job.org_id,
                actor_id: p.actor_id,
                role_id: p.role_id,
                brief_id: p.brief_id,
                // Same pull and location always use the same key, so a retry
                // after a crash never charges twice.
                idempotency_key: format!("pull:{}:{}", p.pull_id, p.location),
                query: SearchQuery {
                    query,
                    size: p.size,
                    scroll_token: None,
                },
                pull_id: Some(p.pull_id),
                location: Some(p.location),
                search_id: p.search_id,
            };
            run_search(&self.pool, self.source.as_ref(), &req).await?;
            // Rank the new people straight away (the ranker skips anyone ranked).
            crate::candidates::queue_rank(&self.pool, job.org_id, p.role_id).await?;
            Ok(())
        })
    }
}
