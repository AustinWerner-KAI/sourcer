//! Each person's own Outlook (SRS F14): connecting it, and the Microsoft Graph
//! calls that send email and spot replies.
//!
//! - Delegated permissions only: Mail.Send, Mail.ReadWrite and offline_access.
//!   Sourcer acts in the connected person's mailbox and nobody else's.
//! - Mail.ReadWrite is needed so follow-ups go in the same thread
//!   (createReply). Replies are spotted from sender, subject and time only;
//!   no email text is read or kept.
//! - The refresh token is encrypted with MAIL_TOKEN_KEY before it is stored.
//!   The key lives only in deploy/.env.
//! - A person can only connect the mailbox of the account they use for
//!   Sourcer, so nobody can send from someone else's address.

use std::{collections::HashMap, sync::Mutex, time::Duration};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    app::AppState,
    audit,
    auth::{self, AuthConfig, CurrentUser},
};

pub const CALLBACK_PATH: &str = "/api/mail/callback";
pub const SCOPES: &str = "offline_access User.Read Mail.Send Mail.ReadWrite";
/// Holds the `state` of a connection in progress, so only this browser can finish it.
const CONNECT_COOKIE: &str = "sourcer_mail";
const CONNECT_MINUTES: i64 = 10;
/// Graph ids that survive a move from Drafts to Sent Items.
const IMMUTABLE: (&str, &str) = ("Prefer", "IdType=\"ImmutableId\"");
/// Most pages of new mail read in one reply check (100 a page).
const MAX_PAGES: usize = 20;
/// Shown when Microsoft no longer accepts the saved connection.
pub const RECONNECT: &str = "Outlook needs connecting again. Open Settings and connect it.";

// ---------- Encryption at rest ----------

#[derive(Clone)]
pub struct Cipher(Aes256Gcm);

impl Cipher {
    /// From 32 random bytes, base64.
    pub fn from_base64(key: &str) -> anyhow::Result<Self> {
        let bytes = STANDARD
            .decode(key.trim())
            .map_err(|_| anyhow::anyhow!("MAIL_TOKEN_KEY must be base64"))?;
        anyhow::ensure!(
            bytes.len() == 32,
            "MAIL_TOKEN_KEY must be 32 bytes (44 characters of base64)"
        );
        Ok(Self(Aes256Gcm::new_from_slice(&bytes)?))
    }

    /// A fresh nonce, then the ciphertext.
    pub fn seal(&self, plain: &str) -> Vec<u8> {
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let mut out = nonce.to_vec();
        out.extend(
            self.0
                .encrypt(Nonce::from_slice(&nonce), plain.as_bytes())
                .expect("AES-GCM encryption does not fail"),
        );
        out
    }

    pub fn open(&self, sealed: &[u8]) -> anyhow::Result<String> {
        anyhow::ensure!(sealed.len() > 12, "sealed value too short");
        let (nonce, body) = sealed.split_at(12);
        let plain = self
            .0
            .decrypt(Nonce::from_slice(nonce), body)
            .map_err(|_| anyhow::anyhow!("could not decrypt; was MAIL_TOKEN_KEY changed?"))?;
        Ok(String::from_utf8(plain)?)
    }
}

// ---------- Settings ----------

#[derive(Clone)]
pub struct MailConfig {
    pub tenant_id: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub login_base: String,
    pub graph_base: String,
    pub cipher: Cipher,
}

impl MailConfig {
    pub fn new(auth: &AuthConfig, public_url: &str, key: &str) -> anyhow::Result<Self> {
        Ok(Self {
            tenant_id: auth.tenant_id.clone(),
            client_id: auth.client_id.clone(),
            client_secret: auth.client_secret.clone(),
            redirect_uri: format!("{}{CALLBACK_PATH}", public_url.trim_end_matches('/')),
            login_base: auth.login_base.clone(),
            graph_base: auth.graph_base.clone(),
            cipher: Cipher::from_base64(key)?,
        })
    }

    fn token_url(&self) -> String {
        format!("{}/{}/oauth2/v2.0/token", self.login_base, self.tenant_id)
    }

    fn secure_cookie(&self) -> bool {
        self.redirect_uri.starts_with("https://")
    }
}

impl std::fmt::Debug for MailConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailConfig")
            .field("tenant_id", &self.tenant_id)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("redirect_uri", &self.redirect_uri)
            .field("cipher", &"<redacted>")
            .finish()
    }
}

// ---------- Errors ----------

