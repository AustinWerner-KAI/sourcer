use std::sync::Arc;

use axum::{
    extract::State,
    middleware,
    routing::{get, patch, post, put},
    Json, Router,
};
use sqlx::PgPool;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

use crate::{
    ai::Claude,
    auth,
    auth::AuthConfig,
    domain::Health,
    ratelimit::{self, RateLimiter},
    roles, team,
};

/// Brief drafts each person may run in ten minutes.
const DRAFTS_PER_TEN_MINUTES: u32 = 20;

/// Header the web app sends with every change. A page on another site cannot
/// send it without the browser asking first (which this server never allows),
/// so a forged form or link cannot make changes in someone's name.
pub const CHANGE_HEADER: &str = "x-sourcer";

#[derive(Clone)]
pub struct AppState {
    /// `None` only in tests that do not need a database.
    pub pool: Option<PgPool>,
    /// `None` until the Microsoft 365 app registration is configured.
    pub auth: Option<Arc<AuthConfig>>,
    pub http: reqwest::Client,
    /// Limits sign-in starts per network address.
    pub sign_in_limit: Arc<RateLimiter>,
    /// Drafts briefs. Not configured until ANTHROPIC_API_KEY is set.
    pub ai: Arc<Claude>,
    /// Limits paid brief drafts per user.
    pub draft_limit: Arc<RateLimiter<uuid::Uuid>>,
}

impl AppState {
    pub fn new(pool: Option<PgPool>, auth: Option<AuthConfig>) -> Self {
        Self {
            pool,
            auth: auth.map(Arc::new),
            http: reqwest::Client::new(),
            sign_in_limit: Arc::new(RateLimiter::new(
                ratelimit::SIGN_IN_PER_MINUTE,
                std::time::Duration::from_secs(60),
            )),
            ai: Arc::new(Claude::new(None, None)),
            draft_limit: Arc::new(RateLimiter::new(
                DRAFTS_PER_TEN_MINUTES,
                std::time::Duration::from_secs(600),
            )),
        }
    }
}

pub fn router(state: AppState) -> Router {
    router_with_web(state, None)
}

/// API routes, plus the built web app when `web_dir` is set. Unknown paths fall
/// back to `index.html` so client-side routes like `/candidates` load.
pub fn router_with_web(state: AppState, web_dir: Option<&str>) -> Router {
    let login = Router::new()
        .route("/api/auth/login", get(auth::login))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            ratelimit::sign_in,
        ));
    let api = Router::new()
        .route("/api/health", get(health))
        .merge(login)
        .route("/api/auth/callback", get(auth::callback))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/me", get(auth::me))
        .route("/api/team", get(team::list).post(team::invite))
        .route("/api/team/:id", patch(team::update))
        .route(
            "/api/clients",
            get(roles::list_clients).post(roles::create_client),
        )
        .route(
            "/api/roles",
            get(roles::list_roles).post(roles::create_role),
        )
        .route(
            "/api/roles/:id",
            get(roles::get_role).patch(roles::update_role),
        )
        .route("/api/roles/:id/brief", put(roles::save_brief))
        .route("/api/roles/:id/brief/draft", post(roles::draft_brief))
        .route("/api/roles/:id/brief/confirm", post(roles::confirm_brief))
        .with_state(state);
    let api = api.layer(middleware::from_fn(require_change_header));
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

