//! Round 2: when a count finds too few people, Claude reads the brief and the
//! counts, says why, and proposes a relaxed brief. Nothing is saved or
//! searched here; the resourcer agrees first, and the normal confirm and count
//! steps then run.
//!
//! Claude may only relax. Its reply is a list of relaxing moves (a required
//! tool becomes nice to have, a Must domain becomes Plus, titles, levels,
//! employer types or locations are added, fewer years, remote allowed), applied
//! here in code, so a reply can never narrow the search or touch the
//! resourcer's own choices (excluded titles, leave-out list, must-haves).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::types::Json as SqlJson;
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    ai::AiError,
    app::AppState,
    audit,
    auth::CurrentUser,
    domain::{BriefLines, CountLocation, DomainWeight, ToolStatus},
    plan,
    searching::{confirmed_brief, role_exists},
};

/// Claude's reading of a count that found too few people, and the relaxed
/// brief it proposes. Nothing is saved or searched until the resourcer agrees.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct RetuneView {
    /// The confirmed brief version this relaxes; confirming checks it.
    pub based_on: i32,
    /// Why the search found so few, in plain words.
    pub diagnosis: String,
    /// What changes, line by line.
    pub changes: Vec<RetuneChange>,
    /// The whole relaxed brief, ready to confirm.
    pub lines: BriefLines,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct RetuneChange {
    pub line: String,
    pub before: String,
    pub after: String,
    /// Claude's reason, or empty.
    pub reason: String,
}

/// The employer types Sourcer knows how to search.
pub const EMPLOYER_TYPES: &[&str] = &[
    "Payments and neobanks",
    "Crypto and digital assets",
    "Trading firms",
    "E-commerce",
    "Adtech",
];
const MAX_DIAGNOSIS_CHARS: usize = 800;
const MAX_REASON_CHARS: usize = 200;
const MAX_ITEM_CHARS: usize = 120;

const INSTRUCTIONS: &str = "You help a recruiter whose candidate search found too few people. \
Answer only by calling the record_round_two tool, once, with no other text.

How the search works (People Data Labs): a person is found only if ALL of these hold. Their \
current job title contains one of the titles AND one of the levels, and contains none of the \
excluded titles. Each required tool must appear in their headline, summary, job summary or \
skills list; many profiles have no summary, so every required tool removes many people, and a \
niche product removes almost everyone. At least one Must domain must appear in the same places. \
Their employer's industry must fit one of the employer types. They must have at least the \
fewest years. Each location is a separate search. Everything else (must-haves, capabilities, \
nice-to-have tools, Plus domains, standards) only ranks people and never removes anyone.

diagnosis: two or three plain sentences for the recruiter: which lines most likely emptied the \
search and why. Be specific; name the lines.

Then choose the smallest set of relaxing moves likely to give a workable pool (roughly 25 to 300 \
people per location). Prefer, in this order: move niche or vendor-specific required tools to \
nice to have (a platform almost everyone in the role lists, such as AWS for a cloud role, may \
stay); move Must domains to Plus; add employer types; lower or drop the fewest years; add close \
job titles or levels. Add a location only if the market is clearly too small. Everything you \
relax still counts in the ranking, so nothing is lost. Never invent requirements.

tools_to_nice: names of required tools to make nice to have, exactly as given. domains_to_plus: \
names of Must domains to make Plus, exactly as given. add_titles and add_levels: new ones only. \
min_years: the new fewest years, lower than now, or null for none; omit or repeat the current \
value to keep it. add_employer_types: only from the allowed list. add_locations: city names. \
allow_remote: true only to allow remote. reasons: one per line you change, one short sentence.";

/// Claude's reply: a diagnosis and relaxing moves only.
#[derive(Debug, Default, Deserialize)]
pub struct Reply {
    #[serde(default)]
    pub diagnosis: String,
    #[serde(default)]
    pub tools_to_nice: Vec<String>,
    #[serde(default)]
    pub domains_to_plus: Vec<String>,
    #[serde(default)]
    pub add_titles: Vec<String>,
    #[serde(default)]
    pub add_levels: Vec<String>,
    /// Read leniently; absent means keep.
    #[serde(default)]
    pub min_years: Option<Value>,
    #[serde(default)]
    pub add_employer_types: Vec<String>,
    #[serde(default)]
    pub add_locations: Vec<String>,
    #[serde(default)]
    pub allow_remote: bool,
    #[serde(default)]
    pub reasons: Vec<Reason>,
}

