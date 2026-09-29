use std::sync::Arc;

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use sqlx::PgPool;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

use crate::{auth, auth::AuthConfig, domain::Health};

#[derive(Clone)]
pub struct AppState {
    /// `None` only in tests that do not need a database.
    pub pool: Option<PgPool>,
    /// `None` until the Microsoft 365 app registration is configured.
    pub auth: Option<Arc<AuthConfig>>,
    pub http: reqwest::Client,
}

impl AppState {
    pub fn new(pool: Option<PgPool>, auth: Option<AuthConfig>) -> Self {
        Self {
            pool,
            auth: auth.map(Arc::new),
            http: reqwest::Client::new(),
        }
    }
}

pub fn router(state: AppState) -> Router {
    router_with_web(state, None)
}

/// API routes, plus the built web app when `web_dir` is set. Unknown paths fall
/// back to `index.html` so client-side routes like `/candidates` load.
pub fn router_with_web(state: AppState, web_dir: Option<&str>) -> Router {
    let api = Router::new()
        .route("/api/health", get(health))
        .route("/api/auth/login", get(auth::login))
        .route("/api/auth/callback", get(auth::callback))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/me", get(auth::me))
        .with_state(state);
    let app = match web_dir {
        Some(dir) => {
            let index = format!("{dir}/index.html");
            api.fallback_service(ServeDir::new(dir).not_found_service(ServeFile::new(index)))
        }
        None => api,
    };
    // Log the path only: query strings can carry sign-in codes.
    app.layer(
        TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
            tracing::info_span!("http", method = %req.method(), path = %req.uri().path())
        }),
    )
}

