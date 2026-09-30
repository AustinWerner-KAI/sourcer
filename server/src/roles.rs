//! Clients, roles and the brief (SRS F2, F3, F4, F8).
//!
//! - A role belongs to a client. The client's staff, and every off-limits
//!   client's staff, are locked out of the role and cannot be put back.
//! - Claude drafts the brief from the spec; the resourcer edits it.
//! - A brief is confirmed only when every line is filled and every named tool
//!   is answered. Confirmed versions never change; later edits start a new draft.
//!
//! Every query is scoped to the signed-in user's organisation.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use sqlx::{types::Json as SqlJson, PgPool};
use uuid::Uuid;

use crate::{
    ai::AiError,
    app::AppState,
    audit,
    auth::CurrentUser,
    domain::{
        Brief, BriefDomain, BriefLines, BriefTool, Client, ConfirmBrief, LockedOut, NewClient,
        NewRole, RoleDetail, RoleSummary, RoleUpdate,
    },
    employer,
};

/// The same limit Claude reads, so nothing past it is silently ignored.
const MAX_SPEC_CHARS: usize = crate::ai::MAX_SPEC_CHARS;
const SPEC_TOO_LONG: &str = "The job spec is too long. Keep it under 30,000 characters.";
const MAX_ITEMS: usize = 20;
const MAX_ITEM_CHARS: usize = 120;

fn refuse(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, msg.into()).into_response()
}

fn server_error(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "roles request failed");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

fn unavailable() -> Response {
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

fn clean_text(raw: &str, max: usize) -> Option<String> {
    let s = raw.trim();
    (!s.is_empty() && s.chars().count() <= max).then(|| s.to_string())
}

/// What still stops this brief being confirmed. Empty means ready.
pub fn problems(l: &BriefLines) -> Vec<String> {
    let mut out = Vec::new();
    if l.levels.is_empty() {
        out.push("Choose at least one level.".to_string());
    }
    if l.must_haves.is_empty() {
        out.push("Add at least one must-have.".to_string());
    }
    if l.must_haves.len() > 3 {
        out.push("Keep to three must-haves.".to_string());
    }
    if l.domains.is_empty() {
        out.push("Add at least one domain focus.".to_string());
    }
    let unanswered = l.tools.iter().filter(|t| t.status.is_none()).count();
    if unanswered > 0 {
        out.push(format!(
            "Answer {unanswered} named tool{}.",
            if unanswered == 1 { "" } else { "s" }
        ));
    }
    if l.locations.is_empty() && !l.remote {
        out.push("Add a location, or allow remote.".to_string());
    }
    if l.employer_types.is_empty() {
        out.push("Choose at least one employer type.".to_string());
    }
    out
}

/// Trim every entry, drop blanks and repeats, and refuse oversized input.
fn tidy(mut l: BriefLines) -> Result<BriefLines, &'static str> {
    fn list(v: Vec<String>) -> Result<Vec<String>, &'static str> {
        if v.len() > MAX_ITEMS {
            return Err("Too many entries in one line.");
        }
        let mut out: Vec<String> = Vec::new();
        for s in v {
            let s = s.trim().to_string();
            if s.chars().count() > MAX_ITEM_CHARS {
                return Err("One entry is too long.");
            }
            if !s.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(&s)) {
                out.push(s);
            }
        }
        Ok(out)
    }
    l.levels = list(l.levels)?;
    l.excluded_titles = list(l.excluded_titles)?;
    l.must_haves = list(l.must_haves)?;
    l.capabilities = list(l.capabilities)?;
    l.locations = list(l.locations)?;
    l.employer_types = list(l.employer_types)?;
    l.leave_out = list(l.leave_out)?;
    if l.tools.len() > MAX_ITEMS {
        return Err("Too many named tools.");
    }
    let mut tools: Vec<BriefTool> = Vec::new();
    for t in l.tools {
        let name = t.name.trim().to_string();
        if name.chars().count() > MAX_ITEM_CHARS {
            return Err("One entry is too long.");
        }
        if !name.is_empty() && !tools.iter().any(|o| o.name.eq_ignore_ascii_case(&name)) {
            tools.push(BriefTool {
                name,
                status: t.status,
            });
        }
    }
    l.tools = tools;
    if l.domains.len() > MAX_ITEMS {
        return Err("Too many domains.");
    }
    let mut domains: Vec<BriefDomain> = Vec::new();
    for d in l.domains {
        let name = d.name.trim().to_string();
        if name.chars().count() > MAX_ITEM_CHARS {
            return Err("One entry is too long.");
        }
        if !name.is_empty() && !domains.iter().any(|o| o.name.eq_ignore_ascii_case(&name)) {
            domains.push(BriefDomain {
                name,
                weight: d.weight,
            });
        }
    }
    l.domains = domains;
    Ok(l)
}

