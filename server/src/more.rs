//! More searches for a role (Kai, 2 Oct 2026): one search is never enough,
//! so beside the brief a role has up to three wider searches. Claude chooses
//! two (slots 1 and 2); slot 3 is the resourcer's own.
//!
//! - Each is the confirmed brief with relaxing moves applied (`retune::relax`),
//!   so it can only widen, and everyone found is ranked against the brief.
//! - Only the places where it differs from the brief are counted and pulled,
//!   so nothing is paid for twice.
//! - Counts and pulls leave out people already found for the role, so a count
//!   says how many are new.
//! - Confirming the brief again starts afresh: searches belong to one version.
//! - A count is of one version of the search; changing the search makes it stale.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{types::Json as SqlJson, PgPool};
use uuid::Uuid;

use crate::{
    ai::AiError,
    app::AppState,
    audit,
    auth::CurrentUser,
    domain::{
        BriefLines, CountLocation, CountRequest, DomainWeight, MoreCount, MorePullRequest,
        MoreSearch, SaveSearch, ToolStatus, Widen,
    },
    employer::Company,
    plan::{self, LocationSearch},
    retune::{self, Reply},
    searching::{
        blocked, confirmed_brief, do_count, refuse, role_exists, server_error, state_response,
        valid_key, Counted, PullJob, CONFIRM_ABOVE, PULL_JOB,
    },
    sources::pdl::MAX_PAGE,
};

/// Claude's picks are slots 1 and 2.
pub const CLAUDE_SLOTS: [i32; 2] = [1, 2];
/// The resourcer's own search.
pub const OWN_SLOT: i32 = 3;
pub const OWN_NAME: &str = "Your search";
const MAX_NAME_CHARS: usize = 40;
const MAX_NOTE_CHARS: usize = 200;
/// Entries read from any one list of a widening; the search caps cut further.
const MAX_LIST: usize = 20;

fn moves(w: &Widen) -> Reply {
    let take = |v: &[String]| v.iter().take(MAX_LIST).cloned().collect::<Vec<_>>();
    Reply {
        tools_to_nice: take(&w.tools_to_nice),
        domains_to_plus: take(&w.domains_to_plus),
        add_titles: take(&w.add_titles),
        add_levels: take(&w.add_levels),
        min_years: w
            .min_years
            .map(|y| if y <= 0 { Value::Null } else { json!(y) }),
        add_employer_types: take(&w.add_employer_types),
        add_locations: take(&w.add_locations),
        allow_remote: false,
    }
}

/// Apply a widening to the brief. Returns the widening as it took effect
/// (entries that changed nothing dropped, names as the brief spells them)
/// and the widened lines.
pub fn apply(brief: &BriefLines, w: &Widen) -> (Widen, BriefLines) {
    let wide = retune::relax(brief, &moves(w));
    let added = |old: &[String], new: &[String]| new.get(old.len()..).unwrap_or_default().to_vec();
    let took = Widen {
        add_titles: added(&brief.titles, &wide.titles),
        add_levels: added(&brief.levels, &wide.levels),
        add_locations: added(&brief.locations, &wide.locations),
        add_employer_types: added(&brief.employer_types, &wide.employer_types),
        tools_to_nice: brief
            .tools
            .iter()
            .zip(&wide.tools)
            .filter(|(a, b)| a.status == Some(ToolStatus::Required) && b.status != a.status)
            .map(|(a, _)| a.name.clone())
            .collect(),
        domains_to_plus: brief
            .domains
            .iter()
            .zip(&wide.domains)
            .filter(|(a, b)| a.weight == DomainWeight::Must && b.weight != a.weight)
            .map(|(a, _)| a.name.clone())
            .collect(),
        min_years: (wide.min_years != brief.min_years).then(|| wide.min_years.unwrap_or(0)),
    };
    (took, wide)
}

/// Nothing in it widens the brief.
pub fn is_empty(w: &Widen) -> bool {
    *w == Widen::default()
}