#[derive(Debug, Deserialize)]
pub struct Reason {
    pub line: String,
    #[serde(default)]
    pub reason: String,
}

/// The lines Claude may change, as named in its reasons.
const LINES: [&str; 8] = [
    "titles",
    "levels",
    "min_years",
    "tools",
    "domains",
    "employer_types",
    "locations",
    "remote",
];

/// The search as it ran, in plain words for Claude.
fn situation(b: &BriefLines, counts: &[CountLocation]) -> Value {
    let tools = |st: ToolStatus| -> Vec<&str> {
        b.tools
            .iter()
            .filter(|t| t.status == Some(st))
            .map(|t| t.name.as_str())
            .collect()
    };
    let domains = |w: DomainWeight| -> Vec<&str> {
        b.domains
            .iter()
            .filter(|d| d.weight == w)
            .map(|d| d.name.as_str())
            .collect()
    };
    json!({
        "role_summary": b.analysis,
        "narrows": {
            "titles": b.titles,
            "levels": b.levels,
            "excluded_titles": b.excluded_titles,
            "min_years": b.min_years,
            "required_tools": tools(ToolStatus::Required),
            "must_domains": domains(DomainWeight::Must),
            "employer_types": b.employer_types,
            "locations": b.locations,
            "remote": b.remote,
        },
        "only_ranks": {
            "must_haves": b.must_haves,
            "nice_to_have_tools": tools(ToolStatus::Nice),
            "plus_domains": domains(DomainWeight::Plus),
        },
        "allowed_employer_types": EMPLOYER_TYPES,
        "counts": counts.iter().map(|c| json!({"location": c.label, "people": c.total})).collect::<Vec<_>>(),
    })
}

/// The request body for Claude.
pub fn request(model: &str, b: &BriefLines, counts: &[CountLocation]) -> Value {
    let list = json!({"type": "array", "items": {"type": "string"}});
    json!({
        "model": model,
        "max_tokens": 2048,
        "system": INSTRUCTIONS,
        "tools": [{
            "name": "record_round_two",
            "description": "Record why the search found too few people and the relaxing moves to try.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "diagnosis": {"type": "string"},
                    "tools_to_nice": list, "domains_to_plus": list,
                    "add_titles": list, "add_levels": list,
                    "min_years": {"type": ["integer", "null"]},
                    "add_employer_types": {"type": "array", "items": {"type": "string", "enum": EMPLOYER_TYPES}},
                    "add_locations": list,
                    "allow_remote": {"type": "boolean"},
                    "reasons": {"type": "array", "items": {
                        "type": "object",
                        "properties": {
                            "line": {"type": "string", "enum": LINES},
                            "reason": {"type": "string"}
                        },
                        "required": ["line", "reason"]
                    }}
                },
                "required": ["diagnosis", "tools_to_nice", "domains_to_plus", "add_titles",
                             "add_levels", "add_employer_types", "add_locations",
                             "allow_remote", "reasons"]
            }
        }],
        // Newer models refuse a forced tool; a reply without it is an error.
        "tool_choice": {"type": "auto"},
        "messages": [{"role": "user", "content": format!(
            "<search>\n{}\n</search>",
            serde_json::to_string_pretty(&situation(b, counts)).unwrap_or_default()
        )}]
    })
}

fn clean(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.chars().take(MAX_ITEM_CHARS).collect())
}