// ---------- Clients ----------

type ClientRow = (Uuid, String, Option<String>, bool);

fn client((id, name, domain, off_limits): ClientRow) -> Client {
    Client {
        id,
        name,
        domain,
        off_limits,
    }
}

/// GET /api/clients
pub async fn list_clients(State(state): State<AppState>, user: CurrentUser) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return unavailable();
    };
    let rows: Result<Vec<ClientRow>, _> = sqlx::query_as(
        "SELECT id, name, domain, off_limits FROM client WHERE org_id = $1 ORDER BY lower(name)",
    )
    .bind(user.org_id)
    .fetch_all(pool)
    .await;
    match rows {
        Ok(rows) => Json(rows.into_iter().map(client).collect::<Vec<_>>()).into_response(),
        Err(e) => server_error(e),
    }
}

/// POST /api/clients: the web domain is required, so staff can be matched.
pub async fn create_client(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(new): Json<NewClient>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return unavailable();
    };
    let Some(name) = clean_text(&new.name, 200) else {
        return refuse(StatusCode::BAD_REQUEST, "Enter the client's name.");
    };
    let Some(domain) = employer::normalise_domain(&new.domain) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "Enter the client's web domain, for example example.com.",
        );
    };
    let result = async {
        let mut tx = pool.begin().await?;
        let row: Option<ClientRow> = sqlx::query_as(
            "INSERT INTO client (org_id, name, domain, off_limits) VALUES ($1, $2, $3, $4)
             ON CONFLICT (org_id, lower(domain)) WHERE domain IS NOT NULL DO NOTHING
             RETURNING id, name, domain, off_limits",
        )
        .bind(user.org_id)
        .bind(&name)
        .bind(&domain)
        .bind(new.off_limits)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::CLIENT_CREATED,
            &format!("client:{}", row.0),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(Some(client(row)))
    }
    .await;
    match result {
        Ok(Some(c)) => (StatusCode::CREATED, Json(c)).into_response(),
        Ok(None) => refuse(
            StatusCode::CONFLICT,
            "A client with that domain already exists.",
        ),
        Err(e) => server_error(e),
    }
}

// ---------- Roles ----------