async fn health(State(state): State<AppState>) -> Json<Health> {
    let database = match &state.pool {
        Some(pool) => sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(pool)
            .await
            .is_ok(),
        None => false,
    };
    Json(Health {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        database,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use axum::body::Body;
    use axum::http::{header, Request, Response, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;
    use uuid::Uuid;

    async fn send(app: &Router, req: Request<Body>) -> Response<Body> {
        app.clone().oneshot(req).await.unwrap()
    }

    fn get_req(uri: &str, cookie: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().uri(uri);
        if let Some(c) = cookie {
            b = b.header(header::COOKIE, c);
        }
        b.body(Body::empty()).unwrap()
    }

    fn location(res: &Response<Body>) -> String {
        res.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_string()
    }

    /// The `name=value` part of the Set-Cookie for `name`, if any.
    fn set_cookie(res: &Response<Body>, name: &str) -> Option<String> {
        res.headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .find(|c| c.starts_with(&format!("{name}=")))
    }

    async fn json_body(res: Response<Body>) -> Value {
        let body = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn health_responds_without_db() {
        let app = router(AppState::new(None, None));
        let res = send(&app, get_req("/api/health", None)).await;
        assert_eq!(res.status(), StatusCode::OK);
        let v = json_body(res).await;
        assert_eq!(v["status"], "ok");
        assert_eq!(v["database"], false);
    }

    #[tokio::test]
    async fn me_needs_a_session() {
        let app = router(AppState::new(None, None));
        let res = send(&app, get_req("/api/me", None)).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_without_config_says_so() {
        let app = router(AppState::new(None, None));
        let res = send(&app, get_req("/api/auth/login", None)).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// A stand-in for Microsoft: the token endpoint and Graph `/me`.
    async fn fake_microsoft(email: String, oid: String) -> String {
        let app = Router::new()
            .route(
                "/tenant/oauth2/v2.0/token",
                post(|body: String| async move {
                    assert!(body.contains("code=good-code"));
                    assert!(body.contains("code_verifier="));
                    assert!(body.contains("client_secret=secret"));
                    Json(json!({"access_token": "ms-token", "token_type": "Bearer"}))
                }),
            )
            .route(
                "/v1.0/me",
                get(move |headers: axum::http::HeaderMap| async move {
                    assert_eq!(headers[header::AUTHORIZATION], "Bearer ms-token");
                    Json(json!({"id": oid, "displayName": "Test User", "mail": email}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn app_for(pool: PgPool, ms: &str, admin: &str) -> Router {
        let mut cfg = AuthConfig::new(
            "tenant".into(),
            "client".into(),
            "secret".into(),
            "http://localhost:8080",
            Some(admin.into()),
        );
        cfg.login_base = ms.into();
        cfg.graph_base = ms.into();
        router(AppState::new(Some(pool), Some(cfg)))
    }

    fn unique_email(tag: &str) -> String {
        format!("{tag}-{}@example.com", Uuid::new_v4())
    }

    /// Start a sign-in: returns the `state` and the browser cookie it set.
    async fn begin(app: &Router) -> (String, String) {
        let res = send(app, get_req("/api/auth/login", None)).await;
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        let url = reqwest::Url::parse(&location(&res)).unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["redirect_uri"], "http://localhost:8080/api/auth/callback");
        assert!(url.path().ends_with("/tenant/oauth2/v2.0/authorize"));
        let c = set_cookie(&res, "sourcer_oauth").expect("sign-in is tied to this browser");
        assert!(c.contains("Path=/api/auth/callback") && c.contains("HttpOnly"));
        let pair = c.split(';').next().unwrap().to_string();
        (q["state"].clone(), pair)
    }

    /// A full sign-in as whoever the fake Microsoft says. Returns the callback response.
    async fn sign_in(app: &Router) -> Response<Body> {
        let (state, browser) = begin(app).await;
        let cb = format!("/api/auth/callback?code=good-code&state={state}");
        send(app, get_req(&cb, Some(&browser))).await
    }

    fn session_of(res: &Response<Body>) -> String {
        let c = set_cookie(res, "sourcer_session").expect("a session cookie");
        c.split(';').next().unwrap().to_string()
    }

    async fn add_user(pool: &PgPool, email: &str, ms_oid: Option<&str>) -> Uuid {
        let org = testutil::org(pool).await;
        sqlx::query_scalar(
            "INSERT INTO app_user (org_id, email, name, role, ms_oid)
             VALUES ($1, $2, 'Invited', 'resourcer', $3) RETURNING id",
        )
        .bind(org)
        .bind(email)
        .bind(ms_oid)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn first_admin_signs_in_sees_me_and_signs_out() {
        let Some(pool) = testutil::fresh_pool().await else {
            return;
        };
        let email = unique_email("admin");
        let ms = fake_microsoft(email.clone(), Uuid::new_v4().to_string()).await;
        let app = app_for(pool.clone(), &ms, &email);

        let (state, browser) = begin(&app).await;
        let cb = format!("/api/auth/callback?code=good-code&state={state}");
        let res = send(&app, get_req(&cb, Some(&browser))).await;
        assert_eq!(location(&res), "/");
        let set = set_cookie(&res, "sourcer_session").unwrap();
        assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax"));
        let cleared = set_cookie(&res, "sourcer_oauth").unwrap();
        assert!(
            cleared.contains("Max-Age=0"),
            "sign-in cookie is single use"
        );
        let cookie = session_of(&res);

        let me = json_body(send(&app, get_req("/api/me", Some(&cookie))).await).await;
        assert_eq!(
            (me["email"].as_str(), me["role"].as_str()),
            (Some(email.as_str()), Some("admin"))
        );

        let audited: i64 =
            sqlx::query_scalar("SELECT count(*) FROM audit WHERE action = 'auth.sign_in'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(audited, 1);

        // The same state cannot be used twice.
        let again = send(&app, get_req(&cb, Some(&browser))).await;
        assert_eq!(location(&again), "/?signin=expired");

        let out = Request::builder()
            .method("POST")
            .uri("/api/auth/logout")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(&app, out).await.status(), StatusCode::NO_CONTENT);
        let res = send(&app, get_req("/api/me", Some(&cookie))).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_email_does_not_create_a_second_admin() {
        let Some(pool) = testutil::fresh_pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        sqlx::query("INSERT INTO app_user (org_id, email, name, role) VALUES ($1, 'boss@example.com', 'Boss', 'admin')")
            .bind(org)
            .execute(&pool)
            .await
            .unwrap();
        // The address in ADMIN_EMAIL now belongs to someone new.
        let ms = fake_microsoft("reused@example.com".into(), Uuid::new_v4().to_string()).await;
        let app = app_for(pool.clone(), &ms, "reused@example.com");

        let res = sign_in(&app).await;
        assert_eq!(location(&res), "/?signin=not-invited");
        let admins: i64 = sqlx::query_scalar("SELECT count(*) FROM app_user WHERE role = 'admin'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(admins, 1);
    }

    #[tokio::test]
    async fn invited_user_is_linked_on_first_sign_in() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let email = unique_email("invited");
        let oid = Uuid::new_v4().to_string();
        let user = add_user(&pool, &email, None).await;
        let ms = fake_microsoft(email.to_uppercase(), oid.clone()).await;
        let app = app_for(pool.clone(), &ms, "nobody@example.com");

        let res = sign_in(&app).await;
        assert_eq!(location(&res), "/");
        let me = json_body(send(&app, get_req("/api/me", Some(&session_of(&res)))).await).await;
        assert_eq!(me["role"], "resourcer");
        let linked: Option<String> =
            sqlx::query_scalar("SELECT ms_oid FROM app_user WHERE id = $1")
                .bind(user)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(linked.as_deref(), Some(oid.as_str()));
    }

    #[tokio::test]
    async fn reused_email_cannot_take_over_an_account() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let email = unique_email("leaver");
        let old_account = format!("old-{}", Uuid::new_v4());
        let user = add_user(&pool, &email, Some(&old_account)).await;
        // A different Microsoft account now holds the same address.
        let ms = fake_microsoft(email.clone(), Uuid::new_v4().to_string()).await;
        let app = app_for(pool.clone(), &ms, &email);

        let res = sign_in(&app).await;
        assert_eq!(location(&res), "/?signin=mismatch");
        assert!(set_cookie(&res, "sourcer_session").is_none());
        let (linked, refused): (Option<String>, i64) = sqlx::query_as(
            "SELECT u.ms_oid, (SELECT count(*) FROM audit a WHERE a.target = 'user:' || u.id
                               AND a.action = 'auth.sign_in_refused')
             FROM app_user u WHERE u.id = $1",
        )
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            linked.as_deref(),
            Some(old_account.as_str()),
            "link unchanged"
        );
        assert_eq!(refused, 1);
    }

    #[tokio::test]
    async fn disabled_user_cannot_sign_in_and_loses_open_sessions() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let email = unique_email("staff");
        let user = add_user(&pool, &email, None).await;
        let ms = fake_microsoft(email, Uuid::new_v4().to_string()).await;
        let app = app_for(pool.clone(), &ms, "nobody@example.com");
        let cookie = session_of(&sign_in(&app).await);
        assert_eq!(
            send(&app, get_req("/api/me", Some(&cookie))).await.status(),
            StatusCode::OK
        );

        sqlx::query("UPDATE app_user SET disabled_at = now() WHERE id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .unwrap();
        let res = send(&app, get_req("/api/me", Some(&cookie))).await;
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "open session ends at once"
        );
        assert_eq!(location(&sign_in(&app).await), "/?signin=disabled");
    }

    #[tokio::test]
    async fn idle_or_over_age_sessions_end() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let email = unique_email("idle");
        add_user(&pool, &email, None).await;
        let ms = fake_microsoft(email, Uuid::new_v4().to_string()).await;
        let app = app_for(pool.clone(), &ms, "nobody@example.com");

        let idle = session_of(&sign_in(&app).await);
        let old = session_of(&sign_in(&app).await);
        let hash = |c: &str| {
            use base64::Engine;
            use sha2::Digest;
            let token = c.split_once('=').unwrap().1;
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(sha2::Sha256::digest(token.as_bytes()))
        };
        sqlx::query("UPDATE user_session SET expires_at = now() - interval '1 minute' WHERE token_hash = $1")
            .bind(hash(&idle))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE user_session SET absolute_expires_at = now() - interval '1 minute' WHERE token_hash = $1")
            .bind(hash(&old))
            .execute(&pool)
            .await
            .unwrap();
        for c in [idle, old] {
            assert_eq!(
                send(&app, get_req("/api/me", Some(&c))).await.status(),
                StatusCode::UNAUTHORIZED
            );
        }
    }

    #[tokio::test]
    async fn sign_in_link_from_another_browser_is_refused() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let email = unique_email("victim");
        add_user(&pool, &email, None).await;
        let ms = fake_microsoft(email, Uuid::new_v4().to_string()).await;
        let app = app_for(pool.clone(), &ms, "nobody@example.com");

        // The attacker starts a sign-in and sends the callback link to a colleague.
        let (state, _attackers_cookie) = begin(&app).await;
        let cb = format!("/api/auth/callback?code=good-code&state={state}");
        let res = send(&app, get_req(&cb, None)).await;
        assert_eq!(location(&res), "/?signin=browser");
        assert!(set_cookie(&res, "sourcer_session").is_none());
        let res = send(&app, get_req(&cb, Some("sourcer_oauth=someone-elses"))).await;
        assert_eq!(location(&res), "/?signin=browser");
    }

    #[tokio::test]
    async fn someone_not_invited_is_turned_away() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let stranger = unique_email("stranger");
        let ms = fake_microsoft(stranger.clone(), Uuid::new_v4().to_string()).await;
        let app = app_for(pool.clone(), &ms, "someone-else@example.com");

        let res = sign_in(&app).await;
        assert_eq!(location(&res), "/?signin=not-invited");
        assert!(set_cookie(&res, "sourcer_session").is_none());
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM app_user WHERE email = $1")
            .bind(&stranger)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn forged_state_and_cancelled_sign_in_fail_safely() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let app = app_for(pool, "http://127.0.0.1:9", "a@example.com");
        let forged = get_req(
            "/api/auth/callback?code=x&state=forged",
            Some("sourcer_oauth=forged"),
        );
        assert_eq!(location(&send(&app, forged).await), "/?signin=expired");
        let res = send(
            &app,
            get_req("/api/auth/callback?error=access_denied", None),
        )
        .await;
        assert_eq!(location(&res), "/?signin=cancelled");
    }
}