/// The searches a wider search runs: one per place where its query differs
/// from the brief's. A place searched the same way is already the brief's.
pub fn searches(brief: &BriefLines, wide: &BriefLines, locked: &[Company]) -> Vec<LocationSearch> {
    let base = plan::plan(brief, locked);
    plan::plan(wide, locked)
        .into_iter()
        .filter(|s| {
            !base
                .iter()
                .any(|b| b.label == s.label && b.query == s.query)
        })
        .collect()
}

/// Spread `size` people over a count's places, in proportion to what each
/// can give (at most one page each). `Err` carries the most it can give.
pub fn split(size: u32, places: &[CountLocation]) -> Result<Vec<(String, u32)>, u32> {
    let caps: Vec<u32> = places
        .iter()
        .map(|c| (c.total.max(0) as u64).min(MAX_PAGE as u64) as u32)
        .collect();
    let most: u32 = caps.iter().sum();
    if size > most {
        return Err(most);
    }
    let mut out: Vec<u32> = caps
        .iter()
        .map(|c| (size as u64 * *c as u64 / most.max(1) as u64) as u32)
        .collect();
    let mut left = size - out.iter().sum::<u32>();
    let mut order: Vec<usize> = (0..caps.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(caps[i]));
    while left > 0 {
        for &i in &order {
            if left > 0 && out[i] < caps[i] {
                out[i] += 1;
                left -= 1;
            }
        }
    }
    Ok(places
        .iter()
        .zip(out)
        .filter(|(_, n)| *n > 0)
        .map(|(c, n)| (c.label.clone(), n))
        .collect())
}

/// The wider searches of this brief version, as the Search screen shows them.
pub async fn views(
    pool: &PgPool,
    org_id: Uuid,
    brief_id: Uuid,
    lines: &BriefLines,
) -> anyhow::Result<Vec<MoreSearch>> {
    type Row = (
        i16,
        bool,
        String,
        String,
        SqlJson<Widen>,
        i32,
        Option<Uuid>,
        Option<i64>,
        Option<i32>,
        Option<SqlJson<Vec<Counted>>>,
        Option<i32>,
        bool,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT e.slot, e.by_claude, e.name, e.note, e.widen, e.version,
                x.id, extract(epoch FROM x.created_at)::bigint, x.search_version, x.locations,
                x.credits_used, EXISTS (SELECT 1 FROM pull p WHERE p.count_id = x.id)
         FROM extra_search e
         LEFT JOIN LATERAL (
           SELECT c.* FROM search_count c
           WHERE c.search_id = e.id AND c.locations IS NOT NULL
           ORDER BY c.created_at DESC LIMIT 1
         ) x ON true
         WHERE e.brief_id = $1 AND e.org_id = $2
         ORDER BY e.slot",
    )
    .bind(brief_id)
    .bind(org_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                slot,
                by_claude,
                name,
                note,
                widen,
                version,
                cid,
                at,
                cver,
                locs,
                credits,
                pulled,
            )| {
                let (_, wide) = apply(lines, &widen.0);
                let count = match (cid, locs) {
                    (Some(id), Some(locs)) => Some(MoreCount {
                        id,
                        counted_at: at.unwrap_or_default(),
                        stale: cver != Some(version),
                        locations: locs
                            .0
                            .into_iter()
                            .map(|c| CountLocation {
                                label: c.label,
                                total: c.total,
                            })
                            .collect(),
                        credits_used: credits.unwrap_or_default(),
                        pulled,
                    }),
                    _ => None,
                };
                MoreSearch {
                    slot: slot.into(),
                    by_claude,
                    name,
                    note,
                    locations: searches(lines, &wide, &[])
                        .into_iter()
                        .map(|s| s.label)
                        .collect(),
                    widen: widen.0,
                    version,
                    count,
                }
            },
        )
        .collect())
}

// ---- Claude's picks -------------------------------------------------------

const INSTRUCTIONS: &str = "You help a recruiter find more suitable people for a job. The \
recruiter's search from the brief has run; one search is never enough, so you choose two \
WIDER searches to run beside it. Answer only by calling the record_searches tool, once, with \
no other text.