/// GET /api/roles: newest first.
pub async fn list_roles(State(state): State<AppState>, user: CurrentUser) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return unavailable();
    };
    type SummaryRow = (Uuid, String, Option<String>, String);
    let rows: Result<Vec<SummaryRow>, _> = sqlx::query_as(
        "SELECT r.id, r.title, c.name,
                CASE WHEN EXISTS (SELECT 1 FROM brief b WHERE b.role_id = r.id AND b.confirmed_at IS NULL) THEN 'draft'
                     WHEN EXISTS (SELECT 1 FROM brief b WHERE b.role_id = r.id) THEN 'confirmed'
                     ELSE 'none' END
         FROM role r LEFT JOIN client c ON c.id = r.client_id
         WHERE r.org_id = $1
         ORDER BY r.created_at DESC",
    )
    .bind(user.org_id)
    .fetch_all(pool)
    .await;
    match rows {
        Ok(rows) => Json(
            rows.into_iter()
                .map(|(id, title, client_name, brief_state)| RoleSummary {
                    id,
                    title,
                    client_name,
                    brief_state,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => server_error(e),
    }
}

/// POST /api/roles
pub async fn create_role(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(new): Json<NewRole>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return unavailable();
    };
    let Some(title) = clean_text(&new.title, 200) else {
        return refuse(StatusCode::BAD_REQUEST, "Enter the role title.");
    };
    if new.spec_text.chars().count() > MAX_SPEC_CHARS {
        return refuse(StatusCode::BAD_REQUEST, SPEC_TOO_LONG);
    }
    let result = async {
        let mut tx = pool.begin().await?;
        let id: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO role (org_id, client_id, title, owner_id, spec_text)
             SELECT $1, c.id, $3, $4, $5 FROM client c WHERE c.id = $2 AND c.org_id = $1
             RETURNING id",
        )
        .bind(user.org_id)
        .bind(new.client_id)
        .bind(&title)
        .bind(user.id)
        .bind(new.spec_text.trim())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(id) = id else {
            return Ok(None);
        };
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::ROLE_CREATED,
            &format!("role:{id}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(Some(id))
    }
    .await;
    match result {
        Ok(Some(id)) => match role_detail(pool, user.org_id, id).await {
            Ok(Some(d)) => (StatusCode::CREATED, Json(d)).into_response(),
            Ok(None) => StatusCode::NOT_FOUND.into_response(),
            Err(e) => server_error(e),
        },
        Ok(None) => refuse(StatusCode::BAD_REQUEST, "Choose a client."),
        Err(e) => server_error(e),
    }
}

type BriefRow = (
    Uuid,
    i32,
    SqlJson<Vec<String>>,
    SqlJson<Vec<String>>,
    SqlJson<Vec<String>>,
    SqlJson<Vec<String>>,
    SqlJson<Vec<BriefDomain>>,
    SqlJson<Vec<BriefTool>>,
    SqlJson<Vec<String>>,
    bool,
    SqlJson<Vec<String>>,
    SqlJson<Vec<String>>,
    bool,
    bool,
);

const BRIEF_COLUMNS: &str =
    "id, version, levels, excluded_titles, must_haves, capabilities, domains, tools, locations,
     remote, employer_types, leave_out, drafted_by_ai, confirmed_at IS NOT NULL";

fn brief(r: BriefRow) -> Brief {
    Brief {
        id: r.0,
        version: r.1,
        lines: BriefLines {
            levels: r.2 .0,
            excluded_titles: r.3 .0,
            must_haves: r.4 .0,
            capabilities: r.5 .0,
            domains: r.6 .0,
            tools: r.7 .0,
            locations: r.8 .0,
            remote: r.9,
            employer_types: r.10 .0,
            leave_out: r.11 .0,
        },
        drafted_by_ai: r.12,
        confirmed: r.13,
    }
}

/// The role with its client, latest brief and locked-out companies.
async fn role_detail(pool: &PgPool, org_id: Uuid, id: Uuid) -> anyhow::Result<Option<RoleDetail>> {
    let row: Option<(Uuid, String, Option<String>, Option<Uuid>)> = sqlx::query_as(
        "SELECT id, title, spec_text, client_id FROM role WHERE id = $1 AND org_id = $2",
    )
    .bind(id)
    .bind(org_id)
    .fetch_optional(pool)
    .await?;
    let Some((id, title, spec_text, client_id)) = row else {
        return Ok(None);
    };
    let hiring: Option<ClientRow> = match client_id {
        Some(c) => {
            sqlx::query_as("SELECT id, name, domain, off_limits FROM client WHERE id = $1")
                .bind(c)
                .fetch_optional(pool)
                .await?
        }
        None => None,
    };
    let latest: Option<BriefRow> = sqlx::query_as(&format!(
        "SELECT {BRIEF_COLUMNS} FROM brief WHERE role_id = $1
         ORDER BY confirmed_at IS NULL DESC, version DESC LIMIT 1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let locked_out = employer::locked_out(pool, org_id, id)
        .await?
        .into_iter()
        .map(|c| LockedOut {
            id: c.id,
            name: c.name,
            hiring: c.hiring,
        })
        .collect();
    Ok(Some(RoleDetail {
        id,
        title,
        client: hiring.map(client),
        spec_text: spec_text.unwrap_or_default(),
        brief: latest.map(brief),
        locked_out,
    }))
}

/// GET /api/roles/:id
pub async fn get_role(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return unavailable();
    };
    match role_detail(pool, user.org_id, id).await {
        Ok(Some(d)) => Json(d).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => server_error(e),
    }
}

/// PATCH /api/roles/:id: change the title, the spec or the client.
pub async fn update_role(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(change): Json<RoleUpdate>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return unavailable();
    };
    let Some(title) = clean_text(&change.title, 200) else {
        return refuse(StatusCode::BAD_REQUEST, "Enter the role title.");
    };
    if change.spec_text.chars().count() > MAX_SPEC_CHARS {
        return refuse(StatusCode::BAD_REQUEST, SPEC_TOO_LONG);
    }
    if let Some(client_id) = change.client_id {
        let ours: Result<bool, _> = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM client WHERE id = $1 AND org_id = $2)",
        )
        .bind(client_id)
        .bind(user.org_id)
        .fetch_one(pool)
        .await;
        match ours {
            Ok(true) => {}
            Ok(false) => return refuse(StatusCode::BAD_REQUEST, "Choose a client."),
            Err(e) => return server_error(e),
        }
    }
    let result = async {
        let mut tx = pool.begin().await?;
        let current: Option<Option<Uuid>> = sqlx::query_scalar(
            "SELECT client_id FROM role WHERE id = $1 AND org_id = $2 FOR UPDATE",
        )
        .bind(id)
        .bind(user.org_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(current) = current else {
            return Ok(Err(StatusCode::NOT_FOUND.into_response()));
        };
        // The client is set once. Changing it later would leave people already
        // checked against the old client unchecked against the new one.
        if let (Some(old), Some(new)) = (current, change.client_id) {
            if old != new {
                return Ok(Err(refuse(
                    StatusCode::CONFLICT,
                    "The client cannot be changed once set. Create a new role instead.",
                )));
            }
        }
        sqlx::query(
            "UPDATE role SET title = $2, spec_text = $3, client_id = coalesce(client_id, $4)
             WHERE id = $1",
        )
        .bind(id)
        .bind(&title)
        .bind(change.spec_text.trim())
        .bind(change.client_id)
        .execute(&mut *tx)
        .await?;
        if let (None, Some(new)) = (current, change.client_id) {
            audit::record(
                &mut *tx,
                user.org_id,
                Some(user.id),
                audit::action::ROLE_CLIENT_SET,
                &format!("role:{id} client:{new}"),
            )
            .await?;
        }
        tx.commit().await?;
        anyhow::Ok(Ok(()))
    }
    .await;
    match result {
        Ok(Ok(())) => get_role(State(state), user, Path(id)).await,
        Ok(Err(r)) => r,
        Err(e) => server_error(e),
    }
}

