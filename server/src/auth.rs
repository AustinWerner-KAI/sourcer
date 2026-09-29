//! Sign in with Microsoft 365 (SRS F1, N3).
//!
//! Authorization code flow with PKCE against the Austin Werner tenant only.
//! Only invited users get in; the admin named in `ADMIN_EMAIL` is created on
//! their first sign-in. Sessions are an HttpOnly cookie whose hash is stored.

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
const SESSION_DAYS: i64 = 7;
const SCOPES: &str = "openid profile email User.Read";

#[derive(Clone)]
pub struct AuthConfig {
    pub tenant_id: String,
    pub client_id: String,
    pub client_secret: String,
    /// Where Microsoft sends the user back, e.g. `http://localhost:8080/api/auth/callback`.
    pub redirect_uri: String,
    /// Lower-cased. Created as admin on first sign-in.
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
            redirect_uri: format!("{}/api/auth/callback", public_url.trim_end_matches('/')),
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

/// Where a failed sign-in sends the browser, so the app can explain it.
fn failed(reason: &str) -> Response {
    Redirect::to(&format!("/?signin={reason}")).into_response()
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn sha256_b64(s: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(s.as_bytes()))
}

fn parts(state: &AppState) -> Option<(&PgPool, &AuthConfig)> {
    Some((state.pool.as_ref()?, state.auth.as_deref()?))
}

fn not_configured() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, "sign-in is not configured").into_response()
}

/// GET /api/auth/login: send the browser to Microsoft.
pub async fn login(State(state): State<AppState>) -> Response {
    let Some((pool, cfg)) = parts(&state) else {
        return not_configured();
    };
    let (st, verifier) = (random_token(), random_token());
    let saved = sqlx::query(
        "WITH gone AS (DELETE FROM oauth_state WHERE created_at < now() - interval '10 minutes')
         INSERT INTO oauth_state (state, verifier) VALUES ($1, $2)",
    )
    .bind(&st)
    .bind(&verifier)
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
    Redirect::to(url.as_str()).into_response()
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
pub async fn callback(State(state): State<AppState>, Query(q): Query<Callback>) -> Response {
    let Some((pool, cfg)) = parts(&state) else {
        return not_configured();
    };
    if q.error.is_some() {
        return failed("cancelled");
    }
    let (Some(code), Some(st)) = (q.code, q.state) else {
        return failed("invalid");
    };
    // One use only, and only within 10 minutes.
    let verifier: Option<String> = sqlx::query_scalar(
        "DELETE FROM oauth_state WHERE state = $1 AND created_at > now() - interval '10 minutes'
         RETURNING verifier",
    )
    .bind(&st)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);
    let Some(verifier) = verifier else {
        return failed("expired");
    };

    let me = match fetch_identity(&state.http, cfg, &code, &verifier).await {
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

    match find_or_create_user(pool, cfg, &me.id, &email, &name).await {
        Ok(Some((user_id, org_id))) => match start_session(pool, org_id, user_id).await {
            Ok(token) => {
                let cookie = format!(
                    "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
                    SESSION_DAYS * 86_400,
                    if cfg.secure_cookie() { "; Secure" } else { "" }
                );
                let mut res = Redirect::to("/").into_response();
                res.headers_mut().insert(
                    header::SET_COOKIE,
                    HeaderValue::from_str(&cookie).expect("cookie is ASCII"),
                );
                res
            }
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Ok(None) => failed("not-invited"),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
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

/// Known user by Microsoft id or email; the configured admin on first sign-in;
/// otherwise `None` (not invited).
async fn find_or_create_user(
    pool: &PgPool,
    cfg: &AuthConfig,
    ms_oid: &str,
    email: &str,
    name: &str,
) -> anyhow::Result<Option<(Uuid, Uuid)>> {
    let existing: Option<(Uuid, Uuid)> = sqlx::query_as(
        "UPDATE app_user SET ms_oid = $1, name = $3
         WHERE id = (SELECT id FROM app_user WHERE ms_oid = $1 OR lower(email) = $2
                     ORDER BY (ms_oid = $1) DESC NULLS LAST LIMIT 1)
         RETURNING id, org_id",
    )
    .bind(ms_oid)
    .bind(email)
    .bind(name)
    .fetch_optional(pool)
    .await?;
    if existing.is_some() {
        return Ok(existing);
    }
    if email.is_empty() || cfg.admin_email.as_deref() != Some(email) {
        return Ok(None);
    }
    let mut tx = pool.begin().await?;
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
    Ok(Some((user_id, org_id)))
}

async fn start_session(pool: &PgPool, org_id: Uuid, user_id: Uuid) -> anyhow::Result<String> {
    let token = random_token();
    sqlx::query(
        "INSERT INTO user_session (token_hash, org_id, user_id, expires_at)
         VALUES ($1, $2, $3, now() + make_interval(days => $4))",
    )
    .bind(sha256_b64(&token))
    .bind(org_id)
    .bind(user_id)
    .bind(SESSION_DAYS as i32)
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

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_string())
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
        let token = session_cookie(&parts.headers).ok_or(StatusCode::UNAUTHORIZED)?;
        let row: Option<(Uuid, Uuid, String, String, String)> = sqlx::query_as(
            "SELECT u.id, u.org_id, u.email, u.name, u.role::text
             FROM user_session s JOIN app_user u ON u.id = s.user_id
             WHERE s.token_hash = $1 AND s.expires_at > now()",
        )
        .bind(sha256_b64(&token))
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
pub async fn me(user: CurrentUser) -> Json<Me> {
    Json(Me {
        id: user.id,
        name: user.name,
        email: user.email,
        role: user.role,
    })
}

/// POST /api/auth/logout: end this session.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let (Some(pool), Some(token)) = (&state.pool, session_cookie(&headers)) {
        let _ = sqlx::query("DELETE FROM user_session WHERE token_hash = $1")
            .bind(sha256_b64(&token))
            .execute(pool)
            .await;
    }
    let clear = format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    (
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            HeaderValue::from_str(&clear).expect("ASCII"),
        )],
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
    fn reads_the_session_cookie_among_others() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; sourcer_session=abc; b=2"),
        );
        assert_eq!(session_cookie(&h).as_deref(), Some("abc"));
        assert_eq!(session_cookie(&HeaderMap::new()), None);
    }

    #[test]
    fn secret_is_never_printed() {
        let c = AuthConfig::new("t".into(), "c".into(), "s3cret".into(), "http://x", None);
        assert!(!format!("{c:?}").contains("s3cret"));
        assert_eq!(c.redirect_uri, "http://x/api/auth/callback");
        assert!(!c.secure_cookie());
    }
}