#[derive(Debug)]
pub enum MailError {
    /// This person has not connected Outlook, or sending is not set up.
    NotConnected,
    /// Microsoft refused the saved connection; the person must connect again.
    Reconnect,
    /// Anything else: network, Graph errors. Worth trying again later.
    Other(anyhow::Error),
}

impl From<anyhow::Error> for MailError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}
impl From<reqwest::Error> for MailError {
    fn from(e: reqwest::Error) -> Self {
        Self::Other(e.into())
    }
}
impl From<sqlx::Error> for MailError {
    fn from(e: sqlx::Error) -> Self {
        Self::Other(e.into())
    }
}
impl std::fmt::Display for MailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected => write!(f, "Outlook is not connected"),
            Self::Reconnect => write!(f, "{RECONNECT}"),
            Self::Other(e) => write!(f, "{e:#}"),
        }
    }
}

// ---------- Graph client ----------

#[derive(Debug, Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub id: String,
    pub internet_message_id: Option<String>,
    pub conversation_id: Option<String>,
}

/// Mail that arrived: who from, the subject, and which thread. Never the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    pub from: String,
    pub subject: String,
    pub conversation_id: Option<String>,
    pub received: chrono::DateTime<chrono::Utc>,
}

pub struct Mail {
    pub cfg: Option<MailConfig>,
    http: reqwest::Client,
    /// Access tokens by person, with when they stop working.
    tokens: Mutex<HashMap<Uuid, (String, std::time::Instant)>>,
}

impl Mail {
    pub fn new(cfg: Option<MailConfig>) -> Self {
        Self {
            cfg,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("HTTP client"),
            tokens: Mutex::new(HashMap::new()),
        }
    }

    pub fn configured(&self) -> bool {
        self.cfg.is_some()
    }

    fn cfg(&self) -> Result<&MailConfig, MailError> {
        self.cfg.as_ref().ok_or(MailError::NotConnected)
    }

    /// A working access token for this person's mailbox, refreshing if needed.
    /// Microsoft sends a new refresh token each time; it replaces the old one.
    pub async fn access_token(&self, pool: &PgPool, user_id: Uuid) -> Result<String, MailError> {
        let cfg = self.cfg()?;
        if let Some((t, until)) = self.tokens.lock().unwrap().get(&user_id) {
            if *until > std::time::Instant::now() {
                return Ok(t.clone());
            }
        }
        let row: Option<(Vec<u8>, Option<String>)> =
            sqlx::query_as("SELECT refresh_token, broken FROM mailbox WHERE user_id = $1")
                .bind(user_id)
                .fetch_optional(pool)
                .await?;
        let Some((sealed, broken)) = row else {
            return Err(MailError::NotConnected);
        };
        if broken.is_some() {
            return Err(MailError::Reconnect);
        }
        let refresh = cfg.cipher.open(&sealed)?;
        let res = self
            .http
            .post(cfg.token_url())
            .form(&[
                ("client_id", cfg.client_id.as_str()),
                ("client_secret", cfg.client_secret.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.as_str()),
                ("scope", SCOPES),
            ])
            .send()
            .await?;
        if res.status() == reqwest::StatusCode::BAD_REQUEST
            || res.status() == reqwest::StatusCode::UNAUTHORIZED
        {
            let body: Value = res.json().await.unwrap_or_default();
            let code = body["error"].as_str().unwrap_or_default().to_string();
            // Only invalid_grant is about this person's connection (revoked,
            // expired, password changed, consent removed). Anything else, such
            // as an expired app secret, is the server's setup: nobody's
            // connection is marked broken for it.
            if code != "invalid_grant" {
                return Err(MailError::Other(anyhow::anyhow!(
                    "Microsoft refused the token request: {code}"
                )));
            }
            tracing::warn!(user = %user_id, "Outlook connection refused");
            sqlx::query("UPDATE mailbox SET broken = $2 WHERE user_id = $1")
                .bind(user_id)
                .bind(RECONNECT)
                .execute(pool)
                .await?;
            self.forget(user_id);
            return Err(MailError::Reconnect);
        }
        let t: Tokens = res.error_for_status()?.json().await?;
        if let Some(next) = &t.refresh_token {
            sqlx::query("UPDATE mailbox SET refresh_token = $2 WHERE user_id = $1")
                .bind(user_id)
                .bind(cfg.cipher.seal(next))
                .execute(pool)
                .await?;
        }
        // Five minutes' margin before Microsoft's expiry.
        let life = t.expires_in.unwrap_or(3600).saturating_sub(300).max(60);
        self.tokens.lock().unwrap().insert(
            user_id,
            (
                t.access_token.clone(),
                std::time::Instant::now() + Duration::from_secs(life),
            ),
        );
        Ok(t.access_token)
    }

