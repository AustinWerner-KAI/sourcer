//! Sign in with Microsoft 365 (SRS F1, N3).
//!
//! Authorization code flow with PKCE against the Austin Werner tenant only.
//! - The sign-in is tied to the browser that started it (login CSRF).
//! - Only invited users get in. Once a user has signed in, only that Microsoft
//!   account can use the user; a reused email address never inherits it.
//! - The admin named in `ADMIN_EMAIL` is created on first sign-in, and only
//!   while no admin exists.
//! - Sessions are an HttpOnly cookie whose hash is stored. They end after 12
//!   hours idle or 7 days in total, or at once when the user is disabled.

use axum::{
    extract::{FromRequestParts, Query, State},
    http::{header, request::Parts, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{app::AppState, audit, domain::Me};

pub const COOKIE: &str = "sourcer_session";
/// Holds the `state` of a sign-in in progress, so only this browser can finish it.
const LOGIN_COOKIE: &str = "sourcer_oauth";
const CALLBACK_PATH: &str = "/api/auth/callback";
const IDLE_HOURS: i32 = 12;
const MAX_DAYS: i32 = 7;
const LOGIN_MINUTES: i64 = 10;
const SCOPES: &str = "openid profile email User.Read";

#[derive(Clone)]
pub struct AuthConfig {
    pub tenant_id: String,
    pub client_id: String,
    pub client_secret: String,
    /// Where Microsoft sends the user back, e.g. `http://localhost:8080/api/auth/callback`.
    pub redirect_uri: String,
    /// Lower-cased. Created as admin on first sign-in while no admin exists.
    pub admin_email: Option<String>,
    pub login_base: String,
    pub graph_base: String,
}

impl AuthConfig {
    pub fn new(
        tenant_id: String,
        client_id: String,
        client_secret: String,
        public_url: &str,
        admin_email: Option<String>,
    ) -> Self {
        Self {
            tenant_id,
            client_id,
            client_secret,
            redirect_uri: format!("{}{CALLBACK_PATH}", public_url.trim_end_matches('/')),
            admin_email: admin_email.map(|e| e.trim().to_lowercase()),
            login_base: "https://login.microsoftonline.com".into(),
            graph_base: "https://graph.microsoft.com".into(),
        }
    }

    fn secure_cookie(&self) -> bool {
        self.redirect_uri.starts_with("https://")
    }
}

impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthConfig")
            .field("tenant_id", &self.tenant_id)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("redirect_uri", &self.redirect_uri)
            .finish()
    }
}

/// One place that builds every cookie, so set and clear always match.
pub(crate) fn cookie(
    name: &str,
    value: &str,
    path: &str,
    max_age: i64,
    secure: bool,
) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    let c =
        format!("{name}={value}; Path={path}; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}");
    HeaderValue::from_str(&c).expect("cookie is ASCII")
}

/// Where a failed sign-in sends the browser, so the app can explain it.
fn failed(reason: &str) -> Response {
    Redirect::to(&format!("/?signin={reason}")).into_response()
}

pub(crate) fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn sha256_b64(s: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(s.as_bytes()))
}

fn parts(state: &AppState) -> Option<(&PgPool, &AuthConfig)> {
    Some((state.pool.as_ref()?, state.auth.as_deref()?))
}

fn not_configured() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, "sign-in is not configured").into_response()
}

pub(crate) fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