How the search works (People Data Labs): a person is found only if ALL of these hold. Their \
current job title contains one of the titles AND one of the levels, and none of the excluded \
titles. Each required tool must appear in their headline, summary, job summary or skills list; \
many profiles have no summary, so every required tool removes many people. At least one Must \
domain must appear in the same places. Their employer's industry must fit one of the employer \
types. They must have at least the fewest years. Each location is a separate search. \
Everything else only ranks people and never removes anyone.

Choose exactly two searches, each a different idea, each likely to find people who could do \
this exact job. Good ideas: close job titles the same work goes by at other firms; nearby \
markets people often move from; employer types where the same skills are used; a niche required \
tool or Must domain made optional; fewer years. Use the counts: if the brief found very few, \
widen more boldly; if it found many, stay close. Never add a title so broad it matches \
unrelated work, and never invent requirements.

For each search: name, two or three words saying what it widens (e.g. \"Wider titles\", \
\"Nearby markets\"). note: one short plain sentence on what it adds and why. add_titles, \
add_levels, add_locations (city names): new ones only. add_employer_types: only from the \
allowed list. tools_to_nice: required tools, exactly as given, to make nice to have. \
domains_to_plus: Must domains, exactly as given, to make Plus. min_years: lower than now, 0 \
for any, or null to keep.";

/// Claude's reply: two wider searches.
#[derive(Debug, Default, Deserialize)]
pub struct Suggestions {
    #[serde(default)]
    pub searches: Vec<Suggestion>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Suggestion {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub note: String,
    #[serde(flatten)]
    pub moves: Reply,
}

impl Suggestion {
    /// The moves as a widening. Read leniently: a bad years value keeps the brief's.
    pub fn widen(&self) -> Widen {
        let m = &self.moves;
        // Null and absent both keep the brief's years; 0 means any.
        let min_years = match &m.min_years {
            Some(Value::Number(n)) => n.as_f64().map(|y| y.round() as i32),
            Some(Value::String(t)) => t.trim().parse::<f64>().ok().map(|y| y.round() as i32),
            _ => None,
        };
        Widen {
            add_titles: m.add_titles.clone(),
            add_levels: m.add_levels.clone(),
            add_locations: m.add_locations.clone(),
            add_employer_types: m.add_employer_types.clone(),
            tools_to_nice: m.tools_to_nice.clone(),
            domains_to_plus: m.domains_to_plus.clone(),
            min_years,
        }
    }
}

fn trimmed(s: &str, max: usize) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

/// The request body for Claude.
pub fn request(model: &str, b: &BriefLines, counts: &[CountLocation]) -> Value {
    let list = json!({"type": "array", "items": {"type": "string"}});
    json!({
        "model": model,
        "max_tokens": 2048,
        "system": INSTRUCTIONS,
        "tools": [{
            "name": "record_searches",
            "description": "Record the two wider searches to run beside the brief.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "searches": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "note": {"type": "string"},
                                "add_titles": list, "add_levels": list, "add_locations": list,
                                "add_employer_types": {"type": "array", "items": {"type": "string", "enum": retune::EMPLOYER_TYPES}},
                                "tools_to_nice": list, "domains_to_plus": list,
                                "min_years": {"type": ["integer", "null"]}
                            },
                            "required": ["name", "note", "add_titles", "add_levels",
                                         "add_locations", "add_employer_types",
                                         "tools_to_nice", "domains_to_plus"]
                        }
                    }
                },
                "required": ["searches"]
            }
        }],
        // Newer models refuse a forced tool; a reply without it is an error.
        "tool_choice": {"type": "auto"},
        "messages": [{"role": "user", "content": format!(
            "<search>\n{}\n</search>",
            serde_json::to_string_pretty(&retune::situation(b, counts)).unwrap_or_default()
        )}]
    })
}

// ---- Handlers -------------------------------------------------------------

