//! Today (SRS F16): what needs a person now. Replies first, then emails
//! waiting for approval, then what is going out.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    app::AppState,
    audit,
    auth::CurrentUser,
    sending::{dubai_date, dubai_time, in_hours},
};

/// Most items shown in each list.
const MAX_ITEMS: i64 = 100;

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct TodayReply {
    pub candidacy_id: String,
    pub role_id: String,
    pub role_title: String,
    pub name: String,
    /// "reply", "auto" (an automatic reply) or "bounce".
    pub kind: String,
    /// e.g. "2 Oct 2026 09:14" (Dubai).
    pub at: String,
    /// Whose Outlook it came to.
    pub sender: String,
    /// Already on the do-not-contact list or opted out.
    pub opted_out: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct TodayItem {
    pub candidacy_id: String,
    pub role_id: String,
    pub role_title: String,
    pub name: String,
    /// e.g. "Email 2 due Mon 6 Oct".
    pub note: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct TodayView {
    pub replies: Vec<TodayReply>,
    /// Your drafted emails, waiting for your approval.
    pub to_approve: Vec<TodayItem>,
    /// Your approved emails still to go.
    pub going_out: Vec<TodayItem>,
    /// An admin has paused all sending.
    pub paused: bool,
    /// Your Outlook is connected and working.
    pub outlook_ready: bool,
    /// Monday to Friday, 08:00 to 18:00 Dubai.
    pub in_hours: bool,
}

type ReplyRow = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    DateTime<Utc>,
    String,
    bool,
);
type ItemRow = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    Option<i32>,
    Option<DateTime<Utc>>,
);