/// Add new entries after the old ones, without repeats, up to `max`.
fn widen(old: &[String], new: &[String], max: usize) -> Vec<String> {
    let mut out = old.to_vec();
    for s in new.iter().filter_map(|s| clean(s)) {
        if out.len() >= max {
            break;
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&s)) {
            out.push(s);
        }
    }
    out
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Apply Claude's relaxing moves to the brief. Only ever relaxes.
pub fn relax(old: &BriefLines, r: &Reply) -> BriefLines {
    let mut b = old.clone();
    for t in &mut b.tools {
        if t.status == Some(ToolStatus::Required)
            && r.tools_to_nice.iter().any(|n| same(n, &t.name))
        {
            t.status = Some(ToolStatus::Nice);
        }
    }
    for d in &mut b.domains {
        if d.weight == DomainWeight::Must && r.domains_to_plus.iter().any(|n| same(n, &d.name)) {
            d.weight = DomainWeight::Plus;
        }
    }
    b.titles = widen(&old.titles, &r.add_titles, plan::MAX_TITLES);
    b.levels = widen(&old.levels, &r.add_levels, plan::MAX_LEVELS);
    if let (Some(now), Some(v)) = (old.min_years, &r.min_years) {
        let proposed = match v {
            Value::Null => Some(None),
            Value::Number(n) => n.as_f64().map(|y| Some(y.round() as i32)),
            Value::String(t) => t.trim().parse::<f64>().ok().map(|y| Some(y.round() as i32)),
            _ => None,
        };
        match proposed {
            Some(None) => b.min_years = None,
            Some(Some(y)) if y <= 0 => b.min_years = None,
            Some(Some(y)) if y < now => b.min_years = Some(y),
            _ => {}
        }
    }
    let allowed: Vec<String> = r
        .add_employer_types
        .iter()
        .filter_map(|t| EMPLOYER_TYPES.iter().find(|k| same(k, t)))
        .map(|k| k.to_string())
        .collect();
    b.employer_types = widen(
        &old.employer_types,
        &allowed,
        EMPLOYER_TYPES.len().max(old.employer_types.len()),
    );
    // A remote-only brief has no locations; adding cities would narrow it.
    if !old.locations.is_empty() {
        b.locations = widen(&old.locations, &r.add_locations, plan::MAX_LOCATIONS);
    }
    b.remote = old.remote || r.allow_remote;
    b
}

fn join(v: &[String]) -> String {
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}

/// What changed between the two briefs, line by line, with Claude's reason.
pub fn describe(old: &BriefLines, new: &BriefLines, reasons: &[Reason]) -> Vec<RetuneChange> {
    let why = |line: &str| -> String {
        reasons
            .iter()
            .find(|r| r.line == line)
            .map(|r| r.reason.trim().chars().take(MAX_REASON_CHARS).collect())
            .unwrap_or_default()
    };
    let required = |b: &BriefLines| -> Vec<String> {
        b.tools
            .iter()
            .filter(|t| t.status == Some(ToolStatus::Required))
            .map(|t| t.name.clone())
            .collect()
    };
    let must = |b: &BriefLines| -> Vec<String> {
        b.domains
            .iter()
            .filter(|d| d.weight == DomainWeight::Must)
            .map(|d| d.name.clone())
            .collect()
    };
    let years = |y: Option<i32>| {
        y.filter(|y| *y > 0)
            .map_or("any".to_string(), |y| format!("{y}+"))
    };
    let rows: [(&str, &str, String, String); 8] = [
        (
            "tools",
            "Required tools",
            join(&required(old)),
            join(&required(new)),
        ),
        (
            "domains",
            "Must domains",
            join(&must(old)),
            join(&must(new)),
        ),
        (
            "employer_types",
            "Employers",
            join(&old.employer_types),
            join(&new.employer_types),
        ),
        (
            "min_years",
            "Years",
            years(old.min_years),
            years(new.min_years),
        ),
        ("titles", "Job titles", join(&old.titles), join(&new.titles)),
        ("levels", "Levels", join(&old.levels), join(&new.levels)),
        (
            "locations",
            "Locations",
            join(&old.locations),
            join(&new.locations),
        ),
        (
            "remote",
            "Remote",
            if old.remote { "allowed" } else { "no" }.into(),
            if new.remote { "allowed" } else { "no" }.into(),
        ),
    ];
    rows.into_iter()
        .filter(|(_, _, before, after)| before != after)
        .map(|(key, label, before, after)| RetuneChange {
            line: label.into(),
            before,
            after,
            reason: why(key),
        })
        .collect()
}

fn refuse(code: StatusCode, msg: &str) -> Response {
    (code, msg.to_string()).into_response()
}

fn server_error(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "round 2 request failed");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