/// The role's confirmed brief, or the response that says why not.
#[allow(clippy::result_large_err)]
async fn current(
    state: &AppState,
    user: &CurrentUser,
    role_id: Uuid,
) -> Result<(PgPool, Uuid, i32, BriefLines), Response> {
    let Some(pool) = state.pool.clone() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE.into_response());
    };
    match role_exists(&pool, user.org_id, role_id).await {
        Ok(true) => {}
        Ok(false) => return Err(StatusCode::NOT_FOUND.into_response()),
        Err(e) => return Err(server_error(e)),
    }
    match confirmed_brief(&pool, role_id).await {
        Ok(Some((id, version, lines))) => Ok((pool, id, version, lines)),
        Ok(None) => Err(refuse(StatusCode::CONFLICT, "Confirm the brief first.")),
        Err(e) => Err(server_error(e)),
    }
}

/// POST /api/roles/:id/searches/suggest: Claude chooses the two wider
/// searches for the confirmed brief. Free of search credits; once chosen, a
/// second call returns them without asking again.
pub async fn suggest(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(role_id): Path<Uuid>,
) -> Response {
    let (pool, brief_id, version, lines) = match current(&state, &user, role_id).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let taken: Result<Vec<(i16, SqlJson<Widen>)>, _> =
        sqlx::query_as("SELECT slot, widen FROM extra_search WHERE brief_id = $1 AND org_id = $2")
            .bind(brief_id)
            .bind(user.org_id)
            .fetch_all(&pool)
            .await;
    let taken = match taken {
        Ok(t) => t,
        Err(e) => return server_error(e),
    };
    let free: Vec<i32> = CLAUDE_SLOTS
        .into_iter()
        .filter(|s| !taken.iter().any(|(t, _)| i32::from(*t) == *s))
        .collect();
    if free.is_empty() {
        return state_response(&state, &pool, user.org_id, role_id).await;
    }
    // Claude reads how the brief's search did.
    let counted: Result<Option<SqlJson<Vec<CountLocation>>>, _> = sqlx::query_scalar(
        "SELECT locations FROM search_count
         WHERE role_id = $1 AND org_id = $2 AND brief_id = $3 AND search_id IS NULL
           AND locations IS NOT NULL
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(role_id)
    .bind(user.org_id)
    .bind(brief_id)
    .fetch_optional(&pool)
    .await;
    let counts = match counted {
        Ok(Some(c)) => c.0,
        Ok(None) => return refuse(StatusCode::CONFLICT, "Count the brief's search first."),
        Err(e) => return server_error(e),
    };
    if !state.draft_limit.allow(user.id) {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests to Claude in a short time. Wait a few minutes, then try again.",
        );
    }
    let reply = match state.ai.suggest_searches(&lines, &counts).await {
        Ok(r) => r,
        Err(AiError::NotConfigured) => {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "Claude is not set up yet (no key). Set up your own search for now.",
            )
        }
        Err(e) => {
            tracing::warn!(error = %e, "choosing wider searches failed");
            return refuse(
                StatusCode::BAD_GATEWAY,
                "Claude could not choose searches just now. Try again, or set up your own.",
            );
        }
    };

    // Keep the usable ones: they widen, the brief can still run, and no two match.
    let mut seen: Vec<Widen> = taken.into_iter().map(|(_, w)| w.0).collect();
    let mut picks = Vec::new();
    for s in &reply.searches {
        let (w, wide) = apply(&lines, &s.widen());
        if is_empty(&w) || !crate::roles::problems(&wide).is_empty() || seen.contains(&w) {
            continue;
        }
        seen.push(w.clone());
        let name = match trimmed(&s.name, MAX_NAME_CHARS) {
            n if n.is_empty() => "Wider search".to_string(),
            n => n,
        };
        picks.push((name, trimmed(&s.note, MAX_NOTE_CHARS), w));
    }
    if picks.is_empty() {
        tracing::warn!("Claude's wider searches were all unusable");
        return refuse(
            StatusCode::BAD_GATEWAY,
            "Claude's searches could not be used. Try again, or set up your own.",
        );
    }
    let result = async {
        let mut tx = pool.begin().await?;
        let mut slots = Vec::new();
        for (slot, (name, note, w)) in free.iter().zip(picks) {
            let added = sqlx::query(
                "INSERT INTO extra_search (org_id, role_id, brief_id, slot, by_claude, name, note, widen, created_by)
                 VALUES ($1, $2, $3, $4, true, $5, $6, $7, $8)
                 ON CONFLICT (brief_id, slot) DO NOTHING",
            )
            .bind(user.org_id)
            .bind(role_id)
            .bind(brief_id)
            .bind(*slot as i16)
            .bind(name)
            .bind(note)
            .bind(SqlJson(&w))
            .bind(user.id)
            .execute(&mut *tx)
            .await?;
            if added.rows_affected() > 0 {
                slots.push(slot.to_string());
            }
        }
        if !slots.is_empty() {
            audit::record(
                &mut *tx,
                user.org_id,
                Some(user.id),
                audit::action::SEARCHES_SUGGESTED,
                &format!("role:{role_id} v{version} slots:{}", slots.join(",")),
            )
            .await?;
        }
        tx.commit().await?;
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => state_response(&state, &pool, user.org_id, role_id).await,
        Err(e) => server_error(e),
    }
}