/// Refuse any API change that lacks the web app's header (see CHANGE_HEADER).
async fn require_change_header(
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    use axum::{http::Method, response::IntoResponse};
    let reads = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !reads && req.uri().path().starts_with("/api/") && !req.headers().contains_key(CHANGE_HEADER)
    {
        return (axum::http::StatusCode::FORBIDDEN, "Missing request header.").into_response();
    }
    next.run(req).await
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
            .header(CHANGE_HEADER, "1")
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

    /// A user in `org` with an open session, made directly. Returns (user, cookie).
    async fn signed_in(pool: &PgPool, org: Uuid, role: &str) -> (Uuid, String) {
        use base64::Engine;
        use sha2::Digest;
        let user: Uuid = sqlx::query_scalar(
            "INSERT INTO app_user (org_id, email, name, role, ms_oid)
             VALUES ($1, $2, 'Someone', $3::user_role, $4) RETURNING id",
        )
        .bind(org)
        .bind(unique_email(role))
        .bind(role)
        .bind(Uuid::new_v4().to_string())
        .fetch_one(pool)
        .await
        .unwrap();
        let token = Uuid::new_v4().to_string();
        let hash = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(token.as_bytes()));
        sqlx::query(
            "INSERT INTO user_session (token_hash, org_id, user_id, expires_at, absolute_expires_at)
             VALUES ($1, $2, $3, now() + interval '1 hour', now() + interval '1 day')",
        )
        .bind(hash)
        .bind(org)
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
        (user, format!("sourcer_session={token}"))
    }

    fn json_req(method: &str, uri: &str, cookie: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .header(CHANGE_HEADER, "1")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn only_admins_manage_the_team() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let (_, resourcer) = signed_in(&pool, org, "resourcer").await;
        let app = app_for(pool.clone(), "http://127.0.0.1:9", "nobody@example.com");

        let res = send(&app, get_req("/api/team", None)).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = send(&app, get_req("/api/team", Some(&resourcer))).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let invite = json!({"name": "X", "email": "x@example.com", "role": "admin"});
        let res = send(&app, json_req("POST", "/api/team", &resourcer, invite)).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "no self-promotion");
        let uri = format!("/api/team/{}", Uuid::new_v4());
        let res = send(
            &app,
            json_req("PATCH", &uri, &resourcer, json!({"disabled": true})),
        )
        .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let made: i64 =
            sqlx::query_scalar("SELECT count(*) FROM app_user WHERE email = 'x@example.com'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(made, 0);
    }

    #[tokio::test]
    async fn admin_invites_then_the_invitee_can_sign_in() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let (_, admin) = signed_in(&pool, org, "admin").await;
        let email = unique_email("new-starter");
        let oid = Uuid::new_v4().to_string();
        let ms = fake_microsoft(email.clone(), oid).await;
        let app = app_for(pool.clone(), &ms, "nobody@example.com");

        let body =
            json!({"name": "  New Starter ", "email": email.to_uppercase(), "role": "resourcer"});
        let res = send(&app, json_req("POST", "/api/team", &admin, body.clone())).await;
        assert_eq!(res.status(), StatusCode::CREATED);
        let m = json_body(res).await;
        assert_eq!(
            (
                m["name"].as_str(),
                m["email"].as_str(),
                m["status"].as_str()
            ),
            (Some("New Starter"), Some(email.as_str()), Some("invited"))
        );
        let again = send(&app, json_req("POST", "/api/team", &admin, body)).await;
        assert_eq!(again.status(), StatusCode::CONFLICT);
        let bad = json!({"name": "X", "email": "not-an-email", "role": "resourcer"});
        let res = send(&app, json_req("POST", "/api/team", &admin, bad)).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let bad_role = json!({"name": "X", "email": "y@example.com", "role": "owner"});
        let res = send(&app, json_req("POST", "/api/team", &admin, bad_role)).await;
        assert!(res.status().is_client_error());

        assert_eq!(location(&sign_in(&app).await), "/");
        let team = json_body(send(&app, get_req("/api/team", Some(&admin))).await).await;
        let starter = team
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["email"] == email.as_str())
            .unwrap();
        assert_eq!(starter["status"], "active");
        let audited: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'team.invite'",
        )
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audited, 1);
    }

    #[tokio::test]
    async fn admin_switches_someone_off_and_back_on() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let (admin_id, admin) = signed_in(&pool, org, "admin").await;
        let (staff_id, staff) = signed_in(&pool, org, "resourcer").await;
        let app = app_for(pool.clone(), "http://127.0.0.1:9", "nobody@example.com");
        let uri = format!("/api/team/{staff_id}");

        let res = send(
            &app,
            json_req("PATCH", &uri, &admin, json!({"disabled": true})),
        )
        .await;
        assert_eq!(json_body(res).await["status"], "disabled");
        let res = send(&app, get_req("/api/me", Some(&staff))).await;
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "sessions end at once"
        );
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM user_session WHERE user_id = $1")
            .bind(staff_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);

        let res = send(
            &app,
            json_req("PATCH", &uri, &admin, json!({"disabled": false})),
        )
        .await;
        assert_eq!(json_body(res).await["status"], "active");

        let me = format!("/api/team/{admin_id}");
        let res = send(
            &app,
            json_req("PATCH", &me, &admin, json!({"disabled": true})),
        )
        .await;
        assert_eq!(
            res.status(),
            StatusCode::BAD_REQUEST,
            "cannot switch yourself off"
        );

        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM audit WHERE org_id = $1 AND action LIKE 'team.%' ORDER BY id",
        )
        .bind(org)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(actions, ["team.disable", "team.enable"]);
    }

    #[tokio::test]
    async fn admins_only_see_and_change_their_own_organisation() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let (org_a, org_b) = (testutil::org(&pool).await, testutil::org(&pool).await);
        let (_, admin_a) = signed_in(&pool, org_a, "admin").await;
        let (outsider, _) = signed_in(&pool, org_b, "resourcer").await;
        let app = app_for(pool.clone(), "http://127.0.0.1:9", "nobody@example.com");

        let team = json_body(send(&app, get_req("/api/team", Some(&admin_a))).await).await;
        assert_eq!(team.as_array().unwrap().len(), 1, "only org A");
        let uri = format!("/api/team/{outsider}");
        let res = send(
            &app,
            json_req("PATCH", &uri, &admin_a, json!({"disabled": true})),
        )
        .await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let disabled: bool =
            sqlx::query_scalar("SELECT disabled_at IS NOT NULL FROM app_user WHERE id = $1")
                .bind(outsider)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(!disabled);
    }

    #[tokio::test]
    async fn repeated_sign_in_starts_are_slowed_down_per_address() {
        use axum::extract::ConnectInfo;
        use std::net::SocketAddr;
        let mut state = AppState::new(None, None);
        state.sign_in_limit = Arc::new(RateLimiter::new(2, std::time::Duration::from_secs(60)));
        let app = router(state);
        let from = |uri: &str, addr: &str| {
            let mut req = get_req(uri, None);
            req.extensions_mut()
                .insert(ConnectInfo(addr.parse::<SocketAddr>().unwrap()));
            req
        };
        for _ in 0..2 {
            let res = send(&app, from("/api/auth/login", "10.0.0.1:5000")).await;
            assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
        let res = send(&app, from("/api/auth/login", "10.0.0.1:5001")).await;
        assert_eq!(location(&res), "/?signin=busy");
        let res = send(&app, from("/api/auth/login", "10.0.0.2:5000")).await;
        assert_eq!(
            res.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "other addresses unaffected"
        );
        let res = send(&app, from("/api/auth/callback", "10.0.0.1:5000")).await;
        assert_eq!(
            res.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "callback is not limited"
        );
        let res = send(&app, get_req("/api/health", None)).await;
        assert_eq!(res.status(), StatusCode::OK, "other routes are not limited");
    }

    #[tokio::test]
    async fn invited_admin_signs_in_as_admin() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let (_, admin) = signed_in(&pool, org, "admin").await;
        let email = unique_email("second-admin");
        let ms = fake_microsoft(email.clone(), Uuid::new_v4().to_string()).await;
        let app = app_for(pool.clone(), &ms, "nobody@example.com");
        let body = json!({"name": "Second", "email": email, "role": "admin"});
        let res = send(&app, json_req("POST", "/api/team", &admin, body)).await;
        assert_eq!(res.status(), StatusCode::CREATED);
        let target: String = sqlx::query_scalar(
            "SELECT target FROM audit WHERE org_id = $1 AND action = 'team.invite'",
        )
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            target.ends_with("role:admin"),
            "audit says who made an admin"
        );

        let cookie = session_of(&sign_in(&app).await);
        let me = json_body(send(&app, get_req("/api/me", Some(&cookie))).await).await;
        assert_eq!(me["role"], "admin");
    }

    #[tokio::test]
    async fn switched_back_on_user_can_sign_in_again() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let (_, admin) = signed_in(&pool, org, "admin").await;
        let email = unique_email("returner");
        let ms = fake_microsoft(email.clone(), Uuid::new_v4().to_string()).await;
        let app = app_for(pool.clone(), &ms, "nobody@example.com");
        let body = json!({"name": "Returner", "email": email, "role": "resourcer"});
        let m =
            json_body(send(&app, json_req("POST", "/api/team", &admin, body.clone())).await).await;
        let uri = format!("/api/team/{}", m["id"].as_str().unwrap());

        send(
            &app,
            json_req("PATCH", &uri, &admin, json!({"disabled": true})),
        )
        .await;
        assert_eq!(location(&sign_in(&app).await), "/?signin=disabled");
        let refused: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'auth.sign_in_refused'",
        )
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(refused, 1, "refusals of switched-off users are audited");
        let res = send(&app, json_req("POST", "/api/team", &admin, body)).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let msg = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec())
            .unwrap();
        assert!(msg.contains("switched off"));

        send(
            &app,
            json_req("PATCH", &uri, &admin, json!({"disabled": false})),
        )
        .await;
        assert_eq!(location(&sign_in(&app).await), "/");
    }

    #[tokio::test]
    async fn invite_catches_an_older_mixed_case_address() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let (_, admin) = signed_in(&pool, org, "admin").await;
        let email = unique_email("Mixed.Case");
        sqlx::query("INSERT INTO app_user (org_id, email, name) VALUES ($1, $2, 'Old')")
            .bind(org)
            .bind(&email)
            .execute(&pool)
            .await
            .unwrap();
        let app = app_for(pool.clone(), "http://127.0.0.1:9", "nobody@example.com");
        let body = json!({"name": "Dup", "email": email.to_lowercase(), "role": "resourcer"});
        let res = send(&app, json_req("POST", "/api/team", &admin, body)).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn repeating_a_switch_changes_and_audits_nothing() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let (_, admin) = signed_in(&pool, org, "admin").await;
        let (staff, _) = signed_in(&pool, org, "resourcer").await;
        let app = app_for(pool.clone(), "http://127.0.0.1:9", "nobody@example.com");
        let uri = format!("/api/team/{staff}");
        for _ in 0..2 {
            let res = send(
                &app,
                json_req("PATCH", &uri, &admin, json!({"disabled": true})),
            )
            .await;
            assert_eq!(res.status(), StatusCode::OK);
        }
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'team.disable'",
        )
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n, 1);
    }

    fn plain_app(pool: PgPool) -> Router {
        router(AppState::new(Some(pool), None))
    }

    async fn new_client(app: &Router, cookie: &str, name: &str, off_limits: bool) -> Value {
        let body = json!({"name": name, "domain": format!("{}.example.com", Uuid::new_v4()), "off_limits": off_limits});
        let res = send(app, json_req("POST", "/api/clients", cookie, body)).await;
        assert_eq!(res.status(), StatusCode::CREATED);
        json_body(res).await
    }

    fn confirming(tool_status: Option<&str>, based_on: Option<i64>) -> Value {
        json!({"lines": lines(tool_status), "based_on": based_on})
    }

    fn lines(tool_status: Option<&str>) -> Value {
        json!({
            "levels": ["Senior", "Lead"], "excluded_titles": ["Director"],
            "must_haves": ["Cloud security", "IAM"],
            "tools": [{"name": "CyberArk", "status": tool_status}],
            "locations": ["Dubai"], "remote": false,
            "employer_types": ["Trading firms"], "leave_out": []
        })
    }

    #[tokio::test]
    async fn clients_need_a_real_domain_and_it_is_unique() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let (_, me) = signed_in(&pool, org, "resourcer").await;
        let app = plain_app(pool.clone());
        let bad = json!({"name": "Acme", "domain": "not a domain", "off_limits": false});
        let res = send(&app, json_req("POST", "/api/clients", &me, bad)).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let domain = format!("{}.example.com", Uuid::new_v4());
        let ok = json!({"name": "Acme", "domain": format!("https://www.{domain}/jobs"), "off_limits": false});
        let res = send(&app, json_req("POST", "/api/clients", &me, ok.clone())).await;
        assert_eq!(
            json_body(res).await["domain"],
            domain.as_str(),
            "domain is cleaned"
        );
        let res = send(&app, json_req("POST", "/api/clients", &me, ok)).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn brief_is_confirmed_only_when_every_tool_is_answered_then_locks() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let (_, me) = signed_in(&pool, org, "resourcer").await;
        let app = plain_app(pool.clone());
        let hiring = new_client(&app, &me, "Client A", false).await;
        new_client(&app, &me, "Blocked Co", true).await;
        new_client(&app, &me, "Unrelated", false).await;

        let body =
            json!({"client_id": hiring["id"], "title": "Senior IAM Engineer", "spec_text": "spec"});
        let res = send(&app, json_req("POST", "/api/roles", &me, body)).await;
        assert_eq!(res.status(), StatusCode::CREATED);
        let role = json_body(res).await;
        assert_eq!(
            role["locked_out"],
            json!([
                {"id": role["locked_out"][0]["id"], "name": "Blocked Co", "hiring": false},
                {"id": hiring["id"], "name": "Client A", "hiring": true}
            ]),
            "hiring client and off-limits clients, nothing else"
        );
        let uri = format!("/api/roles/{}", role["id"].as_str().unwrap());

        let confirm = format!("{uri}/brief/confirm");
        let res = send(
            &app,
            json_req("PUT", &format!("{uri}/brief"), &me, lines(None)),
        )
        .await;
        assert_eq!(json_body(res).await["brief"]["confirmed"], false);
        let res = send(
            &app,
            json_req("POST", &confirm, &me, confirming(None, Some(1))),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let msg = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec())
            .unwrap();
        assert!(msg.contains("Answer 1 named tool"), "{msg}");

        // Confirm saves and locks exactly the lines sent, in one step.
        let res = send(
            &app,
            json_req(
                "POST",
                &confirm,
                &me,
                confirming(Some("replacing"), Some(1)),
            ),
        )
        .await;
        let d = json_body(res).await;
        assert_eq!(
            (
                d["brief"]["confirmed"].as_bool(),
                d["brief"]["version"].as_i64()
            ),
            (Some(true), Some(1))
        );

        // Editing after confirming starts version 2; version 1 never changes.
        let mut edited = lines(Some("required"));
        edited["locations"] = json!(["New York"]);
        let res = send(&app, json_req("PUT", &format!("{uri}/brief"), &me, edited)).await;
        let d = json_body(res).await;
        assert_eq!(
            (
                d["brief"]["confirmed"].as_bool(),
                d["brief"]["version"].as_i64()
            ),
            (Some(false), Some(2))
        );
        let v1: Value =
            sqlx::query_scalar("SELECT locations FROM brief WHERE role_id = $1 AND version = 1")
                .bind(Uuid::parse_str(role["id"].as_str().unwrap()).unwrap())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(v1, json!(["Dubai"]));
        let audited: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit WHERE org_id = $1 AND action IN ('role.create', 'brief.confirm', 'client.create')",
        )
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audited, 5);
    }

    #[tokio::test]
    async fn roles_and_clients_of_another_organisation_are_invisible() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let (org_a, org_b) = (testutil::org(&pool).await, testutil::org(&pool).await);
        let (_, a) = signed_in(&pool, org_a, "resourcer").await;
        let (_, b) = signed_in(&pool, org_b, "admin").await;
        let app = plain_app(pool.clone());
        let client_a = new_client(&app, &a, "A's client", false).await;
        let body = json!({"client_id": client_a["id"], "title": "Role", "spec_text": ""});
        let role =
            json_body(send(&app, json_req("POST", "/api/roles", &a, body.clone())).await).await;
        let uri = format!("/api/roles/{}", role["id"].as_str().unwrap());

        // B can neither see nor use A's client or role.
        let res = send(&app, json_req("POST", "/api/roles", &b, body)).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            send(&app, get_req(&uri, Some(&b))).await.status(),
            StatusCode::NOT_FOUND
        );
        let res = send(
            &app,
            json_req("PUT", &format!("{uri}/brief"), &b, lines(None)),
        )
        .await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let res = send(
            &app,
            json_req(
                "POST",
                &format!("{uri}/brief/confirm"),
                &b,
                confirming(Some("nice"), None),
            ),
        )
        .await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let lists = json_body(send(&app, get_req("/api/roles", Some(&b))).await).await;
        assert_eq!(lists, json!([]));
        let clients = json_body(send(&app, get_req("/api/clients", Some(&b))).await).await;
        assert_eq!(clients, json!([]));
        assert_eq!(
            send(&app, get_req("/api/roles", None)).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn changes_without_the_app_header_are_refused() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let (_, me) = signed_in(&pool, org, "resourcer").await;
        let app = plain_app(pool.clone());
        let forged = Request::builder()
            .method("POST")
            .uri("/api/clients")
            .header(header::COOKIE, &me)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"name": "X", "domain": "x.example", "off_limits": false}).to_string(),
            ))
            .unwrap();
        assert_eq!(send(&app, forged).await.status(), StatusCode::FORBIDDEN);
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM client WHERE org_id = $1")
            .bind(org)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(
            send(&app, get_req("/api/clients", Some(&me)))
                .await
                .status(),
            StatusCode::OK,
            "reads need no header"
        );
    }

    #[tokio::test]
    async fn a_role_without_a_client_cannot_be_confirmed_until_one_is_set() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let (_, me) = signed_in(&pool, org, "resourcer").await;
        let app = plain_app(pool.clone());
        let role: Uuid = sqlx::query_scalar(
            "INSERT INTO role (org_id, title) VALUES ($1, 'Old role') RETURNING id",
        )
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
        let uri = format!("/api/roles/{role}");
        let res = send(
            &app,
            json_req(
                "POST",
                &format!("{uri}/brief/confirm"),
                &me,
                confirming(Some("nice"), None),
            ),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let client = new_client(&app, &me, "Late Client", false).await;
        let body = json!({"title": "Old role", "spec_text": "", "client_id": client["id"]});
        let d = json_body(send(&app, json_req("PATCH", &uri, &me, body)).await).await;
        assert_eq!(d["locked_out"][0]["hiring"], true);
        let res = send(
            &app,
            json_req(
                "POST",
                &format!("{uri}/brief/confirm"),
                &me,
                confirming(Some("nice"), None),
            ),
        )
        .await;
        assert_eq!(json_body(res).await["brief"]["confirmed"], true);

        // A window still showing "no brief" cannot confirm over version 1.
        let res = send(
            &app,
            json_req(
                "POST",
                &format!("{uri}/brief/confirm"),
                &me,
                confirming(Some("required"), None),
            ),
        )
        .await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        // And the client, once set, stays.
        let other = new_client(&app, &me, "Other Client", false).await;
        let body = json!({"title": "Old role", "spec_text": "", "client_id": other["id"]});
        let res = send(&app, json_req("PATCH", &uri, &me, body)).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn claude_drafts_the_brief_from_the_spec() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let (_, me) = signed_in(&pool, org, "resourcer").await;
        let fake = Router::new().route(
            "/v1/messages",
            post(|| async {
                Json(json!({"content": [{"type": "tool_use", "id": "t", "name": "record_brief", "input": {
                    "levels": ["Senior"], "must_haves": ["IAM"], "tools": ["Okta"],
                    "locations": ["Dubai"], "remote": false
                }}]}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });

        let without = plain_app(pool.clone());
        let mut state = AppState::new(Some(pool.clone()), None);
        state.ai = Arc::new(Claude::with_base_url(Some("k".into()), &url));
        let app = router(state);
        let client = new_client(&app, &me, "Client B", false).await;
        let body = json!({"client_id": client["id"], "title": "IAM", "spec_text": "Senior IAM Engineer in Dubai"});
        let role = json_body(send(&app, json_req("POST", "/api/roles", &me, body)).await).await;
        let draft = format!("/api/roles/{}/brief/draft", role["id"].as_str().unwrap());

        let res = send(&without, json_req("POST", &draft, &me, json!({}))).await;
        assert_eq!(
            res.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "no key, no call"
        );

        let d = json_body(send(&app, json_req("POST", &draft, &me, json!({}))).await).await;
        assert_eq!(d["brief"]["drafted_by_ai"], true);
        assert_eq!(
            d["brief"]["lines"]["tools"],
            json!([{"name": "Okta", "status": null}])
        );
        assert!(
            d["brief"]["lines"]["excluded_titles"]
                .as_array()
                .unwrap()
                .len()
                >= 5
        );
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