/// GET /api/today
pub async fn today(State(state): State<AppState>, user: CurrentUser) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = async {
        // Everyone's replies: a small team, and a reply should not wait for one person.
        let replies: Vec<ReplyRow> = sqlx::query_as(
            "SELECT c.id, r.id, r.title, p.full_name, o.reply_kind, o.replied_at, u.name,
                    (p.opted_out OR EXISTS (SELECT 1 FROM do_not_contact d
                                            WHERE d.org_id = o.org_id AND d.identifier = lower(o.to_email)))
             FROM outreach o
             JOIN candidacy c ON c.id = o.candidacy_id
             JOIN person p ON p.id = c.person_id
             JOIN role r ON r.id = c.role_id
             JOIN app_user u ON u.id = o.sender_id
             WHERE o.org_id = $1 AND o.reply_kind IS NOT NULL AND o.reply_handled_at IS NULL
             ORDER BY o.replied_at DESC LIMIT $2",
        )
        .bind(user.org_id)
        .bind(MAX_ITEMS)
        .fetch_all(pool)
        .await?;
        let to_approve: Vec<ItemRow> = sqlx::query_as(
            "SELECT c.id, r.id, r.title, p.full_name, o.status::text, NULL::int, NULL::timestamptz
             FROM outreach o
             JOIN candidacy c ON c.id = o.candidacy_id
             JOIN person p ON p.id = c.person_id
             JOIN role r ON r.id = c.role_id
             WHERE o.org_id = $1 AND o.sender_id = $2 AND o.status = 'draft'
               AND r.closed_at IS NULL
             ORDER BY o.updated_at DESC LIMIT $3",
        )
        .bind(user.org_id)
        .bind(user.id)
        .bind(MAX_ITEMS)
        .fetch_all(pool)
        .await?;
        // The next email of each sequence, and when the one before it went.
        let going_out: Vec<ItemRow> = sqlx::query_as(
            "SELECT c.id, r.id, r.title, p.full_name, o.status::text, n.step,
                    b.sent_at + make_interval(days => n.delay_days)
             FROM outreach o
             JOIN candidacy c ON c.id = o.candidacy_id
             JOIN person p ON p.id = c.person_id
             JOIN role r ON r.id = c.role_id
             JOIN LATERAL (SELECT step, delay_days FROM outreach_step s
                           WHERE s.outreach_id = o.id AND s.sent_at IS NULL
                           ORDER BY step LIMIT 1) n ON true
             LEFT JOIN outreach_step b ON b.outreach_id = o.id AND b.step = n.step - 1
             WHERE o.org_id = $1 AND o.sender_id = $2 AND o.status IN ('approved', 'active')
               AND r.closed_at IS NULL
             ORDER BY 7 NULLS FIRST, o.approved_at LIMIT $3",
        )
        .bind(user.org_id)
        .bind(user.id)
        .bind(MAX_ITEMS)
        .fetch_all(pool)
        .await?;
        let (paused, ready): (bool, bool) = sqlx::query_as(
            "SELECT g.sending_paused,
                    EXISTS (SELECT 1 FROM mailbox m WHERE m.user_id = $2 AND m.broken IS NULL)
             FROM org g WHERE g.id = $1",
        )
        .bind(user.org_id)
        .bind(user.id)
        .fetch_one(pool)
        .await?;
        let now = Utc::now();
        let item = |(c, r, title, name, _, step, due): ItemRow, note: String| TodayItem {
            candidacy_id: c.to_string(),
            role_id: r.to_string(),
            role_title: title,
            name,
            note: if note.is_empty() {
                match (step, due) {
                    (Some(1) | None, _) | (_, None) => "Email 1 goes out next".to_string(),
                    (Some(n), Some(d)) if d <= now => format!("Email {n} goes out next"),
                    (Some(n), Some(d)) => format!("Email {n} due {}", dubai_date(d)),
                }
            } else {
                note
            },
        };
        anyhow::Ok(TodayView {
            replies: replies
                .into_iter()
                .map(|(c, r, title, name, kind, at, sender, opted_out)| TodayReply {
                    candidacy_id: c.to_string(),
                    role_id: r.to_string(),
                    role_title: title,
                    name,
                    kind,
                    at: dubai_time(at),
                    sender,
                    opted_out,
                })
                .collect(),
            to_approve: to_approve
                .into_iter()
                .map(|row| item(row, "3 emails drafted".into()))
                .collect(),
            going_out: going_out
                .into_iter()
                .map(|row| item(row, String::new()))
                .collect(),
            paused,
            outlook_ready: ready && state.mail.configured(),
            in_hours: in_hours(now),
        })
    }
    .await;
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "today failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /api/candidates/:id/reply-handled: take a reply off Today.
pub async fn handled(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(candidacy): Path<Uuid>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = async {
        let mut tx = pool.begin().await?;
        let done = sqlx::query(
            "UPDATE outreach SET reply_handled_at = now()
             WHERE candidacy_id = $1 AND org_id = $2 AND reply_kind IS NOT NULL
               AND reply_handled_at IS NULL",
        )
        .bind(candidacy)
        .bind(user.org_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if done == 0 {
            return anyhow::Ok(false);
        }
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::REPLY_HANDLED,
            &format!("candidacy:{candidacy}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(true)
    }
    .await;
    match result {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "could not mark the reply handled");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /api/candidates/:id/opt-out: they said no. Never contacted again by
/// anyone, for any role: opted out, their address on the do-not-contact list,
/// and every email waiting for them stopped. Anyone on the team may do this.
pub async fn opt_out(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(candidacy): Path<Uuid>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = async {
        let mut tx = pool.begin().await?;
        let person: Option<Uuid> = sqlx::query_scalar(
            "UPDATE person p SET opted_out = true FROM candidacy c
             WHERE c.id = $1 AND c.org_id = $2 AND p.id = c.person_id RETURNING p.id",
        )
        .bind(candidacy)
        .bind(user.org_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(person) = person else {
            return anyhow::Ok(false);
        };
        // Every address they were emailed at goes on the list.
        sqlx::query(
            "INSERT INTO do_not_contact (org_id, identifier, reason)
             SELECT DISTINCT o.org_id, lower(o.to_email), 'opt_out'::text FROM outreach o
             JOIN candidacy c ON c.id = o.candidacy_id
             WHERE c.person_id = $1 AND o.org_id = $2
             ON CONFLICT (org_id, identifier) DO NOTHING",
        )
        .bind(person)
        .bind(user.org_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "WITH stopped AS (
               UPDATE outreach o SET status = 'stopped', stop_reason = 'Opted out',
                      version = o.version + 1, updated_at = now()
               FROM candidacy c
               WHERE c.id = o.candidacy_id AND c.person_id = $1 AND o.org_id = $2
                 AND o.status IN ('draft', 'approved', 'active')
               RETURNING o.candidacy_id)
             UPDATE candidacy SET state = 'shortlisted', sequence_approved_by = NULL,
                    sequence_approved_at = NULL, version = version + 1
             WHERE id IN (SELECT candidacy_id FROM stopped) AND state IN ('drafted', 'approved')",
        )
        .bind(person)
        .bind(user.org_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE outreach SET reply_handled_at = coalesce(reply_handled_at, now())
             WHERE candidacy_id = $1 AND reply_kind IS NOT NULL",
        )
        .bind(candidacy)
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::OPTED_OUT,
            &format!("candidacy:{candidacy}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(true)
    }
    .await;
    match result {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "could not record the opt-out");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