/// PUT /api/roles/:id/searches/:slot: set what a search widens. Claude's
/// picks can be changed (an addition removed); slot 3 is the resourcer's own.
/// Free: nothing is searched until it is counted.
pub async fn save(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((role_id, slot)): Path<(Uuid, i32)>,
    Json(req): Json<SaveSearch>,
) -> Response {
    if !(1..=OWN_SLOT).contains(&slot) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let (pool, brief_id, version, lines) = match current(&state, &user, role_id).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let (w, wide) = apply(&lines, &req.widen);
    if is_empty(&w) {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Nothing here widens the brief. Add a title, level or place.",
        );
    }
    if let Some(p) = crate::roles::problems(&wide).into_iter().next() {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, p);
    }
    let saved: Result<Option<Uuid>, _> = if slot == OWN_SLOT {
        sqlx::query_scalar(
            "INSERT INTO extra_search (org_id, role_id, brief_id, slot, by_claude, name, widen, created_by)
             VALUES ($1, $2, $3, $4, false, $5, $6, $7)
             ON CONFLICT (brief_id, slot) DO UPDATE SET
               widen = EXCLUDED.widen,
               version = extra_search.version + (extra_search.widen IS DISTINCT FROM EXCLUDED.widen)::int,
               updated_at = now()
             RETURNING id",
        )
        .bind(user.org_id)
        .bind(role_id)
        .bind(brief_id)
        .bind(slot as i16)
        .bind(OWN_NAME)
        .bind(SqlJson(&w))
        .bind(user.id)
        .fetch_optional(&pool)
        .await
    } else {
        sqlx::query_scalar(
            "UPDATE extra_search SET
               version = version + (widen IS DISTINCT FROM $4)::int,
               widen = $4, updated_at = now()
             WHERE brief_id = $1 AND org_id = $2 AND slot = $3
             RETURNING id",
        )
        .bind(brief_id)
        .bind(user.org_id)
        .bind(slot as i16)
        .bind(SqlJson(&w))
        .fetch_optional(&pool)
        .await
    };
    let id = match saved {
        Ok(Some(id)) => id,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return server_error(e),
    };
    if let Err(e) = audit::record(
        &pool,
        user.org_id,
        Some(user.id),
        audit::action::SEARCH_WIDENED,
        &format!("search:{id} role:{role_id} v{version} slot:{slot}"),
    )
    .await
    {
        return server_error(e);
    }
    state_response(&state, &pool, user.org_id, role_id).await
}