/// GET /api/auth/login: send the browser to Microsoft.
pub async fn login(State(state): State<AppState>) -> Response {
    let Some((pool, cfg)) = parts(&state) else {
        return not_configured();
    };
    let (st, verifier) = (random_token(), random_token());
    // Save this sign-in, and clear out stale sign-ins and ended sessions.
    let saved = sqlx::query(
        "WITH old_logins AS (DELETE FROM oauth_state WHERE created_at < now() - make_interval(mins => $3)),
              old_sessions AS (DELETE FROM user_session WHERE expires_at < now() OR absolute_expires_at < now())
         INSERT INTO oauth_state (state, verifier) VALUES ($1, $2)",
    )
    .bind(&st)
    .bind(&verifier)
    .bind(LOGIN_MINUTES as i32)
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
            ("code_challenge", sha256_b64(&verifier).as_str()),
            ("code_challenge_method", "S256"),
            ("prompt", "select_account"),
        ],
    )
    .expect("valid authorize URL");
    let mut res = Redirect::to(url.as_str()).into_response();
    res.headers_mut().append(
        header::SET_COOKIE,
        cookie(
            LOGIN_COOKIE,
            &st,
            CALLBACK_PATH,
            LOGIN_MINUTES * 60,
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
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphMe {
    id: String,
    display_name: Option<String>,
    mail: Option<String>,
    user_principal_name: Option<String>,
}

/// GET /api/auth/callback: Microsoft sends the browser back here.
pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<Callback>,
) -> Response {
    let Some((pool, cfg)) = parts(&state) else {
        return not_configured();
    };
    let mut res = finish_sign_in(&state.http, pool, cfg, &headers, q).await;
    // The sign-in cookie is single use, whatever the outcome.
    res.headers_mut().append(
        header::SET_COOKIE,
        cookie(LOGIN_COOKIE, "", CALLBACK_PATH, 0, cfg.secure_cookie()),
    );
    res
}

async fn finish_sign_in(
    http: &reqwest::Client,
    pool: &PgPool,
    cfg: &AuthConfig,
    headers: &HeaderMap,
    q: Callback,
) -> Response {
    if q.error.is_some() {
        return failed("cancelled");
    }
    let (Some(code), Some(st)) = (q.code, q.state) else {
        return failed("invalid");
    };
    // Only the browser that started this sign-in may finish it.
    if read_cookie(headers, LOGIN_COOKIE).as_deref() != Some(st.as_str()) {
        return failed("browser");
    }
    // One use only, and only within the time limit.
    let verifier: Option<String> = sqlx::query_scalar(
        "DELETE FROM oauth_state
         WHERE state = $1 AND created_at > now() - make_interval(mins => $2)
         RETURNING verifier",
    )
    .bind(&st)
    .bind(LOGIN_MINUTES as i32)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);
    let Some(verifier) = verifier else {
        return failed("expired");
    };

    let me = match fetch_identity(http, cfg, &code, &verifier).await {
        Ok(me) => me,
        Err(e) => {
            tracing::warn!(error = %e, "sign-in exchange failed");
            return failed("microsoft");
        }
    };
    let email = me
        .mail
        .or(me.user_principal_name)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let name = me.display_name.unwrap_or_else(|| email.clone());

    let (user_id, org_id) = match resolve_user(pool, cfg, &me.id, &email, &name).await {
        Ok(Access::Granted { user_id, org_id }) => (user_id, org_id),
        Ok(Access::NotInvited) => return failed("not-invited"),
        Ok(Access::Disabled) => return failed("disabled"),
        Ok(Access::OtherAccount) => return failed("mismatch"),
        Err(e) => {
            tracing::error!(error = %e, "sign-in lookup failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    match start_session(pool, org_id, user_id).await {
        Ok(token) => {
            let mut res = Redirect::to("/").into_response();
            res.headers_mut().append(
                header::SET_COOKIE,
                cookie(
                    COOKIE,
                    &token,
                    "/",
                    i64::from(MAX_DAYS) * 86_400,
                    cfg.secure_cookie(),
                ),
            );
            res
        }
        Err(e) => {
            tracing::error!(error = %e, "could not start session");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Exchange the code for a token, then ask Microsoft Graph who signed in.
/// The token comes straight from Microsoft over TLS and is used once here.
async fn fetch_identity(
    http: &reqwest::Client,
    cfg: &AuthConfig,
    code: &str,
    verifier: &str,
) -> anyhow::Result<GraphMe> {
    let token: TokenResponse = http
        .post(format!(
            "{}/{}/oauth2/v2.0/token",
            cfg.login_base, cfg.tenant_id
        ))
        .form(&[
            ("client_id", cfg.client_id.as_str()),
            ("client_secret", cfg.client_secret.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", cfg.redirect_uri.as_str()),
            ("code_verifier", verifier),
            ("scope", SCOPES),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let me = http
        .get(format!("{}/v1.0/me", cfg.graph_base))
        .bearer_auth(&token.access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(me)
}

#[derive(Debug, PartialEq, Eq)]
enum Access {
    Granted {
        user_id: Uuid,
        org_id: Uuid,
    },
    NotInvited,
    Disabled,
    /// The email belongs to a user already linked to a different Microsoft account.
    OtherAccount,
}

/// Decide who this Microsoft account is in Sourcer.
async fn resolve_user(
    pool: &PgPool,
    cfg: &AuthConfig,
    ms_oid: &str,
    email: &str,
    name: &str,
) -> anyhow::Result<Access> {
    let row: Option<(Uuid, Uuid, Option<String>, bool)> = sqlx::query_as(
        "SELECT id, org_id, ms_oid, disabled_at IS NOT NULL FROM app_user
         WHERE ms_oid = $1 OR (lower(email) = $2 AND $2 <> '')
         ORDER BY (ms_oid = $1) DESC NULLS LAST
         LIMIT 1",
    )
    .bind(ms_oid)
    .bind(email)
    .fetch_optional(pool)
    .await?;

    if let Some((user_id, org_id, linked, disabled)) = row {
        if linked.as_deref().is_some_and(|l| l != ms_oid) {
            audit::record(
                pool,
                org_id,
                None,
                audit::action::SIGN_IN_REFUSED,
                &format!("user:{user_id}"),
            )
            .await?;
            return Ok(Access::OtherAccount);
        }
        if disabled {
            audit::record(
                pool,
                org_id,
                None,
                audit::action::SIGN_IN_REFUSED,
                &format!("user:{user_id} switched-off"),
            )
            .await?;
            return Ok(Access::Disabled);
        }
        // First sign-in links the Microsoft account; later ones refresh the name.
        // Only links while still unlinked, so two first sign-ins cannot race.
        let linked_now = sqlx::query(
            "UPDATE app_user SET ms_oid = $2, name = $3
             WHERE id = $1 AND (ms_oid IS NULL OR ms_oid = $2)",
        )
        .bind(user_id)
        .bind(ms_oid)
        .bind(name)
        .execute(pool)
        .await?;
        if linked_now.rows_affected() == 0 {
            return Ok(Access::OtherAccount);
        }
        return Ok(Access::Granted { user_id, org_id });
    }

    if email.is_empty() || cfg.admin_email.as_deref() != Some(email) {
        return Ok(Access::NotInvited);
    }
    bootstrap_admin(pool, ms_oid, email, name).await
}

/// Create the first admin. Runs only while no admin exists, one at a time.
async fn bootstrap_admin(
    pool: &PgPool,
    ms_oid: &str,
    email: &str,
    name: &str,
) -> anyhow::Result<Access> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('sourcer.admin_bootstrap'))")
        .execute(&mut *tx)
        .await?;
    let admin_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM app_user WHERE role = 'admin')")
            .fetch_one(&mut *tx)
            .await?;
    if admin_exists {
        return Ok(Access::NotInvited);
    }
    let org_id: Uuid = match sqlx::query_scalar("SELECT id FROM org ORDER BY created_at LIMIT 1")
        .fetch_optional(&mut *tx)
        .await?
    {
        Some(id) => id,
        None => {
            sqlx::query_scalar("INSERT INTO org (name) VALUES ('Austin Werner') RETURNING id")
                .fetch_one(&mut *tx)
                .await?
        }
    };
    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO app_user (org_id, email, name, role, ms_oid, mailbox_provider)
         VALUES ($1, $2, $3, 'admin', $4, 'microsoft') RETURNING id",
    )
    .bind(org_id)
    .bind(email)
    .bind(name)
    .bind(ms_oid)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Access::Granted { user_id, org_id })
}

async fn start_session(pool: &PgPool, org_id: Uuid, user_id: Uuid) -> anyhow::Result<String> {
    let token = random_token();
    sqlx::query(
        "INSERT INTO user_session (token_hash, org_id, user_id, expires_at, absolute_expires_at)
         VALUES ($1, $2, $3, now() + make_interval(hours => $4), now() + make_interval(days => $5))",
    )
    .bind(sha256_b64(&token))
    .bind(org_id)
    .bind(user_id)
    .bind(IDLE_HOURS)
    .bind(MAX_DAYS)
    .execute(pool)
    .await?;
    audit::record(
        pool,
        org_id,
        Some(user_id),
        audit::action::SIGN_IN,
        &format!("user:{user_id}"),
    )
    .await?;
    Ok(token)
}

/// The signed-in user. Any handler that takes this refuses anonymous requests.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub id: Uuid,
    pub org_id: Uuid,
    pub email: String,
    pub name: String,
    pub role: String,
}

#[axum::async_trait]
impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, StatusCode> {
        let pool = state.pool.as_ref().ok_or(StatusCode::UNAUTHORIZED)?;
        let token = read_cookie(&parts.headers, COOKIE).ok_or(StatusCode::UNAUTHORIZED)?;
        // Valid, not idle too long, within the hard cap, user not disabled.
        // Use slides the idle limit forward, never past the cap.
        let row: Option<(Uuid, Uuid, String, String, String)> = sqlx::query_as(
            "WITH s AS (
               UPDATE user_session
               SET expires_at = LEAST(now() + make_interval(hours => $2), absolute_expires_at)
               WHERE token_hash = $1 AND expires_at > now() AND absolute_expires_at > now()
               RETURNING user_id)
             SELECT u.id, u.org_id, u.email, u.name, u.role::text
             FROM s JOIN app_user u ON u.id = s.user_id
             WHERE u.disabled_at IS NULL",
        )
        .bind(sha256_b64(&token))
        .bind(IDLE_HOURS)
        .fetch_optional(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let (id, org_id, email, name, role) = row.ok_or(StatusCode::UNAUTHORIZED)?;
        Ok(CurrentUser {
            id,
            org_id,
            email,
            name,
            role,
        })
    }
}

/// GET /api/me: who is signed in.
pub async fn me(State(state): State<AppState>, user: CurrentUser) -> Json<Me> {
    let recruitly_user_id = match &state.pool {
        Some(pool) => sqlx::query_scalar::<_, Option<String>>(
            "SELECT recruitly_user_id FROM app_user WHERE id = $1 AND org_id = $2",
        )
        .bind(user.id)
        .bind(user.org_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .flatten(),
        None => None,
    };
    Json(Me {
        id: user.id,
        name: user.name,
        email: user.email,
        role: user.role,
        recruitly_user_id,
    })
}

/// POST /api/auth/logout: end this session.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let (Some(pool), Some(token)) = (&state.pool, read_cookie(&headers, COOKIE)) {
        let deleted = sqlx::query("DELETE FROM user_session WHERE token_hash = $1")
            .bind(sha256_b64(&token))
            .execute(pool)
            .await;
        if deleted.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    let secure = state.auth.as_deref().is_some_and(AuthConfig::secure_cookie);
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, cookie(COOKIE, "", "/", 0, secure))],
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_unpadded_base64url_sha256() {
        // Expected value computed independently (Python hashlib + urlsafe_b64encode).
        assert_eq!(
            sha256_b64("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWEOEjXk"),
            "TP3DZQl9rJnri9eRuZaFS6Q7czCrth7sEZC00LreApo"
        );
    }

    #[test]
    fn tokens_are_random_and_url_safe() {
        let (a, b) = (random_token(), random_token());
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn reads_the_right_cookie_among_others() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; sourcer_session=abc; sourcer_oauth=xyz"),
        );
        assert_eq!(read_cookie(&h, COOKIE).as_deref(), Some("abc"));
        assert_eq!(read_cookie(&h, LOGIN_COOKIE).as_deref(), Some("xyz"));
        assert_eq!(read_cookie(&HeaderMap::new(), COOKIE), None);
    }

    #[test]
    fn cookies_carry_the_same_attributes_to_set_and_clear() {
        let set = cookie(COOKIE, "t", "/", 60, true);
        let clear = cookie(COOKIE, "", "/", 0, true);
        for c in [set, clear] {
            let c = c.to_str().unwrap().to_string();
            assert!(c.contains("HttpOnly") && c.contains("SameSite=Lax") && c.contains("Secure"));
        }
        assert!(!cookie(COOKIE, "t", "/", 60, false)
            .to_str()
            .unwrap()
            .contains("Secure"));
    }

    #[test]
    fn secret_is_never_printed() {
        let c = AuthConfig::new("t".into(), "c".into(), "s3cret".into(), "http://x", None);
        assert!(!format!("{c:?}").contains("s3cret"));
        assert_eq!(c.redirect_uri, "http://x/api/auth/callback");
        assert!(!c.secure_cookie());
    }
}