/// POST /api/roles/:id/search/retune: Claude reads the confirmed brief and
/// its latest count, says why so few were found, and proposes a relaxed brief.
/// Nothing is saved and nothing is searched: the resourcer agrees first, then
/// the usual confirm and count run.
pub async fn retune(
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
    let Some((brief_id, version, lines)) = (match confirmed_brief(&pool, role_id).await {
        Ok(b) => b,
        Err(e) => return server_error(e),
    }) else {
        return refuse(StatusCode::CONFLICT, "Confirm the brief first.");
    };
    // Confirming checks the version it was based on, so an open draft would
    // be overwritten; the resourcer settles it first.
    let draft_open: Result<bool, _> = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM brief WHERE role_id = $1 AND confirmed_at IS NULL)",
    )
    .bind(role_id)
    .fetch_one(&pool)
    .await;
    match draft_open {
        Ok(false) => {}
        Ok(true) => {
            return refuse(
                StatusCode::CONFLICT,
                "The brief has edits that are not confirmed. Confirm them first, then ask again.",
            )
        }
        Err(e) => return server_error(e),
    }
    // The latest finished count of this exact brief.
    type CountRow = (Uuid, SqlJson<Vec<CountLocation>>);
    let counted: Result<Option<CountRow>, _> = sqlx::query_as(
        "SELECT id, locations FROM search_count
         WHERE role_id = $1 AND org_id = $2 AND brief_id = $3 AND locations IS NOT NULL
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(role_id)
    .bind(user.org_id)
    .bind(brief_id)
    .fetch_optional(&pool)
    .await;
    let (count_id, counts) = match counted {
        Ok(Some((id, c))) => (id, c.0),
        Ok(None) => return refuse(StatusCode::CONFLICT, "Count the matches first."),
        Err(e) => return server_error(e),
    };
    if !state.draft_limit.allow(user.id) {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests to Claude in a short time. Wait a few minutes, then try again.",
        );
    }
    let reply = match state.ai.retune(&lines, &counts).await {
        Ok(r) => r,
        Err(AiError::NotConfigured) => {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "Claude is not set up yet (no key). Edit the brief yourself for now.",
            )
        }
        Err(e) => {
            tracing::warn!(error = %e, "round 2 failed");
            return refuse(
                StatusCode::BAD_GATEWAY,
                "Claude could not look at this search just now. Try again, or edit the brief yourself.",
            );
        }
    };
    let relaxed = relax(&lines, &reply);
    let changes = describe(&lines, &relaxed, &reply.reasons);
    if changes.is_empty() {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Claude found nothing it could safely relax. Edit the brief yourself.",
        );
    }
    let issues = crate::roles::problems(&relaxed);
    if !issues.is_empty() {
        tracing::warn!(?issues, "round 2 gave a brief that cannot be confirmed");
        return refuse(
            StatusCode::BAD_GATEWAY,
            "Claude's suggestion could not be used. Edit the brief yourself.",
        );
    }
    if let Err(e) = audit::record(
        &pool,
        user.org_id,
        Some(user.id),
        audit::action::SEARCH_RETUNED,
        &format!("role:{role_id} v{version} count:{count_id}"),
    )
    .await
    {
        return server_error(e);
    }
    Json(RetuneView {
        based_on: version,
        diagnosis: diagnosis(&reply),
        changes,
        lines: relaxed,
    })
    .into_response()
}