/// POST /api/roles/:id/searches/:slot/count: one credit per place it differs
/// from the brief. Counts only people not already found for the role.
pub async fn count(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((role_id, slot)): Path<(Uuid, i32)>,
    Json(req): Json<CountRequest>,
) -> Response {
    if !valid_key(&req.key) {
        return refuse(StatusCode::BAD_REQUEST, "Missing request key.");
    }
    let (pool, brief_id, _, lines) = match current(&state, &user, role_id).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match blocked(&state, &pool, user.org_id, role_id, true).await {
        Ok(Some(why)) => return refuse(StatusCode::CONFLICT, why),
        Ok(None) => {}
        Err(e) => return server_error(e),
    }
    let row: Result<Option<(Uuid, i32, SqlJson<Widen>)>, _> = sqlx::query_as(
        "SELECT id, version, widen FROM extra_search WHERE brief_id = $1 AND org_id = $2 AND slot = $3",
    )
    .bind(brief_id)
    .bind(user.org_id)
    .bind(slot as i16)
    .fetch_optional(&pool)
    .await;
    let (search_id, search_version, widen) = match row {
        Ok(Some(r)) => r,
        Ok(None) => return refuse(StatusCode::NOT_FOUND, "Set up this search first."),
        Err(e) => return server_error(e),
    };
    let (_, wide) = apply(&lines, &widen.0);
    if searches(&lines, &wide, &[]).is_empty() {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "This search finds the same people as the brief.",
        );
    }
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
        Some((search_id, search_version)),
        &req.key,
        |locked| searches(&lines, &wide, locked),
    )
    .await
}

