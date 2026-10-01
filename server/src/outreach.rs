//! Email outreach, part one (SRS F12, F13): drafts and approval. Nothing is
//! sent from here; sending comes with the Outlook connection (F14).
//!
//! Kai's rules (1 Oct 2026):
//! - Email only to a personal email address. No personal email, no email.
//! - Three emails: the first, a follow-up 3 days later and a final one 4 days
//!   after that. One approval covers all three.
//! - In Kai's words, short, never naming the client: the role basics and an
//!   ask to reply if suitable and available. His signature on every email; the
//!   line saying where the address came from on the first only.
//!
//! Drafts come from his template, not from AI, so they always sound like him
//! and cost nothing. He can edit any of it before approving.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    app::AppState,
    audit,
    auth::CurrentUser,
    candidates::one_row,
    domain::{BriefLines, CandidacyState, Channel},
    employer::{self, normalise_name, Company, Verdict},
    policy::{self, Blocked, SendCheck},
    searching::confirmed_brief,
};

/// Days after the previous email: the first, then 3, then 4 (Kai).
pub const DELAYS: [i32; 3] = [0, 3, 4];
/// Kai's usual length for a first email, in words.
const MAX_WORDS: usize = 150;
const MAX_SUBJECT: usize = 200;
const MAX_BODY: usize = 5_000;
const MAX_SIGNATURE: usize = 2_000;
const MAX_INTRO: usize = 200;
/// Shown small and grey under the first email (UK GDPR Art. 14: say where
/// the data came from at first contact).
pub const SOURCE_LINE: &str =
    "I got your email from a professional data provider. Reply \"no\" and I won't contact you again.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, sqlx::Type)]
