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
    app.layer(TraceLayer::new_for_http())
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

    #[tokio::test]
    async fn health_responds_without_db() {
        let app = router(AppState::new(None, None));
        let res = send(&app, get_req("/api/health", None)).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let v: Value = serde_json::from_slice(&body).unwrap();
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
    async fn fake_microsoft(email: &'static str, oid: &'static str) -> String {
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
                    Json(json!({"id": oid, "displayName": "Kai Test", "mail": email}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    async fn app_with_fake(pool: PgPool, ms: &str, admin: &str) -> Router {
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

    /// Start a sign-in and return the `state` Microsoft would send back.
    async fn begin(app: &Router) -> String {
        let res = send(app, get_req("/api/auth/login", None)).await;
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        let url = reqwest::Url::parse(&location(&res)).unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["redirect_uri"], "http://localhost:8080/api/auth/callback");
        assert!(url.path().ends_with("/tenant/oauth2/v2.0/authorize"));
        q["state"].clone()
    }

    #[tokio::test]
    async fn admin_signs_in_sees_me_and_signs_out() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let email =
            Box::leak(format!("admin-{}@example.com", uuid::Uuid::new_v4()).into_boxed_str());
        let oid = Box::leak(uuid::Uuid::new_v4().to_string().into_boxed_str());
        let ms = fake_microsoft(email, oid).await;
        let app = app_with_fake(pool.clone(), &ms, email).await;

        let state = begin(&app).await;
        let cb = format!("/api/auth/callback?code=good-code&state={state}");
        let res = send(&app, get_req(&cb, None)).await;
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        assert_eq!(location(&res), "/");
        let set = res.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_string();
        assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax"));
        let cookie = set.split(';').next().unwrap().to_string();

        let res = send(&app, get_req("/api/me", Some(&cookie))).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let me: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            (me["email"].as_str(), me["role"].as_str()),
            (Some(&*email), Some("admin"))
        );

        let signed_in: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit a JOIN app_user u ON u.id = a.actor_id
             WHERE u.email = $1 AND a.action = 'auth.sign_in'",
        )
        .bind(&*email)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(signed_in, 1);

        // The same state cannot be used twice.
        let again = send(&app, get_req(&cb, None)).await;
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
    async fn someone_not_invited_is_turned_away() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let stranger =
            Box::leak(format!("stranger-{}@example.com", uuid::Uuid::new_v4()).into_boxed_str());
        let oid = Box::leak(uuid::Uuid::new_v4().to_string().into_boxed_str());
        let ms = fake_microsoft(stranger, oid).await;
        let app = app_with_fake(pool.clone(), &ms, "someone-else@example.com").await;

        let state = begin(&app).await;
        let res = send(
            &app,
            get_req(
                &format!("/api/auth/callback?code=good-code&state={state}"),
                None,
            ),
        )
        .await;
        assert_eq!(location(&res), "/?signin=not-invited");
        assert!(res.headers().get(header::SET_COOKIE).is_none());
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM app_user WHERE email = $1")
            .bind(&*stranger)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn unknown_state_and_cancelled_sign_in_fail_safely() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let app = app_with_fake(pool, "http://127.0.0.1:9", "a@example.com").await;
        let res = send(
            &app,
            get_req("/api/auth/callback?code=x&state=forged", None),
        )
        .await;
        assert_eq!(location(&res), "/?signin=expired");
        let res = send(
            &app,
            get_req("/api/auth/callback?error=access_denied", None),
        )
        .await;
        assert_eq!(location(&res), "/?signin=cancelled");
    }
}