/// POST /api/roles/:id/searches/pull: pull from one or more wider searches
/// in one press. Each is spread over its places and runs in the background.
pub async fn pull(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(role_id): Path<Uuid>,
    Json(req): Json<MorePullRequest>,
) -> Response {
    if !valid_key(&req.key) {
        return refuse(StatusCode::BAD_REQUEST, "Missing request key.");
    }
    let (pool, brief_id, _, _) = match current(&state, &user, role_id).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match blocked(&state, &pool, user.org_id, role_id, true).await {
        Ok(Some(why)) => return refuse(StatusCode::CONFLICT, why),
        Ok(None) => {}
        Err(e) => return server_error(e),
    }

    struct Chosen {
        slot: i32,
        search_id: Uuid,
        count_id: Uuid,
        widen: Widen,
        parts: Vec<(Counted, u32)>,
        size: u32,
    }
    let mut chosen: Vec<Chosen> = Vec::new();
    for p in req.picks.iter().filter(|p| p.size > 0) {
        if chosen.iter().any(|c| c.slot == p.slot) {
            return refuse(StatusCode::BAD_REQUEST, "Each search once.");
        }
        type Row = (
            Uuid,
            i32,
            String,
            SqlJson<Widen>,
            Option<i32>,
            Option<SqlJson<Vec<Counted>>>,
        );
        let row: Result<Option<Row>, _> = sqlx::query_as(
            "SELECT e.id, e.version, e.name, e.widen, c.search_version, c.locations
             FROM extra_search e JOIN search_count c ON c.search_id = e.id
             WHERE e.brief_id = $1 AND e.org_id = $2 AND e.slot = $3 AND c.id = $4",
        )
        .bind(brief_id)
        .bind(user.org_id)
        .bind(p.slot as i16)
        .bind(p.count_id)
        .fetch_optional(&pool)
        .await;
        let (search_id, version, name, widen, counted_version, locations) = match row {
            Ok(Some(r)) => r,
            Ok(None) => return refuse(StatusCode::NOT_FOUND, "That count is not of this search."),
            Err(e) => return server_error(e),
        };
        let Some(locations) = locations else {
            return refuse(
                StatusCode::CONFLICT,
                "That count did not finish. Count again.",
            );
        };
        if counted_version != Some(version) {
            return refuse(
                StatusCode::CONFLICT,
                format!("{name} changed since it was counted. Count it again first."),
            );
        }
        let totals: Vec<CountLocation> = locations
            .0
            .iter()
            .map(|c| CountLocation {
                label: c.label.clone(),
                total: c.total,
            })
            .collect();
        let parts = match split(p.size, &totals) {
            Ok(parts) => parts,
            Err(most) => {
                return refuse(
                    StatusCode::BAD_REQUEST,
                    format!("{name} has {most} to pull at most."),
                )
            }
        };
        let parts = parts
            .into_iter()
            .filter_map(|(label, n)| {
                locations
                    .0
                    .iter()
                    .find(|c| c.label == label)
                    .map(|c| (c.clone(), n))
            })
            .collect();
        chosen.push(Chosen {
            slot: p.slot,
            search_id,
            count_id: p.count_id,
            widen: widen.0,
            parts,
            size: p.size,
        });
    }
    let requested: u32 = chosen.iter().map(|c| c.size).sum();
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
        for c in &chosen {
            let key = format!("{}:{}", req.key, c.slot);
            let pull_id: Option<Uuid> = sqlx::query_scalar(
                "INSERT INTO pull (org_id, role_id, brief_id, count_id, created_by, requested,
                                   locations, idempotency_key, search_id)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 ON CONFLICT DO NOTHING RETURNING id",
            )
            .bind(user.org_id)
            .bind(role_id)
            .bind(brief_id)
            .bind(c.count_id)
            .bind(user.id)
            .bind(c.size as i32)
            .bind(c.parts.len() as i32)
            .bind(&key)
            .bind(c.search_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(pull_id) = pull_id else {
                // The same press again has already queued it; any other
                // pull of this count would pay for the same people again.
                let same_press: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pull WHERE org_id = $1 AND idempotency_key = $2)",
                )
                .bind(user.org_id)
                .bind(&key)
                .fetch_one(&mut *tx)
                .await?;
                if same_press {
                    continue;
                }
                return anyhow::Ok(false);
            };
            for (place, size) in &c.parts {
                let payload = serde_json::to_value(PullJob {
                    pull_id,
                    role_id,
                    brief_id,
                    actor_id: Some(user.id),
                    location: place.label.clone(),
                    query: place.query.clone(),
                    size: *size,
                    search_id: Some(c.search_id),
                    widen: Some(c.widen.clone()),
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
                &format!(
                    "pull:{pull_id} role:{role_id} requested:{} search:{}",
                    c.size, c.search_id
                ),
            )
            .await?;
        }
        tx.commit().await?;
        anyhow::Ok(true)
    }
    .await;
    match result {
        Ok(true) => state_response(&state, &pool, user.org_id, role_id).await,
        Ok(false) => refuse(
            StatusCode::CONFLICT,
            "That count has already been pulled. Count again to search for more.",
        ),
        Err(e) => server_error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BriefDomain, BriefTool};

    fn brief() -> BriefLines {
        BriefLines {
            titles: vec!["Cloud Security Engineer".into()],
            levels: vec!["Senior".into()],
            excluded_titles: vec!["Director".into()],
            min_years: Some(7),
            must_haves: vec!["AWS security".into()],
            domains: vec![BriefDomain {
                name: "Cloud security".into(),
                weight: DomainWeight::Must,
            }],
            tools: vec![
                BriefTool {
                    name: "AWS".into(),
                    status: Some(ToolStatus::Required),
                },
                BriefTool {
                    name: "Okta".into(),
                    status: Some(ToolStatus::Nice),
                },
            ],
            locations: vec!["Dubai".into(), "Abu Dhabi".into()],
            employer_types: vec!["Trading firms".into()],
            ..Default::default()
        }
    }

    #[test]
    fn a_widening_keeps_only_what_took_effect() {
        let w = Widen {
            add_titles: vec![
                "cloud security engineer".into(),
                "DevSecOps Engineer".into(),
            ],
            add_levels: vec!["Lead".into()],
            add_locations: vec!["Riyadh".into()],
            add_employer_types: vec!["Adtech".into(), "Tier-1 banks".into()],
            tools_to_nice: vec!["aws".into(), "Okta".into(), "Splunk".into()],
            domains_to_plus: vec!["cloud security".into()],
            min_years: Some(12),
        };
        let (took, wide) = apply(&brief(), &w);
        assert_eq!(
            took,
            Widen {
                add_titles: vec!["DevSecOps Engineer".into()],
                add_levels: vec!["Lead".into()],
                add_locations: vec!["Riyadh".into()],
                add_employer_types: vec!["Adtech".into()],
                tools_to_nice: vec!["AWS".into()],
                domains_to_plus: vec!["Cloud security".into()],
                min_years: None,
            },
            "repeats, unknown names, nice tools and more years drop out"
        );
        assert_eq!(wide.excluded_titles, ["Director"], "never narrows");
        assert_eq!(apply(&brief(), &took).0, took, "applying again is stable");
        let any = Widen {
            min_years: Some(0),
            ..Default::default()
        };
        let (took, wide) = apply(&brief(), &any);
        assert_eq!((took.min_years, wide.min_years), (Some(0), None));
        assert!(is_empty(&apply(&brief(), &Widen::default()).0));
    }

    #[test]
    fn only_places_that_differ_are_searched() {
        let b = brief();
        let places = Widen {
            add_locations: vec!["Riyadh".into(), "Doha".into()],
            ..Default::default()
        };
        let (_, wide) = apply(&b, &places);
        let labels: Vec<String> = searches(&b, &wide, &[])
            .into_iter()
            .map(|s| s.label)
            .collect();
        assert_eq!(
            labels,
            ["Riyadh", "Doha"],
            "Dubai and Abu Dhabi are the brief's"
        );
        let titles = Widen {
            add_titles: vec!["Security Architect".into()],
            ..Default::default()
        };
        let (_, wide) = apply(&b, &titles);
        let labels: Vec<String> = searches(&b, &wide, &[])
            .into_iter()
            .map(|s| s.label)
            .collect();
        assert_eq!(labels, ["Dubai", "Abu Dhabi"]);
        assert!(searches(&b, &b, &[]).is_empty());
    }

    #[test]
    fn a_pull_is_spread_over_the_places() {
        let p = |label: &str, total| CountLocation {
            label: label.into(),
            total,
        };
        let places = [p("Dubai", 30), p("Abu Dhabi", 5), p("Riyadh", 0)];
        assert_eq!(
            split(25, &places).unwrap(),
            [("Dubai".to_string(), 22), ("Abu Dhabi".to_string(), 3)]
        );
        assert_eq!(
            split(35, &places).unwrap(),
            [("Dubai".to_string(), 30), ("Abu Dhabi".to_string(), 5)]
        );
        assert_eq!(split(36, &places), Err(35));
        assert_eq!(
            split(150, &[p("London", 900)]),
            Err(100),
            "one page at most"
        );
        let even = split(3, &[p("A", 10), p("B", 10)]).unwrap();
        assert_eq!(even.iter().map(|x| x.1).sum::<u32>(), 3);
    }

    #[test]
    fn claudes_reply_is_read_leniently() {
        let r: Suggestions = serde_json::from_value(json!({"searches": [
            {"name": " Wider  titles ", "note": "Adds close titles.", "add_titles": ["DevSecOps Engineer"],
             "add_levels": [], "add_locations": [], "add_employer_types": [], "tools_to_nice": [],
             "domains_to_plus": [], "min_years": null},
            {"name": "Places", "add_locations": ["Riyadh"], "min_years": "5"},
            {"name": "Any years", "min_years": 0}
        ]}))
        .unwrap();
        assert_eq!(
            r.searches[0].widen().min_years,
            None,
            "null keeps the years"
        );
        assert_eq!(r.searches[1].widen().min_years, Some(5));
        assert_eq!(r.searches[2].widen().min_years, Some(0), "0 means any");
        assert_eq!(trimmed(&r.searches[0].name, MAX_NAME_CHARS), "Wider titles");
        let body = request("m", &brief(), &[]);
        assert_eq!(body["tools"][0]["name"], "record_searches");
    }
}