#[sqlx(type_name = "outreach_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../web/src/api/types/")]
pub enum OutreachStatus {
    Draft,
    Approved,
    /// Sending has started.
    Active,
    Stopped,
    Done,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct OutreachStepView {
    pub step: i32,
    /// Days after the previous email.
    pub delay_days: i32,
    pub subject: String,
    pub body: String,
    /// How it will look, signature and source line included.
    pub html: String,
    pub sent_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct OutreachView {
    /// `None` until drafted.
    pub status: Option<OutreachStatus>,
    /// The personal email it goes to.
    pub to_email: Option<String>,
    pub sender: String,
    pub steps: Vec<OutreachStepView>,
    /// What stops approval, in words.
    pub problems: Vec<String>,
    /// Worth a look, but does not stop approval.
    pub notes: Vec<String>,
    pub stop_reason: Option<String>,
    /// e.g. "1 Oct 2026".
    pub approved_at: Option<String>,
    /// Sent back with changes, so two windows cannot overwrite each other.
    pub version: i32,
    /// The Outlook connection is set up, so approved emails will go out.
    pub sending_ready: bool,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct OutreachStepEdit {
    pub step: i32,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct OutreachEdit {
    pub version: i32,
    pub steps: Vec<OutreachStepEdit>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct OutreachAction {
    pub version: i32,
}

/// How a team member signs and introduces themselves.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct OutreachSettings {
    /// Plain text. Links as [LinkedIn](https://...) or a bare https:// address.
    pub signature: String,
    /// e.g. "I'm a Director at Austin Werner, a recruitment business".
    pub intro: String,
}

// ---------- Drafting (pure) ----------

pub struct StepDraft {
    pub subject: String,
    pub body: String,
}

pub fn first_name(full: &str) -> String {
    full.split_whitespace()
        .next()
        .unwrap_or("there")
        .to_string()
}

/// "New York or Dubai" from the brief's locations, first part of each only.
pub fn place(locations: &[String]) -> String {
    let heads: Vec<&str> = locations
        .iter()
        .map(|l| l.split(',').next().unwrap_or(l).trim())
        .filter(|l| !l.is_empty())
        .take(2)
        .collect();
    heads.join(" or ")
}

/// Up to three things the client is looking for: the must-haves, then
/// capabilities.
pub fn bullets(lines: &BriefLines) -> Vec<String> {
    lines
        .must_haves
        .iter()
        .chain(lines.capabilities.iter())
        .map(|b| b.trim().trim_end_matches('.').trim())
        .filter(|b| !b.is_empty())
        .map(|b| {
            let mut c = b.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .take(3)
        .collect()
}

/// The three emails in Kai's words.
pub fn draft_steps(
    first: &str,
    title: &str,
    place: &str,
    bullets: &[String],
    intro: &str,
) -> [StepDraft; 3] {
    let role = if place.is_empty() {
        format!("{title} role")
    } else {
        format!("{title} role in {place}")
    };
    let subject = if place.is_empty() {
        format!("{title} role")
    } else {
        format!("{title} role, {place}")
    };
    let mut first_body = format!(
        "Hey {first},\n\nHope you're well. Just found your details and thought you might be well aligned to a role I'm working on at the moment.\n\n{intro}, and we have a {role}."
    );
    if bullets.is_empty() {
        first_body.push_str("\n\n");
    } else {
        first_body.push_str(" A brief summary of what they're looking for:\n\n");
        for b in bullets {
            first_body.push_str(&format!("- {b}\n"));
        }
        first_body.push('\n');
    }
    first_body.push_str("Lmk if you're interested, and if you're open to a move right now.");
    [
        StepDraft {
            subject: subject.clone(),
            body: first_body,
        },
        StepDraft {
            subject: format!("Re: {subject}"),
            body: format!(
                "Hey {first}, just bumping this in case it got buried. Still keen to hear if the {role} could suit you. Lmk either way."
            ),
        },
        StepDraft {
            subject: format!("Re: {subject}"),
            body: format!(
                "Hey {first}, last note from me on this one. If the timing's wrong, no problem. Happy to keep you in mind for future roles."
            ),
        },
    ]
}

pub fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// The client's names or web address found in the text, so an email never
/// names who the role is for.
pub fn client_mentions(text: &str, companies: &[Company]) -> Vec<String> {
    let plain = format!(" {} ", normalise_name(text));
    let squashed: String = text
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    let mut found = Vec::new();
    for c in companies {
        let name = normalise_name(&c.name);
        let name_hit = name.chars().count() >= 3 && plain.contains(&format!(" {name} "));
        let stem = c
            .domain
            .as_deref()
            .and_then(employer::normalise_domain)
            .and_then(|d| d.split('.').next().map(str::to_string))
            .filter(|s| s.chars().count() >= 4);
        let domain_hit = stem.is_some_and(|s| squashed.contains(&s));
        if (name_hit || domain_hit) && !found.contains(&c.name) {
            found.push(c.name.clone());
        }
    }
    found
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// One line of the signature, with [text](https://...) and bare https://
/// addresses as links. Only http and https links are made.
fn linkify(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while !rest.is_empty() {
        if let Some(open) = rest.find('[') {
            if let Some((text, url, after)) = markdown_link(&rest[open..]) {
                out.push_str(&bare_links(&rest[..open]));
                out.push_str(&format!("<a href=\"{}\">{}</a>", escape(url), escape(text)));
                rest = after;
                continue;
            }
            out.push_str(&bare_links(&rest[..=open]));
            rest = &rest[open + 1..];
            continue;
        }
        out.push_str(&bare_links(rest));
        break;
    }
    out
}

fn markdown_link(s: &str) -> Option<(&str, &str, &str)> {
    let close = s.find("](")?;
    let text = &s[1..close];
    let tail = &s[close + 2..];
    let end = tail.find(')')?;
    let url = &tail[..end];
    let ok = (url.starts_with("https://") || url.starts_with("http://"))
        && !url.contains(char::is_whitespace)
        && !text.contains('[');
    ok.then_some((text, url, &tail[end + 1..]))
}

fn bare_links(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            if w.starts_with("https://") || w.starts_with("http://") {
                format!("<a href=\"{0}\">{0}</a>", escape(w))
            } else {
                escape(w)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The email as it will be sent: the body in paragraphs (lines starting with
/// "- " as a list), the signature, and on the first email the source line.
pub fn render_html(body: &str, signature: &str, source_line: bool) -> String {
    let mut out = String::from(
        "<div style=\"font-family:Calibri,Arial,sans-serif;font-size:15px;color:#222\">",
    );
    for para in body.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        let lines: Vec<&str> = para.lines().map(str::trim).collect();
        if lines.iter().all(|l| l.starts_with("- ")) {
            out.push_str("<ul>");
            for l in lines {
                out.push_str(&format!("<li>{}</li>", escape(&l[2..])));
            }
            out.push_str("</ul>");
        } else {
            let html: Vec<String> = lines.iter().map(|l| escape(l)).collect();
            out.push_str(&format!("<p>{}</p>", html.join("<br>")));
        }
    }
    let sig: Vec<String> = signature.trim_end().lines().map(linkify).collect();
    if !sig.is_empty() && !sig.iter().all(|l| l.is_empty()) {
        out.push_str(&format!("<p>{}</p>", sig.join("<br>")));
    }
    if source_line {
        out.push_str(&format!(
            "<p style=\"font-size:11px;color:#888;margin-top:20px\">{}</p>",
            escape(SOURCE_LINE)
        ));
    }
    out.push_str("</div>");
    out
}

// ---------- Loading ----------

fn refuse(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, msg.into()).into_response()
}

fn server_error(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "outreach request failed");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

#[derive(sqlx::FromRow)]
struct Context {
    role_id: Uuid,
    person_id: Uuid,
    state: CandidacyState,
    full_name: String,
    title: String,
    org_name: String,
    intro: String,
    sender: String,
    paused: bool,
}

async fn context(
    pool: &PgPool,
    org_id: Uuid,
    user_id: Uuid,
    candidacy: Uuid,
) -> anyhow::Result<Option<Context>> {
    Ok(sqlx::query_as(
        "SELECT c.role_id, c.person_id, c.state, p.full_name, r.title, o.name AS org_name,
                u.intro, u.name AS sender, o.sending_paused AS paused
         FROM candidacy c
         JOIN person p ON p.id = c.person_id
         JOIN role r ON r.id = c.role_id
         JOIN org o ON o.id = c.org_id
         JOIN app_user u ON u.id = $3 AND u.org_id = c.org_id
         WHERE c.id = $1 AND c.org_id = $2",
    )
    .bind(candidacy)
    .bind(org_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?)
}

/// The first personal email on record that is not on the do-not-contact list.
async fn personal_email(
    pool: &PgPool,
    org_id: Uuid,
    person_id: Uuid,
) -> anyhow::Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT k.value FROM contact k
         WHERE k.person_id = $1 AND k.org_id = $2 AND k.kind = 'personal_email'
           AND NOT EXISTS (SELECT 1 FROM do_not_contact d
                           WHERE d.org_id = $2 AND d.identifier = lower(k.value))
         ORDER BY k.created_at, k.value LIMIT 1",
    )
    .bind(person_id)
    .bind(org_id)
    .fetch_optional(pool)
    .await?)
}

type OutreachRow = (
    Uuid,
    OutreachStatus,
    String,
    Option<String>,
    Option<chrono::DateTime<chrono::Utc>>,
    i32,
    String,
);
type StepRow = (
    i32,
    i32,
    String,
    String,
    Option<chrono::DateTime<chrono::Utc>>,
);

async fn view(
    pool: &PgPool,
    org_id: Uuid,
    user_id: Uuid,
    candidacy: Uuid,
) -> anyhow::Result<Option<OutreachView>> {
    let Some(ctx) = context(pool, org_id, user_id, candidacy).await? else {
        return Ok(None);
    };
    let row: Option<OutreachRow> = sqlx::query_as(
        "SELECT o.id, o.status, o.to_email, o.stop_reason, o.approved_at, o.version, u.name
         FROM outreach o JOIN app_user u ON u.id = o.sender_id
         WHERE o.candidacy_id = $1 AND o.org_id = $2",
    )
    .bind(candidacy)
    .bind(org_id)
    .fetch_optional(pool)
    .await?;
    let to_now = personal_email(pool, org_id, ctx.person_id).await?;
    let Some((id, status, to_email, stop_reason, approved_at, version, sender)) = row else {
        let mut problems = Vec::new();
        if to_now.is_none() {
            problems.push("No personal email on record, so no email can be sent.".into());
        }
        return Ok(Some(OutreachView {
            status: None,
            to_email: to_now,
            sender: ctx.sender,
            steps: Vec::new(),
            problems,
            notes: Vec::new(),
            stop_reason: None,
            approved_at: None,
            version: 0,
            sending_ready: false,
        }));
    };
    let steps: Vec<StepRow> = sqlx::query_as(
        "SELECT step, delay_days, subject, body, sent_at FROM outreach_step
         WHERE outreach_id = $1 ORDER BY step",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    // The sender's signature, not the viewer's.
    let signature: String = sqlx::query_scalar(
        "SELECT u.signature FROM outreach o JOIN app_user u ON u.id = o.sender_id WHERE o.id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    let (problems, notes) = if status == OutreachStatus::Draft {
        checks(pool, org_id, candidacy, &ctx, &to_email, &signature, &steps).await?
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(Some(OutreachView {
        status: Some(status),
        to_email: Some(to_email),
        sender,
        steps: steps
            .into_iter()
            .map(
                |(step, delay_days, subject, body, sent_at)| OutreachStepView {
                    html: render_html(&body, &signature, step == 1),
                    step,
                    delay_days,
                    subject,
                    body,
                    sent_at: sent_at.map(|t| t.format("%-d %b %Y %H:%M").to_string()),
                },
            )
            .collect(),
        problems,
        notes,
        stop_reason,
        approved_at: approved_at.map(|t| t.format("%-d %b %Y").to_string()),
        version,
        sending_ready: false,
    }))
}

/// What stops approval, and what is only worth a look.
async fn checks(
    pool: &PgPool,
    org_id: Uuid,
    candidacy: Uuid,
    ctx: &Context,
    to_email: &str,
    signature: &str,
    steps: &[StepRow],
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let mut problems = Vec::new();
    let mut notes = Vec::new();
    let companies = employer::locked_out(pool, org_id, ctx.role_id).await?;
    for (step, _, subject, body, _) in steps {
        let named = client_mentions(&format!("{subject}\n{body}"), &companies);
        if !named.is_empty() {
            problems.push(format!(
                "Email {step} names the client ({}). Take it out.",
                named.join(", ")
            ));
        }
    }
    if signature.trim().is_empty() {
        problems.push("Add your email signature in Settings first.".into());
    }
    let dnc: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM do_not_contact WHERE org_id = $1 AND identifier = lower($2))",
    )
    .bind(org_id)
    .bind(to_email)
    .fetch_one(pool)
    .await?;
    // Opted out here or in Recruitly, or on the list by any identifier.
    let blocked = one_row(pool, org_id, candidacy).await?.do_not_contact;
    let verdict = employer::check_person(pool, org_id, ctx.role_id, ctx.person_id).await?;
    let still_personal: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM contact WHERE person_id = $1 AND org_id = $2
                          AND kind = 'personal_email' AND lower(value) = lower($3))",
    )
    .bind(ctx.person_id)
    .bind(org_id)
    .bind(to_email)
    .fetch_one(pool)
    .await?;
    let check = SendCheck {
        channel: Channel::Email,
        works_at_hiring_client: verdict == Verdict::LockedOut,
        person_opted_out: blocked,
        on_do_not_contact_list: dnc,
        // Approving is not sending; the pause is checked before every send.
        org_sending_paused: false,
        sequence_approved: true,
        replied_any_channel: false,
        to_personal_email: still_personal,
    };
    if let Err(b) = policy::may_send(check) {
        problems.push(
            match b {
                Blocked::WorksAtHiringClient => "Works at the client this role is for.",
                Blocked::OptedOut | Blocked::DoNotContact => "On the do-not-contact list.",
                Blocked::NotPersonalEmail => {
                    "That address is no longer a personal email on record."
                }
                Blocked::SendingPaused | Blocked::NotApproved | Blocked::AlreadyReplied => {
                    "Cannot be approved right now."
                }
            }
            .to_string(),
        );
    }
    if verdict == Verdict::Unknown {
        notes.push("No current employer on record. Check they are not at the client.".into());
    }
    if let Some((_, _, _, body, _)) = steps.first() {
        let n = words(body);
        if n > MAX_WORDS {
            notes.push(format!("Email 1 is {n} words. Keep it under {MAX_WORDS}."));
        }
    }
    if ctx.paused {
        notes.push("Sending is paused by an admin.".into());
    }
    Ok((problems, notes))
}

// ---------- Handlers ----------

async fn answer(pool: &PgPool, user: &CurrentUser, candidacy: Uuid) -> Response {
    match view(pool, user.org_id, user.id, candidacy).await {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => server_error(e),
    }
}

/// GET /api/candidates/:id/outreach
pub async fn get(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(candidacy): Path<Uuid>,
) -> Response {
    let Some(pool) = state.pool.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    answer(&pool, &user, candidacy).await
}

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    /// Replace an existing draft (or a stopped sequence) with a fresh one.
    #[serde(default)]
    pub fresh: bool,
}

/// POST /api/candidates/:id/outreach: draft the three emails.
pub async fn start(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(candidacy): Path<Uuid>,
    Query(q): Query<StartQuery>,
) -> Response {
    let Some(pool) = state.pool.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let ctx = match context(&pool, user.org_id, user.id, candidacy).await {
        Ok(Some(c)) => c,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return server_error(e),
    };
    let existing: Result<Option<(Uuid, OutreachStatus)>, _> =
        sqlx::query_as("SELECT id, status FROM outreach WHERE candidacy_id = $1 AND org_id = $2")
            .bind(candidacy)
            .bind(user.org_id)
            .fetch_optional(&pool)
            .await;
    let existing = match existing {
        Ok(e) => e,
        Err(e) => return server_error(e),
    };
    match existing {
        Some((_, OutreachStatus::Draft)) if !q.fresh => {
            return answer(&pool, &user, candidacy).await
        }
        Some((_, OutreachStatus::Approved | OutreachStatus::Active | OutreachStatus::Done)) => {
            return refuse(
                StatusCode::CONFLICT,
                "Already approved. Stop it first to start again.",
            )
        }
        _ => {}
    }
    let can_start = matches!(
        (ctx.state, existing.map(|e| e.1)),
        (CandidacyState::Shortlisted, _) | (CandidacyState::Drafted, Some(OutreachStatus::Draft))
    );
    if !can_start {
        return refuse(StatusCode::CONFLICT, "Shortlist them first.");
    }
    let to = match personal_email(&pool, user.org_id, ctx.person_id).await {
        Ok(Some(e)) => e,
        Ok(None) => {
            return refuse(
                StatusCode::CONFLICT,
                "No personal email on record, so no email can be sent.",
            )
        }
        Err(e) => return server_error(e),
    };
    let lines = match confirmed_brief(&pool, ctx.role_id).await {
        Ok(b) => b.map(|b| b.2).unwrap_or_default(),
        Err(e) => return server_error(e),
    };
    let intro = if ctx.intro.trim().is_empty() {
        format!("I'm at {}, a recruitment business", ctx.org_name)
    } else {
        ctx.intro.trim().trim_end_matches(['.', ',']).to_string()
    };
    let steps = draft_steps(
        &first_name(&ctx.full_name),
        &ctx.title,
        &place(&lines.locations),
        &bullets(&lines),
        &intro,
    );
    let result = async {
        let mut tx = pool.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO outreach (org_id, candidacy_id, sender_id, to_email)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (candidacy_id) DO UPDATE SET sender_id = $3, to_email = $4,
                    status = 'draft', stop_reason = NULL, approved_by = NULL, approved_at = NULL,
                    version = outreach.version + 1, updated_at = now()
             RETURNING id",
        )
        .bind(user.org_id)
        .bind(candidacy)
        .bind(user.id)
        .bind(&to)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM outreach_step WHERE outreach_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for (i, s) in steps.iter().enumerate() {
            sqlx::query(
                "INSERT INTO outreach_step (org_id, outreach_id, step, delay_days, subject, body)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(user.org_id)
            .bind(id)
            .bind(i as i32 + 1)
            .bind(DELAYS[i])
            .bind(&s.subject)
            .bind(&s.body)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE candidacy SET state = 'drafted', version = version + 1
             WHERE id = $1 AND state = 'shortlisted'",
        )
        .bind(candidacy)
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::OUTREACH_DRAFTED,
            &format!("candidacy:{candidacy}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => answer(&pool, &user, candidacy).await,
        Err(e) => server_error(e),
    }
}

/// PUT /api/candidates/:id/outreach: save edits to the draft.
pub async fn save(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(candidacy): Path<Uuid>,
    Json(edit): Json<OutreachEdit>,
) -> Response {
    let Some(pool) = state.pool.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut seen = Vec::new();
    for s in &edit.steps {
        if !(1..=3).contains(&s.step) || seen.contains(&s.step) {
            return refuse(StatusCode::BAD_REQUEST, "Unknown email.");
        }
        seen.push(s.step);
        let subject = s.subject.trim();
        if subject.is_empty() || subject.chars().count() > MAX_SUBJECT {
            return refuse(
                StatusCode::BAD_REQUEST,
                "Every email needs a short subject.",
            );
        }
        if s.body.trim().is_empty() || s.body.chars().count() > MAX_BODY {
            return refuse(
                StatusCode::BAD_REQUEST,
                "Every email needs some words, and not too many.",
            );
        }
    }
    let result = async {
        let mut tx = pool.begin().await?;
        let id: Option<Uuid> = sqlx::query_scalar(
            "UPDATE outreach SET version = version + 1, updated_at = now()
             WHERE candidacy_id = $1 AND org_id = $2 AND status = 'draft' AND version = $3
             RETURNING id",
        )
        .bind(candidacy)
        .bind(user.org_id)
        .bind(edit.version)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(id) = id else {
            return anyhow::Ok(false);
        };
        for s in &edit.steps {
            sqlx::query(
                "UPDATE outreach_step SET subject = $3, body = $4 WHERE outreach_id = $1 AND step = $2",
            )
            .bind(id)
            .bind(s.step)
            .bind(s.subject.trim())
            .bind(s.body.trim())
            .execute(&mut *tx)
            .await?;
        }
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::OUTREACH_EDITED,
            &format!("candidacy:{candidacy}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(true)
    }
    .await;
    match result {
        Ok(true) => answer(&pool, &user, candidacy).await,
        Ok(false) => refuse(
            StatusCode::CONFLICT,
            "These emails changed in another window, or are no longer a draft. Reload to see the latest.",
        ),
        Err(e) => server_error(e),
    }
}

/// POST /api/candidates/:id/outreach/approve: one approval for all three.
pub async fn approve(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(candidacy): Path<Uuid>,
    Json(a): Json<OutreachAction>,
) -> Response {
    let Some(pool) = state.pool.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let v = match view(&pool, user.org_id, user.id, candidacy).await {
        Ok(Some(v)) => v,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return server_error(e),
    };
    if v.status != Some(OutreachStatus::Draft) || v.version != a.version {
        return refuse(
            StatusCode::CONFLICT,
            "These emails changed, or are no longer a draft. Reload to see the latest.",
        );
    }
    if let Some(p) = v.problems.first() {
        return refuse(StatusCode::CONFLICT, p.clone());
    }
    let result = async {
        let mut tx = pool.begin().await?;
        let done = sqlx::query(
            "UPDATE outreach SET status = 'approved', approved_by = $3, approved_at = now(),
                    version = version + 1, updated_at = now()
             WHERE candidacy_id = $1 AND org_id = $2 AND status = 'draft' AND version = $4",
        )
        .bind(candidacy)
        .bind(user.org_id)
        .bind(user.id)
        .bind(a.version)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if done == 0 {
            return anyhow::Ok(false);
        }
        sqlx::query(
            "UPDATE candidacy SET state = 'approved', sequence_approved_by = $2,
                    sequence_approved_at = now(), version = version + 1
             WHERE id = $1 AND state = 'drafted'",
        )
        .bind(candidacy)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::OUTREACH_APPROVED,
            &format!("candidacy:{candidacy}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(true)
    }
    .await;
    match result {
        Ok(true) => answer(&pool, &user, candidacy).await,
        Ok(false) => refuse(
            StatusCode::CONFLICT,
            "These emails changed, or are no longer a draft. Reload to see the latest.",
        ),
        Err(e) => server_error(e),
    }
}

/// POST /api/candidates/:id/outreach/stop: nothing more is sent.
pub async fn stop(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(candidacy): Path<Uuid>,
    Json(a): Json<OutreachAction>,
) -> Response {
    let Some(pool) = state.pool.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = async {
        let mut tx = pool.begin().await?;
        let done = sqlx::query(
            "UPDATE outreach SET status = 'stopped', stop_reason = $4,
                    version = version + 1, updated_at = now()
             WHERE candidacy_id = $1 AND org_id = $2 AND version = $3
               AND status IN ('draft', 'approved', 'active')",
        )
        .bind(candidacy)
        .bind(user.org_id)
        .bind(a.version)
        .bind(format!("Stopped by {}", user.name))
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if done == 0 {
            return anyhow::Ok(false);
        }
        // Nothing went out yet: back to the shortlist.
        sqlx::query(
            "UPDATE candidacy SET state = 'shortlisted', sequence_approved_by = NULL,
                    sequence_approved_at = NULL, version = version + 1
             WHERE id = $1 AND state IN ('drafted', 'approved')",
        )
        .bind(candidacy)
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::OUTREACH_STOPPED,
            &format!("candidacy:{candidacy}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(true)
    }
    .await;
    match result {
        Ok(true) => answer(&pool, &user, candidacy).await,
        Ok(false) => refuse(
            StatusCode::CONFLICT,
            "These emails changed, or have already stopped. Reload to see the latest.",
        ),
        Err(e) => server_error(e),
    }
}

/// GET /api/me/outreach
pub async fn get_settings(State(state): State<AppState>, user: CurrentUser) -> Response {
    let Some(pool) = state.pool.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let row: Result<(String, String), _> =
        sqlx::query_as("SELECT signature, intro FROM app_user WHERE id = $1 AND org_id = $2")
            .bind(user.id)
            .bind(user.org_id)
            .fetch_one(&pool)
            .await;
    match row {
        Ok((signature, intro)) => Json(OutreachSettings { signature, intro }).into_response(),
        Err(e) => server_error(e),
    }
}

/// PUT /api/me/outreach
pub async fn put_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(s): Json<OutreachSettings>,
) -> Response {
    let Some(pool) = state.pool.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if s.signature.chars().count() > MAX_SIGNATURE {
        return refuse(StatusCode::BAD_REQUEST, "The signature is too long.");
    }
    if s.intro.chars().count() > MAX_INTRO || s.intro.contains('\n') {
        return refuse(
            StatusCode::BAD_REQUEST,
            "Keep the introduction to one short line.",
        );
    }
    let result = async {
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE app_user SET signature = $3, intro = $4 WHERE id = $1 AND org_id = $2")
            .bind(user.id)
            .bind(user.org_id)
            .bind(s.signature.trim_end())
            .bind(s.intro.trim())
            .execute(&mut *tx)
            .await?;
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::OUTREACH_SETTINGS,
            &format!("user:{}", user.id),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => Json(OutreachSettings {
            signature: s.signature.trim_end().to_string(),
            intro: s.intro.trim().to_string(),
        })
        .into_response(),
        Err(e) => server_error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_email_reads_in_kais_words() {
        let [one, two, three] = draft_steps(
            "Sam",
            "Senior Cloud Security Engineer",
            "Dubai",
            &["Hands-on AWS security engineering".into(), "IAM".into()],
            "I'm a Director at Austin Werner, a recruitment business",
        );
        assert_eq!(one.subject, "Senior Cloud Security Engineer role, Dubai");
        assert_eq!(
            one.body,
            "Hey Sam,\n\nHope you're well. Just found your details and thought you might be well aligned to a role I'm working on at the moment.\n\nI'm a Director at Austin Werner, a recruitment business, and we have a Senior Cloud Security Engineer role in Dubai. A brief summary of what they're looking for:\n\n- Hands-on AWS security engineering\n- IAM\n\nLmk if you're interested, and if you're open to a move right now."
        );
        assert_eq!(
            two.subject,
            "Re: Senior Cloud Security Engineer role, Dubai"
        );
        assert!(two
            .body
            .contains("Senior Cloud Security Engineer role in Dubai could suit you"));
        assert!(three.body.starts_with("Hey Sam, last note from me"));
        for s in [&one, &two, &three] {
            assert!(
                !s.body.contains('\u{2014}') && !s.body.contains('\u{2013}'),
                "no dashes"
            );
        }
        assert!(words(&one.body) <= MAX_WORDS);
    }

    #[test]
    fn bullets_come_from_the_brief() {
        let lines = BriefLines {
            must_haves: vec!["cloud security.".into(), " ".into(), "IAM".into()],
            capabilities: vec!["mentoring".into(), "extra".into()],
            ..BriefLines::default()
        };
        assert_eq!(bullets(&lines), ["Cloud security", "IAM", "Mentoring"]);
        assert_eq!(
            place(&["New York, NY".into(), "Dubai".into(), "London".into()]),
            "New York or Dubai"
        );
        assert_eq!(first_name("  Sample Person "), "Sample");
        let [one, ..] = draft_steps(
            "Sam",
            "Engineer",
            "",
            &[],
            "I'm at Austin Werner, a recruitment business",
        );
        assert!(one.body.contains("we have a Engineer role.\n\nLmk"));
        assert_eq!(one.subject, "Engineer role");
    }

    #[test]
    fn the_client_is_never_named() {
        let client = Company {
            id: Uuid::nil(),
            name: "Northwind Capital Ltd".into(),
            domain: Some("https://www.northwindcap.example/".into()),
            hiring: true,
        };
        let c = std::slice::from_ref(&client);
        assert_eq!(
            client_mentions("A role at Northwind Capital in Dubai", c),
            ["Northwind Capital Ltd"]
        );
        assert_eq!(
            client_mentions("see northwindcap.example", c),
            ["Northwind Capital Ltd"]
        );
        assert!(client_mentions("A north-facing wind capital role", c).is_empty());
        assert!(client_mentions("A role in Dubai", c).is_empty());
    }

    #[test]
    fn the_email_renders_with_signature_links_and_the_source_line() {
        let html = render_html(
            "Hey Sam,\n\nIntro & more.\n\n- One\n- Two <b>\n\nLmk.",
            "-- \nRegards\nKai\n[Linkedin](https://www.linkedin.com/in/x/)\nhttps://austinwerner.io\n[bad](javascript:alert(1))",
            true,
        );
        assert!(html.contains("<p>Hey Sam,</p><p>Intro &amp; more.</p><ul><li>One</li><li>Two &lt;b&gt;</li></ul><p>Lmk.</p>"));
        assert!(html.contains("<a href=\"https://www.linkedin.com/in/x/\">Linkedin</a>"));
        assert!(html.contains("<a href=\"https://austinwerner.io\">https://austinwerner.io</a>"));
        assert!(!html.contains("href=\"javascript"), "only web links");
        assert!(html.contains("font-size:11px;color:#888"));
        assert!(html.contains("Reply &quot;no&quot;"));
        assert!(!render_html("Hi", "Kai", false).contains("data provider"));
    }
}