// ---------- Brief ----------

type Tx<'a> = sqlx::Transaction<'a, sqlx::Postgres>;

/// Lock the role so brief changes run one at a time. `None` if the role is
/// not in this organisation, else whether it has a client.
async fn lock_role(tx: &mut Tx<'_>, org_id: Uuid, role_id: Uuid) -> anyhow::Result<Option<bool>> {
    Ok(sqlx::query_scalar(
        "SELECT client_id IS NOT NULL FROM role WHERE id = $1 AND org_id = $2 FOR UPDATE",
    )
    .bind(role_id)
    .bind(org_id)
    .fetch_optional(&mut **tx)
    .await?)
}

/// The latest lines for a role: the open draft, else the last confirmed.
async fn latest_lines(tx: &mut Tx<'_>, role_id: Uuid) -> anyhow::Result<Option<BriefLines>> {
    let row: Option<BriefRow> = sqlx::query_as(&format!(
        "SELECT {BRIEF_COLUMNS} FROM brief WHERE role_id = $1
         ORDER BY confirmed_at IS NULL DESC, version DESC LIMIT 1"
    ))
    .bind(role_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|r| brief(r).lines))
}

/// Write the role's open draft, or start a new version if none is open.
/// The caller holds the role lock. Returns the draft's id and version.
async fn write_draft(
    tx: &mut Tx<'_>,
    org_id: Uuid,
    role_id: Uuid,
    lines: &BriefLines,
    drafted_by_ai: Option<bool>,
) -> anyhow::Result<(Uuid, i32)> {
    let updated: Option<(Uuid, i32)> = sqlx::query_as(
        "UPDATE brief SET levels = $2, excluded_titles = $3, must_haves = $4, tools = $5,
                locations = $6, remote = $7, employer_types = $8, leave_out = $9,
                drafted_by_ai = coalesce($10, drafted_by_ai), capabilities = $11, domains = $12
         WHERE role_id = $1 AND confirmed_at IS NULL
         RETURNING id, version",
    )
    .bind(role_id)
    .bind(SqlJson(&lines.levels))
    .bind(SqlJson(&lines.excluded_titles))
    .bind(SqlJson(&lines.must_haves))
    .bind(SqlJson(&lines.tools))
    .bind(SqlJson(&lines.locations))
    .bind(lines.remote)
    .bind(SqlJson(&lines.employer_types))
    .bind(SqlJson(&lines.leave_out))
    .bind(drafted_by_ai)
    .bind(SqlJson(&lines.capabilities))
    .bind(SqlJson(&lines.domains))
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(done) = updated {
        return Ok(done);
    }
    Ok(sqlx::query_as(
        "INSERT INTO brief (org_id, role_id, version, levels, excluded_titles, must_haves, tools,
                            locations, remote, employer_types, leave_out, drafted_by_ai,
                            capabilities, domains)
         SELECT $1, $2, coalesce(max(version), 0) + 1, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                $12, $13
         FROM brief WHERE role_id = $2
         RETURNING id, version",
    )
    .bind(org_id)
    .bind(role_id)
    .bind(SqlJson(&lines.levels))
    .bind(SqlJson(&lines.excluded_titles))
    .bind(SqlJson(&lines.must_haves))
    .bind(SqlJson(&lines.tools))
    .bind(SqlJson(&lines.locations))
    .bind(lines.remote)
    .bind(SqlJson(&lines.employer_types))
    .bind(SqlJson(&lines.leave_out))
    .bind(drafted_by_ai.unwrap_or(false))
    .bind(SqlJson(&lines.capabilities))
    .bind(SqlJson(&lines.domains))
    .fetch_one(&mut **tx)
    .await?)
}

