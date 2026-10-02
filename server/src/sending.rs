//! Sends approved emails from each sender's own Outlook, and stops a sequence
//! the moment a reply arrives (SRS F14, F15).
//!
//! Kai's rules (1 and 2 Oct 2026):
//! - Only in working hours: Monday to Friday, 08:00 to 18:00 Dubai time.
//! - At most `first_emails_per_day` first emails per person per Dubai day.
//!   Follow-ups do not count. At most one email per person per minute.
//! - Follow-ups go in the same thread, 3 and then 4 days after the one before.
//! - Every check from approval runs again just before each email: do not
//!   contact, opted out, works at the client, still a personal email, the
//!   client named in the emails or the saved signature, and the pause switch.
//! - Any reply stops the rest: a reply, an automatic reply or a bounce. Replies
//!   are checked every 15 minutes, and again just before every follow-up.
//! - Nothing goes twice. A step is marked just before the send call; if
//!   Sourcer stops mid-send, Outlook is asked whether it went before anything
//!   else happens.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Datelike, FixedOffset, TimeZone, Timelike, Utc, Weekday};
use sqlx::PgPool;
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    audit,
    mail::{Inbound, Mail, MailError, SentState},
    outreach::{self, render_html},
};

/// Dubai is UTC+4 all year.
const DUBAI_SECS: i32 = 4 * 3600;
pub const OPEN_HOUR: u32 = 8;
pub const CLOSE_HOUR: u32 = 18;
/// How often replies are checked.
pub const REPLY_EVERY: Duration = Duration::from_secs(15 * 60);
/// How often the loop looks for emails due.
pub const TICK: Duration = Duration::from_secs(60);
/// A send marked longer ago than this is checked against Outlook.
const STUCK_MINUTES: i64 = 5;
/// An email neither in Drafts nor in Sent Items this long is left to a person.
const UNCONFIRMED_MINUTES: i64 = 60;
/// Replies are watched for this long after the last email.
const WATCH_DAYS: i64 = 30;
/// Reply checks overlap the last one by this much, so nothing slips between.
const OVERLAP_MINUTES: i64 = 10;

fn dubai() -> FixedOffset {
    FixedOffset::east_opt(DUBAI_SECS).expect("valid offset")
}

/// Monday to Friday, 08:00 to 18:00 in Dubai.
pub fn in_hours(now: DateTime<Utc>) -> bool {
    let d = now.with_timezone(&dubai());
    !matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
        && (OPEN_HOUR..CLOSE_HOUR).contains(&d.hour())
}

/// Midnight today in Dubai, as UTC: the start of the daily limit.
pub fn dubai_day_start(now: DateTime<Utc>) -> DateTime<Utc> {
    let d = now.with_timezone(&dubai()).date_naive();
    dubai()
        .from_local_datetime(&d.and_hms_opt(0, 0, 0).expect("midnight"))
        .single()
        .expect("fixed offset is unambiguous")
        .with_timezone(&Utc)
}

/// "Mon 6 Oct", in Dubai.
pub fn dubai_date(t: DateTime<Utc>) -> String {
    t.with_timezone(&dubai()).format("%a %-d %b").to_string()
}

/// "2 Oct 2026 09:14", in Dubai.
pub fn dubai_time(t: DateTime<Utc>) -> String {
    t.with_timezone(&dubai())
        .format("%-d %b %Y %H:%M")
        .to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyKind {
    Reply,
    Auto,
    Bounce,
}

impl ReplyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::Auto => "auto",
            Self::Bounce => "bounce",
        }
    }
    /// Why the sequence stopped, in words.
    pub fn stop_reason(self) -> &'static str {
        match self {
            Self::Reply => "Replied",
            Self::Auto => "Automatic reply",
            Self::Bounce => "Bounced: the address did not work",
        }
    }
}

/// A reply, an automatic reply or a bounce, from the sender and subject only.
pub fn classify(m: &Inbound) -> ReplyKind {
    let local = m.from.split('@').next().unwrap_or_default();
    let subject = m.subject.trim().to_lowercase();
    if matches!(local, "postmaster" | "mailer-daemon")
        || local.starts_with("microsoftexchange")
        || [
            "undeliverable",
            "delivery status notification",
            "mail delivery failed",
            "delivery has failed",
            "returned mail",
        ]
        .iter()
        .any(|p| subject.starts_with(p))
    {
        return ReplyKind::Bounce;
    }
    if [
        "automatic reply",
        "auto:",
        "autoreply",
        "auto-reply",
        "auto reply",
        "out of office",
        "out of the office",
        "ooo",
    ]
    .iter()
    .any(|p| subject.starts_with(p))
    {
        return ReplyKind::Auto;
    }
    ReplyKind::Reply
}

/// One sequence being watched for replies.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Thread {
    pub outreach_id: Uuid,
    pub conversation_id: Option<String>,
    pub to_email: String,
    pub first_sent: DateTime<Utc>,
}