/// The diagnosis, trimmed.
pub fn diagnosis(r: &Reply) -> String {
    r.diagnosis
        .trim()
        .chars()
        .take(MAX_DIAGNOSIS_CHARS)
        .collect()
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
            domains: vec![
                BriefDomain {
                    name: "Cloud security".into(),
                    weight: DomainWeight::Must,
                },
                BriefDomain {
                    name: "Custody".into(),
                    weight: DomainWeight::Plus,
                },
            ],
            tools: vec![
                BriefTool {
                    name: "AWS".into(),
                    status: Some(ToolStatus::Required),
                },
                BriefTool {
                    name: "Deep Instinct".into(),
                    status: Some(ToolStatus::Required),
                },
                BriefTool {
                    name: "CyberArk".into(),
                    status: Some(ToolStatus::Replacing),
                },
            ],
            locations: vec!["Dubai".into()],
            employer_types: vec!["Trading firms".into()],
            leave_out: vec!["Some Bank".into()],
            ..Default::default()
        }
    }

    fn reply() -> Reply {
        Reply {
            diagnosis: " Deep Instinct emptied it. ".into(),
            tools_to_nice: vec!["deep instinct".into(), "CyberArk".into(), "Okta".into()],
            domains_to_plus: vec!["Cloud security".into(), "Custody".into()],
            add_titles: vec![
                "DevSecOps Engineer".into(),
                "cloud security engineer".into(),
            ],
            add_levels: vec!["Lead".into()],
            min_years: Some(json!(5)),
            add_employer_types: vec!["Adtech".into(), "Tier-1 banks".into()],
            add_locations: vec!["Abu Dhabi".into()],
            allow_remote: false,
            reasons: vec![Reason {
                line: "tools".into(),
                reason: "Few profiles name Deep Instinct.".into(),
            }],
        }
    }

    #[test]
    fn relaxing_moves_are_applied() {
        let b = relax(&brief(), &reply());
        assert_eq!(b.tools[0].status, Some(ToolStatus::Required), "AWS stays");
        assert_eq!(b.tools[1].status, Some(ToolStatus::Nice));
        assert_eq!(
            b.tools[2].status,
            Some(ToolStatus::Replacing),
            "never turned into a nice to have"
        );
        assert_eq!(b.domains[0].weight, DomainWeight::Plus);
        assert_eq!(b.titles, ["Cloud Security Engineer", "DevSecOps Engineer"]);
        assert_eq!(b.levels, ["Senior", "Lead"]);
        assert_eq!(b.min_years, Some(5));
        assert_eq!(
            b.employer_types,
            ["Trading firms", "Adtech"],
            "unknown types ignored"
        );
        assert_eq!(b.locations, ["Dubai", "Abu Dhabi"]);
        assert_eq!(
            b.excluded_titles,
            ["Director"],
            "the resourcer's own choices stay"
        );
        assert_eq!(b.leave_out, ["Some Bank"]);
        assert_eq!(b.must_haves, ["AWS security"]);
    }

    #[test]
    fn a_reply_can_never_narrow() {
        let mut r = Reply {
            min_years: Some(json!(12)),
            ..Default::default()
        };
        let b = relax(&brief(), &r);
        assert_eq!(b, brief(), "more years is ignored and nothing else moves");
        let mut open = brief();
        open.min_years = None;
        r.min_years = Some(json!(3));
        assert_eq!(relax(&open, &r).min_years, None, "no limit stays no limit");
        r.min_years = Some(Value::Null);
        assert_eq!(relax(&brief(), &r).min_years, None, "null drops the limit");
        let mut remote = brief();
        remote.locations.clear();
        remote.remote = true;
        r.add_locations = vec!["London".into()];
        assert!(
            relax(&remote, &r).locations.is_empty(),
            "remote-only stays everywhere"
        );
    }

    #[test]
    fn additions_respect_the_search_caps() {
        let r = Reply {
            add_titles: (0..20).map(|i| format!("Title {i}")).collect(),
            add_levels: (0..20).map(|i| format!("Level {i}")).collect(),
            ..Default::default()
        };
        let b = relax(&brief(), &r);
        assert_eq!(b.titles.len(), plan::MAX_TITLES);
        assert_eq!(b.levels.len(), plan::MAX_LEVELS);
        assert!(
            crate::roles::problems(&b).is_empty(),
            "{:?}",
            crate::roles::problems(&b)
        );
    }

    #[test]
    fn changes_are_described_with_reasons() {
        let old = brief();
        let new = relax(&old, &reply());
        let c = describe(&old, &new, &reply().reasons);
        let lines: Vec<&str> = c.iter().map(|c| c.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "Required tools",
                "Must domains",
                "Employers",
                "Years",
                "Job titles",
                "Levels",
                "Locations"
            ]
        );
        assert_eq!(c[0].before, "AWS, Deep Instinct");
        assert_eq!(c[0].after, "AWS");
        assert_eq!(c[0].reason, "Few profiles name Deep Instinct.");
        assert_eq!(c[1].after, "none");
        assert_eq!(c[3].before, "7+");
        assert!(describe(&old, &old, &[]).is_empty());
        assert_eq!(diagnosis(&reply()), "Deep Instinct emptied it.");
    }

    #[test]
    fn the_request_names_the_counts_and_the_allowed_moves() {
        let counts = [CountLocation {
            label: "Dubai".into(),
            total: 0,
        }];
        let body = request("m", &brief(), &counts);
        assert_eq!(body["tools"][0]["name"], "record_round_two");
        assert_eq!(body["tool_choice"]["type"], "auto");
        let msg = body["messages"][0]["content"].as_str().unwrap();
        assert!(
            msg.contains("\"people\": 0") && msg.contains("Deep Instinct"),
            "{msg}"
        );
        assert!(!msg.contains("Some Bank"), "leave-out names are not needed");
    }
}