/// A fresh draft from Claude replaces the spec-based lines (levels,
/// must-haves, capabilities, domains, tools, locations) but keeps the
/// resourcer's own choices: excluded titles, employer types, leave-out list,
/// every tool answer for a tool that is still named, and the Must or Plus
/// choice for a domain that is still named.
pub fn merge_redraft(fresh: BriefLines, old: Option<BriefLines>) -> BriefLines {
    let Some(old) = old else {
        return fresh;
    };
    let tools = fresh
        .tools
        .into_iter()
        .map(|t| {
            let status = old
                .tools
                .iter()
                .find(|o| o.name.eq_ignore_ascii_case(&t.name))
                .and_then(|o| o.status);
            BriefTool { status, ..t }
        })
        .collect();
    let domains = fresh
        .domains
        .into_iter()
        .map(|d| {
            let weight = old
                .domains
                .iter()
                .find(|o| o.name.eq_ignore_ascii_case(&d.name))
                .map_or(d.weight, |o| o.weight);
            BriefDomain { weight, ..d }
        })
        .collect();
    BriefLines {
        tools,
        domains,
        excluded_titles: if old.excluded_titles.is_empty() {
            fresh.excluded_titles
        } else {
            old.excluded_titles
        },
        employer_types: if old.employer_types.is_empty() {
            fresh.employer_types
        } else {
            old.employer_types
        },
        leave_out: old.leave_out,
        ..fresh
    }
}

/// POST /api/roles/:id/brief/draft: Claude drafts the brief from the spec.
pub async fn draft_brief(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return unavailable();
    };
    if !state.draft_limit.allow(user.id) {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many drafts in a short time. Wait a few minutes, then try again.",
        );
    }
    let spec: Option<Option<String>> =
        match sqlx::query_scalar("SELECT spec_text FROM role WHERE id = $1 AND org_id = $2")
            .bind(id)
            .bind(user.org_id)
            .fetch_optional(pool)
            .await
        {
            Ok(s) => s,
            Err(e) => return server_error(e),
        };
    let Some(spec) = spec else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let fresh = match state.ai.draft_brief(spec.as_deref().unwrap_or("")).await {
        Ok(l) => l,
        Err(AiError::NotConfigured) => {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "Drafting is not set up yet. Fill in the brief yourself for now.",
            )
        }
        Err(AiError::EmptySpec) => {
            return refuse(StatusCode::BAD_REQUEST, "Paste the job spec first.")
        }
        Err(e) => {
            tracing::warn!(error = %e, "brief draft failed");
            return refuse(
                StatusCode::BAD_GATEWAY,
                "Claude could not draft the brief just now. Try again, or fill it in yourself.",
            );
        }
    };
    let result = async {
        let mut tx = pool.begin().await?;
        if lock_role(&mut tx, user.org_id, id).await?.is_none() {
            return Ok(false);
        }
        let old = latest_lines(&mut tx, id).await?;
        let lines = merge_redraft(fresh, old);
        let (brief_id, version) = write_draft(&mut tx, user.org_id, id, &lines, Some(true)).await?;
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::BRIEF_DRAFTED,
            &format!("brief:{brief_id} role:{id} v{version}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(true)
    }
    .await;
    match result {
        Ok(true) => get_role(State(state), user, Path(id)).await,
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => server_error(e),
    }
}