/// The first reply to each thread among `mail`, from anyone but the sender.
/// A match is the same Outlook thread, or anything from the candidate's address.
pub fn match_replies<'a>(
    own: &str,
    threads: &'a [Thread],
    mail: &[Inbound],
) -> Vec<(&'a Thread, ReplyKind, DateTime<Utc>)> {
    let mut sorted: Vec<&Inbound> = mail.iter().filter(|m| m.from != own).collect();
    sorted.sort_by_key(|m| m.received);
    let mut out: Vec<(&Thread, ReplyKind, DateTime<Utc>)> = Vec::new();
    for t in threads {
        let hit = sorted.iter().find(|m| {
            m.received >= t.first_sent
                && (m.from == t.to_email.to_lowercase()
                    || (m.conversation_id.is_some() && m.conversation_id == t.conversation_id))
        });
        if let Some(m) = hit {
            out.push((t, classify(m), m.received));
        }
    }
    out
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Due {
    step_id: Uuid,
    step: i32,
    outreach_id: Uuid,
    org_id: Uuid,
    candidacy_id: Uuid,
    sender_id: Uuid,
    to_email: String,
    subject: String,
    body: String,
}

pub struct Sender {
    pub pool: PgPool,
    pub mail: Arc<Mail>,
}

/// Held by the one Sourcer process that sends, so two never send at once
/// (for example while an update starts the new version).
const LOCK_KEY: i64 = 7_262_001;
/// Outlook refusing the same email this many times stops the sequence.
const MAX_REFUSALS: i32 = 3;

/// What the last look before sending found.
enum Gate {
    Sent,
    /// Stopped, replied, opted out or done: the draft is not needed.
    Over,
    /// Paused, or the working day ended: try later.
    Wait,
}

/// Outlook said no to the email itself (bad address, missing thread), as
/// opposed to a network or service problem that will pass.
fn refused(e: &MailError) -> bool {
    let MailError::Other(e) = e else {
        return false;
    };
    e.chain().any(|c| {
        c.downcast_ref::<reqwest::Error>()
            .and_then(reqwest::Error::status)
            .is_some_and(|s| {
                s.is_client_error()
                    && s != reqwest::StatusCode::UNAUTHORIZED
                    && s != reqwest::StatusCode::TOO_MANY_REQUESTS
            })
    })
}

/// The tick's time, moved on by however long the tick has run.
fn clock(now: DateTime<Utc>, started: std::time::Instant) -> DateTime<Utc> {
    now + chrono::Duration::from_std(started.elapsed()).unwrap_or_default()
}

impl Sender {
    /// Runs until `stop` turns true: emails every minute, replies every 15.
    pub async fn run(self, mut stop: watch::Receiver<bool>) {
        if !self.mail.configured() {
            tracing::warn!(
                "Outlook sending is off; set MAIL_TOKEN_KEY and Microsoft sign-in to turn it on"
            );
            return;
        }
        let mut lock: Option<sqlx::pool::PoolConnection<sqlx::Postgres>> = None;
        let mut last_replies: Option<std::time::Instant> = None;
        loop {
            // The lock lives on its connection: if that dropped (a database
            // restart), the lock went with it and is taken again.
            if let Some(conn) = lock.as_mut() {
                if sqlx::query("SELECT 1").execute(&mut **conn).await.is_err() {
                    lock = None;
                }
            }
            if lock.is_none() {
                lock = self.take_lock().await;
            }
            if lock.is_some() {
                let now = Utc::now();
                if last_replies.is_none_or(|t| t.elapsed() >= REPLY_EVERY) {
                    if let Err(e) = self.check_replies(now).await {
                        tracing::warn!(error = %e, "reply check failed");
                    }
                    last_replies = Some(std::time::Instant::now());
                }
                if let Err(e) = self.tick(now).await {
                    tracing::warn!(error = %e, "sending failed");
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(TICK) => {}
                _ = stop.changed() => {
                    if *stop.borrow() { return; }
                }
            }
        }
    }

    /// The sending lock, held on its own connection for as long as this runs.
    async fn take_lock(&self) -> Option<sqlx::pool::PoolConnection<sqlx::Postgres>> {
        let mut conn = self.pool.acquire().await.ok()?;
        let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(LOCK_KEY)
            .fetch_one(&mut *conn)
            .await
            .ok()?;
        if !got {
            tracing::info!("another Sourcer is sending; waiting");
        }
        got.then_some(conn)
    }

    /// Settle any send left half-done, then send what is due.
    pub async fn tick(&self, now: DateTime<Utc>) -> anyhow::Result<()> {
        let started = std::time::Instant::now();
        let busy = self.recover(now, started).await;
        if in_hours(clock(now, started)) {
            self.send_due(now, started, &busy).await?;
        }
        Ok(())
    }

    /// At most one email per sender: follow-ups first, then first emails in
    /// the order they were approved, within the daily limit. Senders in
    /// `skip` already had a send settled this tick.
    async fn send_due(
        &self,
        now: DateTime<Utc>,
        started: std::time::Instant,
        skip: &std::collections::HashSet<Uuid>,
    ) -> anyhow::Result<usize> {
        let due: Vec<Due> = sqlx::query_as(
            "SELECT DISTINCT ON (o.sender_id)
                    s.id AS step_id, s.step, o.id AS outreach_id, o.org_id, o.candidacy_id,
                    o.sender_id, o.to_email, s.subject, s.body
             FROM outreach_step s
             JOIN outreach o ON o.id = s.outreach_id
             JOIN org g ON g.id = o.org_id
             JOIN mailbox m ON m.user_id = o.sender_id AND m.broken IS NULL
             JOIN app_user u ON u.id = o.sender_id AND u.disabled_at IS NULL
             WHERE s.sent_at IS NULL AND s.sending_since IS NULL AND NOT g.sending_paused
               AND o.reply_kind IS NULL
               AND (s.retry_after IS NULL OR s.retry_after <= $1)
               AND NOT EXISTS (SELECT 1 FROM outreach_step x JOIN outreach y ON y.id = x.outreach_id
                               WHERE y.sender_id = o.sender_id AND x.sending_since IS NOT NULL
                                 AND x.sent_at IS NULL)
               AND ((s.step = 1 AND o.status = 'approved'
                     AND (SELECT count(*) FROM outreach_step f JOIN outreach p ON p.id = f.outreach_id
                          WHERE p.sender_id = o.sender_id AND f.step = 1 AND f.sent_at >= $2)
                         < g.first_emails_per_day)
                 OR (s.step > 1 AND o.status = 'active' AND EXISTS (
                       SELECT 1 FROM outreach_step b WHERE b.outreach_id = o.id AND b.step = s.step - 1
                         AND b.sent_at + make_interval(days => s.delay_days) <= $1)))
             ORDER BY o.sender_id, s.step DESC, o.approved_at, s.id",
        )
        .bind(now)
        .bind(dubai_day_start(now))
        .fetch_all(&self.pool)
        .await?;
        let mut sent = 0;
        for d in due.iter().filter(|d| !skip.contains(&d.sender_id)) {
            match self.send_one(d, now, started).await {
                Ok(true) => sent += 1,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(outreach = %d.outreach_id, step = d.step, error = %e, "email not sent");
                    if let Err(e) = self.held_back(d, &e, clock(now, started)).await {
                        tracing::warn!(error = %e, "could not record the failed send");
                    }
                }
            }
        }
        Ok(sent)
    }

    /// Try a failed email again later. If Outlook keeps refusing the email
    /// itself, stop the sequence so the sender's other emails are not held up.
    async fn held_back(&self, d: &Due, e: &MailError, at: DateTime<Utc>) -> anyhow::Result<()> {
        let permanent = refused(e);
        let refusals: i32 = sqlx::query_scalar(
            "UPDATE outreach_step SET failures = failures + CASE WHEN $3 THEN 1 ELSE 0 END,
                    retry_after = $2 + make_interval(mins => CASE WHEN $3
                        THEN 30 * power(2, failures)::int ELSE 5 END)
             WHERE id = $1 RETURNING failures",
        )
        .bind(d.step_id)
        .bind(at)
        .bind(permanent)
        .fetch_one(&self.pool)
        .await?;
        if refusals >= MAX_REFUSALS {
            let reason = format!(
                "Not sent: Outlook refused email {} {MAX_REFUSALS} times. Check the address.",
                d.step
            );
            self.stop(d, &reason, "refused").await?;
        }
        Ok(())
    }

    /// Check, mark, draft, then the last look and the send. Returns whether it went.
    async fn send_one(
        &self,
        d: &Due,
        now: DateTime<Utc>,
        started: std::time::Instant,
    ) -> Result<bool, MailError> {
        let token = match self.mail.access_token(&self.pool, d.sender_id).await {
            Ok(t) => t,
            Err(MailError::NotConnected | MailError::Reconnect) => return Ok(false),
            Err(e) => return Err(e),
        };
        // A reply may have come in since the last check.
        if d.step > 1 {
            self.check_mailbox(d.sender_id, now).await?;
        }
        let problems =
            outreach::problems_before_send(&self.pool, d.org_id, d.candidacy_id, d.sender_id)
                .await?;
        if let Some(p) = problems.first() {
            self.stop(d, &format!("Not sent: {p}"), "checks").await?;
            return Ok(false);
        }
        // Mark it before anything reaches Outlook; only one sender can.
        let marked = sqlx::query(
            "UPDATE outreach_step SET sending_since = $2
             WHERE id = $1 AND sent_at IS NULL AND sending_since IS NULL",
        )
        .bind(d.step_id)
        .bind(clock(now, started))
        .execute(&self.pool)
        .await?
        .rows_affected();
        if marked == 0 {
            return Ok(false);
        }
        let signature: String =
            sqlx::query_scalar("SELECT coalesce(signature, '') FROM outreach WHERE id = $1")
                .bind(d.outreach_id)
                .fetch_one(&self.pool)
                .await?;
        let html = render_html(&d.body, &signature, d.step == 1);
        let created = if d.step == 1 {
            self.mail
                .create(d.sender_id, &token, &d.to_email, &d.subject, &html)
                .await
        } else {
            let first: Option<String> = sqlx::query_scalar(
                "SELECT graph_id FROM outreach_step WHERE outreach_id = $1 AND step = 1",
            )
            .bind(d.outreach_id)
            .fetch_one(&self.pool)
            .await?;
            match first {
                Some(first) => {
                    self.mail
                        .create_reply(d.sender_id, &token, &first, &d.to_email, &d.subject, &html)
                        .await
                }
                None => Err(MailError::Other(anyhow::anyhow!(
                    "first email has no Outlook id"
                ))),
            }
        };
        let created = match created {
            Ok(c) => c,
            Err(e) => {
                self.clear(d.step_id).await?;
                return Err(e);
            }
        };
        sqlx::query("UPDATE outreach_step SET graph_id = $2, message_id = $3 WHERE id = $1")
            .bind(d.step_id)
            .bind(&created.id)
            .bind(&created.internet_message_id)
            .execute(&self.pool)
            .await?;
        if d.step == 1 {
            sqlx::query("UPDATE outreach SET conversation_id = $2 WHERE id = $1")
                .bind(d.outreach_id)
                .bind(&created.conversation_id)
                .execute(&self.pool)
                .await?;
        }
        match self
            .send_locked(d, &token, &created.id, now, started)
            .await?
        {
            Gate::Sent => Ok(true),
            Gate::Over | Gate::Wait => {
                let _ = self.mail.delete(d.sender_id, &token, &created.id).await;
                self.clear(d.step_id).await?;
                Ok(false)
            }
        }
    }

    /// The last look and the send, with the sequence locked: Stop, reject,
    /// opt-out and do-not-contact all wait until the email is sent and
    /// recorded, so nothing goes after them and nothing is half-recorded.
    async fn send_locked(
        &self,
        d: &Due,
        token: &str,
        graph_id: &str,
        now: DateTime<Utc>,
        started: std::time::Instant,
    ) -> Result<Gate, MailError> {
        let mut tx = self.pool.begin().await?;
        let (open, paused): (bool, bool) = sqlx::query_as(
            "SELECT o.status IN ('approved', 'active') AND o.reply_kind IS NULL, g.sending_paused
             FROM outreach o JOIN org g ON g.id = o.org_id
             WHERE o.id = $1 FOR UPDATE OF o",
        )
        .bind(d.outreach_id)
        .fetch_one(&mut *tx)
        .await?;
        if !open {
            return Ok(Gate::Over);
        }
        if paused || !in_hours(clock(now, started)) {
            return Ok(Gate::Wait);
        }
        if let Err(e) = self.mail.send(d.sender_id, token, graph_id).await {
            if refused(&e) {
                // Outlook said no to this email: it did not go. Remove the
                // draft so the step can be tried again later, or stopped.
                drop(tx);
                let _ = self.mail.delete(d.sender_id, token, graph_id).await;
                self.clear(d.step_id).await?;
            }
            // Otherwise the mark stays, and the next tick asks Outlook.
            return Err(e);
        }
        finish_in(&mut tx, d.step_id, clock(now, started)).await?;
        tx.commit().await?;
        Ok(Gate::Sent)
    }

    /// Record a step as sent, outside a send (Outlook says it went).
    async fn finish(&self, step_id: Uuid, at: DateTime<Utc>) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        finish_in(&mut tx, step_id, at).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Stop a sequence that failed a check. Nothing more goes out. If nothing
    /// was sent yet, the person goes back to the shortlist.
    async fn stop(&self, d: &Due, reason: &str, why: &str) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let done = sqlx::query(
            "UPDATE outreach SET status = 'stopped', stop_reason = $2,
                    version = version + 1, updated_at = now()
             WHERE id = $1 AND status IN ('approved', 'active')",
        )
        .bind(d.outreach_id)
        .bind(reason)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if done == 0 {
            return Ok(());
        }
        sqlx::query(
            "UPDATE candidacy SET state = 'shortlisted', sequence_approved_by = NULL,
                    sequence_approved_at = NULL, version = version + 1
             WHERE id = $1 AND state IN ('drafted', 'approved')",
        )
        .bind(d.candidacy_id)
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            d.org_id,
            None,
            audit::action::OUTREACH_STOPPED,
            &format!("candidacy:{} reason:{why}", d.candidacy_id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Steps marked more than a few minutes ago and not recorded as sent:
    /// ask Outlook what happened before doing anything else. One bad row
    /// never holds up the rest. Returns the senders it dealt with, who send
    /// nothing else this tick.
    pub async fn recover(
        &self,
        now: DateTime<Utc>,
        started: std::time::Instant,
    ) -> std::collections::HashSet<Uuid> {
        let mut busy = std::collections::HashSet::new();
        let stuck: Vec<(Uuid, Option<String>, DateTime<Utc>)> = match sqlx::query_as(
            "SELECT id, graph_id, sending_since FROM outreach_step
             WHERE sent_at IS NULL AND sending_since < $1
               AND (retry_after IS NULL OR retry_after <= $2)",
        )
        .bind(now - chrono::Duration::minutes(STUCK_MINUTES))
        .bind(now)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "could not look for half-sent emails");
                return busy;
            }
        };
        for (step_id, graph_id, since) in stuck {
            let d = match self.due_of(step_id).await {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(step = %step_id, error = %e, "cannot settle a half-sent email yet");
                    continue;
                }
            };
            busy.insert(d.sender_id);
            if let Err(e) = self.recover_one(&d, graph_id, since, now, started).await {
                tracing::warn!(step = %step_id, error = %e, "cannot settle a half-sent email yet");
                if let Err(e) = self.held_back(&d, &e, clock(now, started)).await {
                    tracing::warn!(error = %e, "could not record the failed send");
                }
            }
        }
        busy
    }

    async fn recover_one(
        &self,
        d: &Due,
        graph_id: Option<String>,
        since: DateTime<Utc>,
        now: DateTime<Utc>,
        started: std::time::Instant,
    ) -> Result<(), MailError> {
        let Some(graph_id) = graph_id else {
            // Nothing reached Outlook.
            self.clear(d.step_id).await?;
            return Ok(());
        };
        let token = self.mail.access_token(&self.pool, d.sender_id).await?;
        match self.mail.sent_state(d.sender_id, &token, &graph_id).await? {
            SentState::Sent => self.finish(d.step_id, clock(now, started)).await?,
            SentState::Gone => self.unconfirmed(d).await?,
            SentState::InTransit => {
                // Usually on its way. Never sent again; after an hour a person decides.
                if now - since > chrono::Duration::minutes(UNCONFIRMED_MINUTES) {
                    self.unconfirmed(d).await?;
                }
            }
            SentState::Draft => {
                // It never went. Nothing is decided while sending could not happen anyway.
                let (open, paused, disabled): (bool, bool, bool) = sqlx::query_as(
                    "SELECT o.status IN ('approved', 'active') AND o.reply_kind IS NULL,
                            g.sending_paused, u.disabled_at IS NOT NULL
                     FROM outreach o JOIN org g ON g.id = o.org_id
                     JOIN app_user u ON u.id = o.sender_id WHERE o.id = $1",
                )
                .bind(d.outreach_id)
                .fetch_one(&self.pool)
                .await?;
                if !open || disabled {
                    let _ = self.mail.delete(d.sender_id, &token, &graph_id).await;
                    if disabled {
                        self.stop(
                            d,
                            "Not sent: the sender's account is switched off.",
                            "disabled",
                        )
                        .await?;
                    }
                    self.clear(d.step_id).await?;
                    return Ok(());
                }
                if paused || !in_hours(clock(now, started)) {
                    return Ok(()); // the draft waits
                }
                // Every check runs again, as for any email.
                if d.step > 1 {
                    self.check_mailbox(d.sender_id, now).await?;
                }
                let problems = outreach::problems_before_send(
                    &self.pool,
                    d.org_id,
                    d.candidacy_id,
                    d.sender_id,
                )
                .await?;
                if let Some(p) = problems.first() {
                    let _ = self.mail.delete(d.sender_id, &token, &graph_id).await;
                    self.stop(d, &format!("Not sent: {p}"), "checks").await?;
                    self.clear(d.step_id).await?;
                    return Ok(());
                }
                match self.send_locked(d, &token, &graph_id, now, started).await? {
                    Gate::Sent | Gate::Wait => {}
                    Gate::Over => {
                        let _ = self.mail.delete(d.sender_id, &token, &graph_id).await;
                        self.clear(d.step_id).await?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Outlook cannot say whether this email went. Treat it as sent: nothing
    /// more goes in this sequence, the person counts as contacted, and the
    /// emails can never be drafted again. A person checks Sent Items.
    async fn unconfirmed(&self, d: &Due) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE outreach_step SET unconfirmed_at = coalesce(sending_since, now()),
                    sending_since = NULL
             WHERE id = $1 AND sent_at IS NULL",
        )
        .bind(d.step_id)
        .execute(&mut *tx)
        .await?;
        // It may have gone, so it shows in their history like any email out.
        sqlx::query(
            "INSERT INTO touch (org_id, person_id, role_id, user_id, channel, direction,
                                message_id, sequence_step, at)
             SELECT c.org_id, c.person_id, c.role_id, $2, 'email', 'out', s.message_id, s.step,
                    s.unconfirmed_at
             FROM candidacy c, outreach_step s WHERE c.id = $1 AND s.id = $3
               AND s.unconfirmed_at IS NOT NULL",
        )
        .bind(d.candidacy_id)
        .bind(d.sender_id)
        .bind(d.step_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE outreach SET status = 'stopped', stop_reason = $2,
                    version = version + 1, updated_at = now()
             WHERE id = $1 AND status IN ('approved', 'active')",
        )
        .bind(d.outreach_id)
        .bind(format!(
            "Could not confirm email {} went out. Check Outlook Sent Items.",
            d.step
        ))
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE candidacy SET state = 'contacted', version = version + 1
             WHERE id = $1 AND state IN ('approved', 'drafted', 'shortlisted')",
        )
        .bind(d.candidacy_id)
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            d.org_id,
            None,
            audit::action::OUTREACH_STOPPED,
            &format!("candidacy:{} reason:unconfirmed", d.candidacy_id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Forget a send that did not happen, so the step can be tried again.
    async fn clear(&self, step_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE outreach_step SET sending_since = NULL, graph_id = NULL, message_id = NULL
             WHERE id = $1 AND sent_at IS NULL",
        )
        .bind(step_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn due_of(&self, step_id: Uuid) -> anyhow::Result<Due> {
        Ok(sqlx::query_as(
            "SELECT s.id AS step_id, s.step, o.id AS outreach_id, o.org_id, o.candidacy_id,
                    o.sender_id, o.to_email, s.subject, s.body
             FROM outreach_step s JOIN outreach o ON o.id = s.outreach_id WHERE s.id = $1",
        )
        .bind(step_id)
        .fetch_one(&self.pool)
        .await?)
    }

    /// Check every connected mailbox that has emails out.
    pub async fn check_replies(&self, now: DateTime<Utc>) -> anyhow::Result<()> {
        let senders: Vec<Uuid> = sqlx::query_scalar(
            "SELECT DISTINCT m.user_id FROM mailbox m JOIN outreach o ON o.sender_id = m.user_id
             WHERE m.broken IS NULL AND o.status IN ('active', 'done', 'stopped')
               AND o.reply_kind IS NULL
               AND EXISTS (SELECT 1 FROM outreach_step s WHERE s.outreach_id = o.id
                             AND coalesce(s.sent_at, s.unconfirmed_at) > $1)",
        )
        .bind(now - chrono::Duration::days(WATCH_DAYS))
        .fetch_all(&self.pool)
        .await?;
        for sender in senders {
            if let Err(e) = self.check_mailbox(sender, now).await {
                tracing::warn!(user = %sender, error = %e, "reply check failed for one mailbox");
            }
        }
        Ok(())
    }

    /// Read new mail in one mailbox (sender, subject and thread only) and stop
    /// any sequence it replies to.
    pub async fn check_mailbox(&self, sender: Uuid, now: DateTime<Utc>) -> Result<(), MailError> {
        let row: Option<(String, Option<DateTime<Utc>>)> =
            sqlx::query_as("SELECT address, checked_at FROM mailbox WHERE user_id = $1")
                .bind(sender)
                .fetch_optional(&self.pool)
                .await?;
        let Some((own, checked_at)) = row else {
            return Ok(());
        };
        let threads: Vec<Thread> = sqlx::query_as(
            "SELECT o.id AS outreach_id, o.conversation_id, o.to_email,
                    coalesce(f.sent_at, f.unconfirmed_at) AS first_sent
             FROM outreach o JOIN outreach_step f ON f.outreach_id = o.id AND f.step = 1
             WHERE o.sender_id = $1 AND o.status IN ('active', 'done', 'stopped')
               AND o.reply_kind IS NULL
               AND coalesce(f.sent_at, f.unconfirmed_at) IS NOT NULL
               AND EXISTS (SELECT 1 FROM outreach_step s WHERE s.outreach_id = o.id
                             AND coalesce(s.sent_at, s.unconfirmed_at) > $2)",
        )
        .bind(sender)
        .bind(now - chrono::Duration::days(WATCH_DAYS))
        .fetch_all(&self.pool)
        .await?;
        if threads.is_empty() {
            return Ok(());
        }
        let earliest = threads.iter().map(|t| t.first_sent).min().unwrap_or(now);
        let since = checked_at
            .map(|c| c - chrono::Duration::minutes(OVERLAP_MINUTES))
            .unwrap_or(earliest)
            .max(earliest - chrono::Duration::minutes(OVERLAP_MINUTES));
        let token = self.mail.access_token(&self.pool, sender).await?;
        let started = Utc::now();
        let (mail, reached) = self.mail.received_since(sender, &token, since).await?;
        for (t, kind, at) in match_replies(&own, &threads, &mail) {
            self.record_reply(t.outreach_id, kind, at).await?;
        }
        sqlx::query("UPDATE mailbox SET checked_at = $2 WHERE user_id = $1")
            .bind(sender)
            .bind(reached.unwrap_or(started))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Stop the rest and flag it on Today. A real reply moves the person to
    /// Replied; an automatic reply or a bounce leaves them as they were.
    async fn record_reply(
        &self,
        outreach_id: Uuid,
        kind: ReplyKind,
        at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let row: Option<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "UPDATE outreach SET reply_kind = $2, replied_at = $3,
                    stop_reason = CASE WHEN status IN ('done', 'stopped') THEN stop_reason ELSE $4 END,
                    status = CASE WHEN status = 'done' THEN status ELSE 'stopped' END,
                    version = version + 1, updated_at = now()
             WHERE id = $1 AND reply_kind IS NULL
             RETURNING org_id, candidacy_id, sender_id",
        )
        .bind(outreach_id)
        .bind(kind.as_str())
        .bind(at)
        .bind(kind.stop_reason())
        .fetch_optional(&mut *tx)
        .await?;
        let Some((org_id, candidacy, sender)) = row else {
            return Ok(());
        };
        if kind == ReplyKind::Reply {
            sqlx::query(
                "UPDATE candidacy SET state = 'replied', version = version + 1
                 WHERE id = $1 AND state IN ('contacted', 'no_reply')",
            )
            .bind(candidacy)
            .execute(&mut *tx)
            .await?;
        }
        if kind != ReplyKind::Bounce {
            sqlx::query(
                "INSERT INTO touch (org_id, person_id, role_id, user_id, channel, direction, at)
                 SELECT c.org_id, c.person_id, c.role_id, $2, 'email', 'in', $3
                 FROM candidacy c WHERE c.id = $1",
            )
            .bind(candidacy)
            .bind(sender)
            .bind(at)
            .execute(&mut *tx)
            .await?;
        }
        audit::record(
            &mut *tx,
            org_id,
            None,
            audit::action::OUTREACH_REPLY,
            &format!("candidacy:{candidacy} kind:{}", kind.as_str()),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// Record a step as sent: the sequence starts, or ends after the third.
async fn finish_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    step_id: Uuid,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let row: Option<(i32, Uuid, Uuid, Uuid, Uuid, Option<String>)> = sqlx::query_as(
        "UPDATE outreach_step s SET sent_at = $2, sending_since = NULL, retry_after = NULL
         FROM outreach o WHERE s.id = $1 AND o.id = s.outreach_id AND s.sent_at IS NULL
         RETURNING s.step, o.id, o.org_id, o.candidacy_id, o.sender_id, s.message_id",
    )
    .bind(step_id)
    .bind(at)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((step, outreach_id, org_id, candidacy, sender, message_id)) = row else {
        return Ok(());
    };
    let last: i32 =
        sqlx::query_scalar("SELECT max(step) FROM outreach_step WHERE outreach_id = $1")
            .bind(outreach_id)
            .fetch_one(&mut **tx)
            .await?;
    sqlx::query(
        "UPDATE outreach SET status = CASE WHEN $2 THEN 'done'::outreach_status
                                           ELSE 'active'::outreach_status END,
                version = version + 1, updated_at = now()
         WHERE id = $1 AND status IN ('approved', 'active')",
    )
    .bind(outreach_id)
    .bind(step >= last)
    .execute(&mut **tx)
    .await?;
    // An email went: they have been contacted, whatever happened since.
    if step == 1 {
        sqlx::query(
            "UPDATE candidacy SET state = 'contacted', version = version + 1
             WHERE id = $1 AND state IN ('approved', 'drafted', 'shortlisted')",
        )
        .bind(candidacy)
        .execute(&mut **tx)
        .await?;
    }
    if step >= last {
        sqlx::query(
            "UPDATE candidacy SET state = 'no_reply', version = version + 1
             WHERE id = $1 AND state = 'contacted'",
        )
        .bind(candidacy)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query(
        "INSERT INTO touch (org_id, person_id, role_id, user_id, channel, direction,
                            message_id, sequence_step, at)
         SELECT c.org_id, c.person_id, c.role_id, $2, 'email', 'out', $3, $4, $5
         FROM candidacy c WHERE c.id = $1",
    )
    .bind(candidacy)
    .bind(sender)
    .bind(message_id)
    .bind(step)
    .bind(at)
    .execute(&mut **tx)
    .await?;
    audit::record(
        &mut **tx,
        org_id,
        None,
        audit::action::OUTREACH_SENT,
        &format!("candidacy:{candidacy} step:{step}"),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn working_hours_are_dubai_weekdays_eight_to_six() {
        // Thursday 2 Oct 2026.
        assert!(!in_hours(at("2026-10-02T03:59:00Z")), "07:59 Dubai");
        assert!(in_hours(at("2026-10-02T04:00:00Z")), "08:00 Dubai");
        assert!(in_hours(at("2026-10-02T13:59:00Z")), "17:59 Dubai");
        assert!(!in_hours(at("2026-10-02T14:00:00Z")), "18:00 Dubai");
        // Friday is a working day in the UAE; Saturday and Sunday are not.
        assert!(in_hours(at("2026-10-02T06:00:00Z")));
        assert!(!in_hours(at("2026-10-03T06:00:00Z")), "Saturday");
        assert!(!in_hours(at("2026-10-04T06:00:00Z")), "Sunday");
        // 23:30 UTC Sunday is 03:30 Monday in Dubai: still closed.
        assert!(!in_hours(at("2026-10-04T23:30:00Z")));
    }

    #[test]
    fn the_day_starts_at_midnight_in_dubai() {
        assert_eq!(
            dubai_day_start(at("2026-10-02T21:00:00Z")),
            at("2026-10-02T20:00:00Z"),
            "01:00 on 3 Oct in Dubai"
        );
        assert_eq!(
            dubai_day_start(at("2026-10-02T19:00:00Z")),
            at("2026-10-01T20:00:00Z")
        );
        assert_eq!(dubai_date(at("2026-10-05T21:00:00Z")), "Tue 6 Oct");
        assert_eq!(dubai_time(at("2026-10-02T05:14:00Z")), "2 Oct 2026 09:14");
    }

    fn msg(from: &str, subject: &str, conv: Option<&str>, when: &str) -> Inbound {
        Inbound {
            from: from.into(),
            subject: subject.into(),
            conversation_id: conv.map(str::to_string),
            received: at(when),
        }
    }

    #[test]
    fn replies_automatic_replies_and_bounces_are_told_apart() {
        let t = "2026-10-02T06:00:00Z";
        assert_eq!(
            classify(&msg("sam@mail.example", "Re: Role, Dubai", None, t)),
            ReplyKind::Reply
        );
        assert_eq!(
            classify(&msg(
                "sam@mail.example",
                "Automatic reply: Role, Dubai",
                None,
                t
            )),
            ReplyKind::Auto
        );
        assert_eq!(
            classify(&msg("sam@mail.example", "Out of Office", None, t)),
            ReplyKind::Auto
        );
        assert_eq!(
            classify(&msg("postmaster@mail.example", "Delivery failure", None, t)),
            ReplyKind::Bounce
        );
        assert_eq!(
            classify(&msg(
                "microsoftexchange329e71ec88ae4615bbc36ab6ce41109e@aw.example",
                "x",
                None,
                t
            )),
            ReplyKind::Bounce
        );
        assert_eq!(
            classify(&msg("x@y.example", "Undeliverable: Role, Dubai", None, t)),
            ReplyKind::Bounce
        );
    }

    #[test]
    fn a_reply_matches_its_thread_or_the_candidates_address() {
        let threads = vec![
            Thread {
                outreach_id: Uuid::from_u128(1),
                conversation_id: Some("conv-1".into()),
                to_email: "Sam@Mail.example".into(),
                first_sent: at("2026-10-01T06:00:00Z"),
            },
            Thread {
                outreach_id: Uuid::from_u128(2),
                conversation_id: Some("conv-2".into()),
                to_email: "alex@mail.example".into(),
                first_sent: at("2026-10-01T06:00:00Z"),
            },
            Thread {
                outreach_id: Uuid::from_u128(3),
                conversation_id: Some("conv-3".into()),
                to_email: "jo@mail.example".into(),
                first_sent: at("2026-10-01T06:00:00Z"),
            },
        ];
        let mail = vec![
            // The sender's own follow-up in the thread is not a reply.
            msg(
                "kai@aw.example",
                "Re: Role",
                Some("conv-1"),
                "2026-10-02T06:00:00Z",
            ),
            // From the candidate's address, in a new thread.
            msg(
                "sam@mail.example",
                "Question",
                Some("other"),
                "2026-10-02T07:00:00Z",
            ),
            // A bounce in the thread, and a later real reply: the first counts.
            msg(
                "postmaster@mail.example",
                "Undeliverable",
                Some("conv-2"),
                "2026-10-02T06:30:00Z",
            ),
            msg(
                "alex@mail.example",
                "Re: Role",
                Some("conv-2"),
                "2026-10-02T09:00:00Z",
            ),
            // From before the first email went: not a reply to it.
            msg("jo@mail.example", "Hello", None, "2026-09-30T09:00:00Z"),
        ];
        let got = match_replies("kai@aw.example", &threads, &mail);
        let got: Vec<(u128, ReplyKind)> = got
            .iter()
            .map(|(t, k, _)| (t.outreach_id.as_u128(), *k))
            .collect();
        assert_eq!(got, [(1, ReplyKind::Reply), (2, ReplyKind::Bounce)]);
    }
}