    pub fn forget(&self, user_id: Uuid) {
        self.tokens.lock().unwrap().remove(&user_id);
    }

    fn url(&self, path: &str) -> Result<String, MailError> {
        Ok(format!("{}/v1.0{path}", self.cfg()?.graph_base))
    }

    /// Graph refused the token itself: drop it so the next call refreshes.
    fn check(&self, user_id: Uuid, res: reqwest::Response) -> Result<reqwest::Response, MailError> {
        if res.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.forget(user_id);
        }
        Ok(res.error_for_status()?)
    }

    /// A new draft to one person, in HTML.
    pub async fn create(
        &self,
        user_id: Uuid,
        token: &str,
        to: &str,
        subject: &str,
        html: &str,
    ) -> Result<Created, MailError> {
        let res = self
            .http
            .post(self.url("/me/messages")?)
            .bearer_auth(token)
            .header(IMMUTABLE.0, IMMUTABLE.1)
            .json(&json!({
                "subject": subject,
                "body": {"contentType": "HTML", "content": html},
                "toRecipients": [{"emailAddress": {"address": to}}],
            }))
            .send()
            .await?;
        created(self.check(user_id, res)?.json().await?)
    }

    /// A draft reply to an email already sent, so it lands in the same thread,
    /// then addressed to the candidate with this step's subject and text.
    pub async fn create_reply(
        &self,
        user_id: Uuid,
        token: &str,
        to_id: &str,
        to: &str,
        subject: &str,
        html: &str,
    ) -> Result<Created, MailError> {
        let res = self
            .http
            .post(self.url(&format!("/me/messages/{}/createReply", enc(to_id)))?)
            .bearer_auth(token)
            .header(IMMUTABLE.0, IMMUTABLE.1)
            .json(&json!({}))
            .send()
            .await?;
        let draft = created(self.check(user_id, res)?.json().await?)?;
        let res = self
            .http
            .patch(self.url(&format!("/me/messages/{}", enc(&draft.id)))?)
            .bearer_auth(token)
            .header(IMMUTABLE.0, IMMUTABLE.1)
            .json(&json!({
                "subject": subject,
                "body": {"contentType": "HTML", "content": html},
                "toRecipients": [{"emailAddress": {"address": to}}],
                "ccRecipients": [],
                "bccRecipients": [],
            }))
            .send()
            .await
            .map_err(MailError::from)
            .and_then(|r| self.check(user_id, r));
        if let Err(e) = res {
            // Never leave a half-made draft addressed to anyone.
            let _ = self.delete(user_id, token, &draft.id).await;
            return Err(e);
        }
        Ok(draft)
    }

    pub async fn send(&self, user_id: Uuid, token: &str, id: &str) -> Result<(), MailError> {
        let res = self
            .http
            .post(self.url(&format!("/me/messages/{}/send", enc(id)))?)
            .bearer_auth(token)
            .header(IMMUTABLE.0, IMMUTABLE.1)
            .header(header::CONTENT_LENGTH, "0")
            .send()
            .await?;
        self.check(user_id, res)?;
        Ok(())
    }

    pub async fn delete(&self, user_id: Uuid, token: &str, id: &str) -> Result<(), MailError> {
        let res = self
            .http
            .delete(self.url(&format!("/me/messages/{}", enc(id)))?)
            .bearer_auth(token)
            .header(IMMUTABLE.0, IMMUTABLE.1)
            .send()
            .await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        self.check(user_id, res)?;
        Ok(())
    }

    /// Whether a message is still a draft, has been sent, or is gone.
    pub async fn sent_state(
        &self,
        user_id: Uuid,
        token: &str,
        id: &str,
    ) -> Result<SentState, MailError> {
        let res = self
            .http
            .get(self.url(&format!(
                "/me/messages/{}?$select=isDraft,parentFolderId",
                enc(id)
            ))?)
            .bearer_auth(token)
            .header(IMMUTABLE.0, IMMUTABLE.1)
            .send()
            .await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(SentState::Gone);
        }
        let v: Value = self.check(user_id, res)?.json().await?;
        if v["isDraft"].as_bool() == Some(false) {
            return Ok(SentState::Sent);
        }
        // Microsoft keeps the same id through sending, but the Sent Items copy
        // only appears once it has gone. A draft is safe to send again only
        // while it is still in Drafts.
        let res = self
            .http
            .get(self.url("/me/mailFolders/drafts?$select=id")?)
            .bearer_auth(token)
            .header(IMMUTABLE.0, IMMUTABLE.1)
            .send()
            .await?;
        let drafts: Value = self.check(user_id, res)?.json().await?;
        let in_drafts = drafts["id"].as_str().is_some()
            && v["parentFolderId"].as_str() == drafts["id"].as_str();
        Ok(if in_drafts {
            SentState::Draft
        } else {
            SentState::InTransit
        })
    }

    /// Mail received since `since`, oldest first, in every folder, by sender,
    /// subject and thread only. Sourcer never asks for the text. If there is
    /// more than one read takes, the second value is how far it got.
    pub async fn received_since(
        &self,
        user_id: Uuid,
        token: &str,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<(Vec<Inbound>, Option<chrono::DateTime<chrono::Utc>>), MailError> {
        let filter = format!("receivedDateTime ge {}", since.format("%Y-%m-%dT%H:%M:%SZ"));
        let mut next = Some(
            reqwest::Url::parse_with_params(
                &self.url("/me/messages")?,
                &[
                    ("$filter", filter.as_str()),
                    (
                        "$select",
                        "from,subject,conversationId,receivedDateTime,isDraft",
                    ),
                    ("$orderby", "receivedDateTime asc"),
                    ("$top", "100"),
                ],
            )
            .map_err(anyhow::Error::from)?,
        );
        let mut out = Vec::new();
        for _ in 0..MAX_PAGES {
            let Some(url) = next.take() else { break };
            let res = self.http.get(url).bearer_auth(token).send().await?;
            let page: Value = self.check(user_id, res)?.json().await?;
            for m in page["value"].as_array().into_iter().flatten() {
                let draft = m["isDraft"].as_bool() == Some(true);
                let (Some(from), Some(at)) = (
                    m["from"]["emailAddress"]["address"].as_str(),
                    m["receivedDateTime"].as_str(),
                ) else {
                    continue;
                };
                let Ok(received) = chrono::DateTime::parse_from_rfc3339(at) else {
                    continue;
                };
                if draft {
                    continue;
                }
                out.push(Inbound {
                    from: from.trim().to_lowercase(),
                    subject: m["subject"].as_str().unwrap_or_default().to_string(),
                    conversation_id: m["conversationId"].as_str().map(str::to_string),
                    received: received.with_timezone(&chrono::Utc),
                });
            }
            next = page["@odata.nextLink"]
                .as_str()
                .and_then(|u| reqwest::Url::parse(u).ok());
        }
        // Stopped at the page limit: the rest is read next time, from here.
        let reached = next.and(out.iter().map(|m| m.received).max());
        Ok((out, reached))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentState {
    /// Still in Drafts: it never went.
    Draft,
    /// Sent.
    Sent,
    /// Neither: on its way (Outbox) or moved. Wait, never send it again.
    InTransit,
    /// Not in the mailbox.
    Gone,
}

fn created(v: Value) -> Result<Created, MailError> {
    let id = v["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Graph returned no message id"))?;
    Ok(Created {
        id: id.to_string(),
        internet_message_id: v["internetMessageId"].as_str().map(str::to_string),
        conversation_id: v["conversationId"].as_str().map(str::to_string),
    })
}

/// Graph ids can hold '/', '+' and '='; they go in the path escaped.
fn enc(id: &str) -> String {
    id.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---------- Status ----------

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../web/src/api/types/")]
pub struct MailStatus {
    /// The server has what it needs to send (sign-in and MAIL_TOKEN_KEY).
    pub configured: bool,
    pub connected: bool,
    /// The connected mailbox.
    pub address: Option<String>,
    /// Why sending from it stopped, in words.
    pub broken: Option<String>,
    /// First emails sent today (Dubai) from this mailbox, and the daily limit.
    pub first_emails_today: i32,
    pub first_emails_per_day: i32,
    /// Where Microsoft must send people back, for the Entra setup steps.
    pub redirect_uri: String,
}

type StatusRow = (Option<String>, Option<String>, bool, i64, i32);

/// GET /api/mail: this person's Outlook connection.
pub async fn status(State(state): State<AppState>, user: CurrentUser) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let row: Result<StatusRow, _> = sqlx::query_as(
        "SELECT m.address, m.broken, m.user_id IS NOT NULL,
                (SELECT count(*) FROM outreach_step s JOIN outreach o ON o.id = s.outreach_id
                 WHERE o.sender_id = $1 AND s.step = 1 AND s.sent_at >= $3),
                g.first_emails_per_day
         FROM org g LEFT JOIN mailbox m ON m.user_id = $1
         WHERE g.id = $2",
    )
    .bind(user.id)
    .bind(user.org_id)
    .bind(crate::sending::dubai_day_start(chrono::Utc::now()))
    .fetch_one(pool)
    .await;
    let redirect_uri = state
        .mail
        .cfg
        .as_ref()
        .map(|c| c.redirect_uri.clone())
        .unwrap_or_default();
    match row {
        Ok((address, broken, connected, today, cap)) => Json(MailStatus {
            configured: state.mail.configured(),
            connected,
            address,
            broken,
            first_emails_today: today as i32,
            first_emails_per_day: cap,
            redirect_uri,
        })
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "mail status failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Where a connection attempt ends, so Settings can explain it.
fn back(outcome: &str) -> Response {
    Redirect::to(&format!("/settings?outlook={outcome}")).into_response()
}

/// GET /api/mail/connect: send the browser to Microsoft to connect Outlook.
pub async fn connect(State(state): State<AppState>, user: CurrentUser) -> Response {
    let (Some(pool), Some(cfg)) = (state.pool.as_ref(), state.mail.cfg.as_ref()) else {
        return back("not-set-up");
    };
    let (st, verifier) = (auth::random_token(), auth::random_token());
    let saved = sqlx::query(
        "WITH old AS (DELETE FROM mail_connect WHERE created_at < now() - make_interval(mins => $4))
         INSERT INTO mail_connect (state, verifier, user_id) VALUES ($1, $2, $3)",
    )
    .bind(&st)
    .bind(&verifier)
    .bind(user.id)
    .bind(CONNECT_MINUTES as i32)
    .execute(pool)
    .await;
    if saved.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let url = reqwest::Url::parse_with_params(
        &format!("{}/{}/oauth2/v2.0/authorize", cfg.login_base, cfg.tenant_id),
        &[
            ("client_id", cfg.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", cfg.redirect_uri.as_str()),
            ("response_mode", "query"),
            ("scope", SCOPES),
            ("state", st.as_str()),
            ("code_challenge", auth::sha256_b64(&verifier).as_str()),
            ("code_challenge_method", "S256"),
            ("login_hint", user.email.as_str()),
        ],
    )
    .expect("valid authorize URL");
    let mut res = Redirect::to(url.as_str()).into_response();
    res.headers_mut().append(
        header::SET_COOKIE,
        auth::cookie(
            CONNECT_COOKIE,
            &st,
            CALLBACK_PATH,
            CONNECT_MINUTES * 60,
            cfg.secure_cookie(),
        ),
    );
    res
}

#[derive(Deserialize)]
pub struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphMe {
    mail: Option<String>,
    user_principal_name: Option<String>,
}

/// GET /api/mail/callback: Microsoft sends the browser back here.
pub async fn callback(
    State(state): State<AppState>,
    user: Option<CurrentUser>,
    headers: HeaderMap,
    Query(q): Query<Callback>,
) -> Response {
    let (Some(pool), Some(cfg)) = (state.pool.as_ref(), state.mail.cfg.as_ref()) else {
        return back("not-set-up");
    };
    let mut res = match user {
        Some(user) => finish_connect(&state, pool, cfg, &user, &headers, q).await,
        None => back("signed-out"),
    };
    res.headers_mut().append(
        header::SET_COOKIE,
        auth::cookie(CONNECT_COOKIE, "", CALLBACK_PATH, 0, cfg.secure_cookie()),
    );
    res
}

async fn finish_connect(
    state: &AppState,
    pool: &PgPool,
    cfg: &MailConfig,
    user: &CurrentUser,
    headers: &HeaderMap,
    q: Callback,
) -> Response {
    if q.error.is_some() {
        return back("cancelled");
    }
    let (Some(code), Some(st)) = (q.code, q.state) else {
        return back("failed");
    };
    if auth::read_cookie(headers, CONNECT_COOKIE).as_deref() != Some(st.as_str()) {
        return back("failed");
    }
    // One use only, within the time limit, and only by the person who started it.
    let verifier: Option<String> = sqlx::query_scalar(
        "DELETE FROM mail_connect
         WHERE state = $1 AND user_id = $2 AND created_at > now() - make_interval(mins => $3)
         RETURNING verifier",
    )
    .bind(&st)
    .bind(user.id)
    .bind(CONNECT_MINUTES as i32)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);
    let Some(verifier) = verifier else {
        return back("expired");
    };
    let http = &state.mail.http;
    let exchanged = async {
        let t: Tokens = http
            .post(cfg.token_url())
            .form(&[
                ("client_id", cfg.client_id.as_str()),
                ("client_secret", cfg.client_secret.as_str()),
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", cfg.redirect_uri.as_str()),
                ("code_verifier", verifier.as_str()),
                ("scope", SCOPES),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let me: GraphMe = http
            .get(format!("{}/v1.0/me", cfg.graph_base))
            .bearer_auth(&t.access_token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        anyhow::Ok((t, me))
    }
    .await;
    let (tokens, me) = match exchanged {
        Ok(x) => x,
        Err(e) => {
            tracing::warn!(error = %e, "Outlook connection failed");
            return back("failed");
        }
    };
    let Some(refresh) = tokens.refresh_token else {
        return back("no-offline");
    };
    let address = me
        .mail
        .or(me.user_principal_name)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if address != user.email.trim().to_lowercase() {
        return back("other-account");
    }
    let saved = async {
        let mut tx = pool.begin().await?;
        sqlx::query(
            "INSERT INTO mailbox (user_id, org_id, address, refresh_token, checked_at)
             VALUES ($1, $2, $3, $4, now())
             ON CONFLICT (user_id) DO UPDATE SET address = $3, refresh_token = $4,
                    connected_at = now(), broken = NULL,
                    checked_at = coalesce(mailbox.checked_at, now())",
        )
        .bind(user.id)
        .bind(user.org_id)
        .bind(&address)
        .bind(cfg.cipher.seal(&refresh))
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::MAIL_CONNECTED,
            &format!("user:{}", user.id),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(())
    }
    .await;
    state.mail.forget(user.id);
    match saved {
        Ok(()) => back("connected"),
        Err(e) => {
            tracing::error!(error = %e, "could not save the Outlook connection");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// DELETE /api/mail: stop sending from this person's Outlook. Approved emails
/// wait until it is connected again.
pub async fn disconnect(State(state): State<AppState>, user: CurrentUser) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = async {
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM mailbox WHERE user_id = $1")
            .bind(user.id)
            .execute(&mut *tx)
            .await?;
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::MAIL_DISCONNECTED,
            &format!("user:{}", user.id),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(())
    }
    .await;
    state.mail.forget(user.id);
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "could not disconnect Outlook");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> String {
        STANDARD.encode([7u8; 32])
    }

    #[test]
    fn tokens_are_sealed_and_opened() {
        let c = Cipher::from_base64(&key()).unwrap();
        let a = c.seal("refresh-token");
        let b = c.seal("refresh-token");
        assert_ne!(a, b, "a fresh nonce each time");
        assert!(!a.windows(7).any(|w| w == b"refresh"), "not readable");
        assert_eq!(c.open(&a).unwrap(), "refresh-token");
        let other = Cipher::from_base64(&STANDARD.encode([8u8; 32])).unwrap();
        assert!(other.open(&a).is_err(), "another key cannot read it");
        let mut tampered = a.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(c.open(&tampered).is_err(), "tampering is caught");
    }

    #[test]
    fn the_key_must_be_32_bytes_of_base64() {
        assert!(Cipher::from_base64("not base64!").is_err());
        assert!(Cipher::from_base64(&STANDARD.encode([1u8; 16])).is_err());
        assert!(Cipher::from_base64(&key()).is_ok());
    }

    #[test]
    fn ids_are_escaped_for_the_path() {
        assert_eq!(enc("AAMk/a+b=="), "AAMk%2Fa%2Bb%3D%3D");
        assert_eq!(enc("plain-id_1.x"), "plain-id_1.x");
    }

    #[test]
    fn secrets_are_never_printed() {
        let auth = AuthConfig::new(
            "t".into(),
            "c".into(),
            "very-secret".into(),
            "http://localhost:8080",
            None,
        );
        let cfg = MailConfig::new(&auth, "http://localhost:8080/", &key()).unwrap();
        assert_eq!(cfg.redirect_uri, "http://localhost:8080/api/mail/callback");
        let out = format!("{cfg:?}");
        assert!(!out.contains("very-secret"));
    }
}