/// PUT /api/roles/:id/brief: save the resourcer's edits to the draft.
pub async fn save_brief(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(lines): Json<BriefLines>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return unavailable();
    };
    let lines = match tidy(lines) {
        Ok(l) => l,
        Err(msg) => return refuse(StatusCode::BAD_REQUEST, msg),
    };
    let result = async {
        let mut tx = pool.begin().await?;
        if lock_role(&mut tx, user.org_id, id).await?.is_none() {
            return Ok(false);
        }
        let (brief_id, version) = write_draft(&mut tx, user.org_id, id, &lines, None).await?;
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::BRIEF_SAVED,
            &format!("brief:{brief_id} role:{id} v{version}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(true)
    }
    .await;
    match result {
        Ok(true) => get_role(State(state), user, Path(id)).await,
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => server_error(e),
    }
}

/// POST /api/roles/:id/brief/confirm: saves exactly the lines the resourcer
/// is looking at, checks them, and locks them, all in one step.
pub async fn confirm_brief(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<ConfirmBrief>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return unavailable();
    };
    let lines = match tidy(req.lines) {
        Ok(l) => l,
        Err(msg) => return refuse(StatusCode::BAD_REQUEST, msg),
    };
    let issues = problems(&lines);
    if !issues.is_empty() {
        return refuse(StatusCode::BAD_REQUEST, issues.join(" "));
    }
    let result = async {
        let mut tx = pool.begin().await?;
        match lock_role(&mut tx, user.org_id, id).await? {
            None => return Ok(Err(StatusCode::NOT_FOUND.into_response())),
            Some(false) => {
                return Ok(Err(refuse(
                    StatusCode::BAD_REQUEST,
                    "Choose the client for this role first, so their staff are kept out.",
                )))
            }
            Some(true) => {}
        }
        let latest: Option<i32> =
            sqlx::query_scalar("SELECT max(version) FROM brief WHERE role_id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        if latest != req.based_on {
            return Ok(Err(refuse(
                StatusCode::CONFLICT,
                "This brief changed in another window. Reload the page to see the latest.",
            )));
        }
        let (brief_id, version) = write_draft(&mut tx, user.org_id, id, &lines, None).await?;
        sqlx::query("UPDATE brief SET confirmed_by = $2, confirmed_at = now() WHERE id = $1")
            .bind(brief_id)
            .bind(user.id)
            .execute(&mut *tx)
            .await?;
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::BRIEF_CONFIRMED,
            &format!("brief:{brief_id} role:{id} v{version}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(Ok(()))
    }
    .await;
    match result {
        Ok(Ok(())) => get_role(State(state), user, Path(id)).await,
        Ok(Err(r)) => r,
        Err(e) => server_error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DomainWeight, ToolStatus};

    fn dom(name: &str, weight: DomainWeight) -> BriefDomain {
        BriefDomain {
            name: name.into(),
            weight,
        }
    }

    fn ready() -> BriefLines {
        BriefLines {
            levels: vec!["Senior".into()],
            excluded_titles: vec!["Director".into()],
            must_haves: vec!["IAM".into()],
            capabilities: vec![],
            domains: vec![dom("Custody", DomainWeight::Must)],
            tools: vec![BriefTool {
                name: "Okta".into(),
                status: Some(ToolStatus::Required),
            }],
            locations: vec!["Dubai".into()],
            remote: false,
            employer_types: vec!["Trading firms".into()],
            leave_out: vec![],
        }
    }

    #[test]
    fn a_complete_brief_has_no_problems() {
        assert!(problems(&ready()).is_empty());
        let remote_only = BriefLines {
            locations: vec![],
            remote: true,
            ..ready()
        };
        assert!(problems(&remote_only).is_empty());
    }

    #[test]
    fn every_tool_must_be_answered() {
        let mut l = ready();
        l.tools.push(BriefTool {
            name: "CyberArk".into(),
            status: None,
        });
        l.tools.push(BriefTool {
            name: "Terraform".into(),
            status: None,
        });
        assert_eq!(problems(&l), ["Answer 2 named tools."]);
    }

    #[test]
    fn missing_lines_are_named() {
        let l = BriefLines::default();
        let p = problems(&l);
        assert_eq!(p.len(), 5, "{p:?}");
        assert!(p.contains(&"Add at least one domain focus.".to_string()));
    }

    #[tokio::test]
    async fn the_database_refuses_to_change_a_confirmed_brief() {
        let Some(pool) = crate::testutil::pool().await else {
            return;
        };
        let org = crate::testutil::org(&pool).await;
        let (_, brief) = crate::testutil::role_with_brief(&pool, org).await;
        let changed = sqlx::query("UPDATE brief SET remote = true WHERE id = $1")
            .bind(brief)
            .execute(&pool)
            .await;
        assert!(changed.is_err(), "confirmed briefs are final");
    }

    #[test]
    fn redrafting_keeps_the_resourcers_own_choices() {
        let mut old = ready();
        old.leave_out = vec!["Some Bank".into()];
        old.excluded_titles = vec!["VP".into()];
        old.employer_types = vec!["Adtech".into()];
        let fresh = BriefLines {
            levels: vec!["Lead".into()],
            excluded_titles: vec!["Director".into()],
            must_haves: vec!["Go".into()],
            capabilities: vec!["Mentoring".into()],
            domains: vec![
                dom("custody", DomainWeight::Plus),
                dom("Payments", DomainWeight::Must),
            ],
            tools: vec![
                BriefTool {
                    name: "OKTA".into(),
                    status: None,
                },
                BriefTool {
                    name: "Vault".into(),
                    status: None,
                },
            ],
            locations: vec!["London".into()],
            remote: true,
            employer_types: vec!["Trading firms".into()],
            leave_out: vec![],
        };
        let m = merge_redraft(fresh, Some(old));
        assert_eq!(m.levels, ["Lead"], "spec lines come from the new draft");
        assert_eq!(m.locations, ["London"]);
        assert_eq!(m.leave_out, ["Some Bank"]);
        assert_eq!(m.excluded_titles, ["VP"]);
        assert_eq!(m.employer_types, ["Adtech"]);
        assert_eq!(m.tools[0].status, Some(ToolStatus::Required), "answer kept");
        assert_eq!(m.tools[1].status, None, "new tool still needs an answer");
        assert_eq!(
            m.capabilities,
            ["Mentoring"],
            "capabilities come from the new draft"
        );
        assert_eq!(
            m.domains,
            [
                dom("custody", DomainWeight::Must),
                dom("Payments", DomainWeight::Must)
            ],
            "Must or Plus kept for a domain still named; new ones as drafted"
        );
    }

    #[test]
    fn tidy_trims_dedupes_and_limits() {
        let mut l = ready();
        l.locations = vec![" Dubai ".into(), "dubai".into(), "".into()];
        l.tools.push(BriefTool {
            name: " okta ".into(),
            status: None,
        });
        l.capabilities = vec![" Mentoring ".into(), "mentoring".into()];
        l.domains.push(dom(" custody ", DomainWeight::Plus));
        l.domains.push(dom(" ", DomainWeight::Plus));
        let t = tidy(l).unwrap();
        assert_eq!(t.locations, ["Dubai"]);
        assert_eq!(t.tools.len(), 1);
        assert_eq!(t.capabilities, ["Mentoring"]);
        assert_eq!(
            t.domains,
            [dom("Custody", DomainWeight::Must)],
            "first one wins"
        );
        let mut long = ready();
        long.must_haves = vec!["x".repeat(121)];
        assert!(tidy(long).is_err());
        let mut many = ready();
        many.leave_out = vec!["a".into(); 21];
        assert!(tidy(many).is_err());
        let mut many = ready();
        many.domains = (0..21)
            .map(|i| dom(&i.to_string(), DomainWeight::Plus))
            .collect();
        assert!(tidy(many).is_err());
        let mut long = ready();
        long.domains = vec![dom(&"x".repeat(121), DomainWeight::Plus)];
        assert!(tidy(long).is_err());
    }
}
