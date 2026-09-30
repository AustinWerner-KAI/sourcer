//! End-to-end API tests, one file per area. Shared helpers live here.

use super::*;
use crate::testutil;
use axum::body::Body;
use axum::http::{header, Request, Response, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

mod auth_api;
mod candidates_api;
mod people_api;
mod roles_api;
mod search_api;
mod team_api;

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
        "capabilities": ["Stakeholder management"],
        "domains": [{"name": "Privileged access", "weight": "must"}],
        "tools": [{"name": "CyberArk", "status": tool_status}],
        "locations": ["Dubai"], "remote": false,
        "employer_types": ["Trading firms"], "leave_out": []
    })
}

/// A stand-in for People Data Labs: every search matches `total` people and
/// returns as many made-up records as asked for. Returns (url, calls).
async fn fake_pdl(total: u64) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    fake_pdl_seeing(total, Arc::default()).await
}

type Bodies = Arc<std::sync::Mutex<Vec<Value>>>;

async fn fake_pdl_seeing(
    total: u64,
    bodies: Bodies,
) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let app = Router::new().route(
        "/v5/person/search",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            let bodies = bodies.clone();
            async move {
                bodies.lock().unwrap().push(body.clone());
                let n = seen.fetch_add(1, Ordering::SeqCst);
                let size = body["size"].as_u64().unwrap().min(total);
                let data: Vec<Value> = (0..size)
                    .map(|i| json!({"id": format!("p-{n}-{i}-{}", Uuid::new_v4()), "full_name": "Sample Person",
                                    "job_company_name": "Samplefirm"}))
                    .collect();
                Json(json!({"status": 200, "total": total, "data": data}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, calls)
}

fn searching_app(pool: PgPool, pdl_url: &str) -> (Router, Arc<PdlClient>) {
    let pdl = Arc::new(PdlClient::with_base_url(Some("k".into()), pdl_url));
    let mut state = AppState::new(Some(pool), None);
    state.pdl = pdl.clone();
    (router(state), pdl)
}

/// A role for a new client with a confirmed brief for New York and Dubai.
async fn searchable_role(app: &Router, me: &str) -> String {
    let client = new_client(app, me, "Client S", false).await;
    let body = json!({"client_id": client["id"], "title": "IAM", "spec_text": ""});
    let role = json_body(send(app, json_req("POST", "/api/roles", me, body)).await).await;
    let uri = format!("/api/roles/{}", role["id"].as_str().unwrap());
    let mut l = lines(Some("required"));
    l["locations"] = json!(["New York", "Dubai"]);
    let res = send(
        app,
        json_req(
            "POST",
            &format!("{uri}/brief/confirm"),
            me,
            json!({"lines": l, "based_on": null}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    uri
}

/// A stand-in for Claude's ranking: tier A for c1, B for c2, C for the rest,
/// best first. Returns (url, request bodies).
async fn fake_ranker() -> (String, Bodies) {
    let bodies: Bodies = Arc::default();
    let seen = bodies.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(body.clone());
                let text = body["messages"][0]["content"].as_str().unwrap().to_string();
                let n = text.matches("\"id\": \"c").count();
                let candidates: Vec<Value> = (1..=n)
                    .map(|i| {
                        let tier = ["A", "B"].get(i - 1).copied().unwrap_or("C");
                        json!({"id": format!("c{i}"), "tier": tier, "score": 100 - i as i64,
                               "reason": format!("Evidence {i} with **IAM**."),
                               "unknowns": ["Python or Go"]})
                    })
                    .collect();
                Json(
                    json!({"content": [{"type": "tool_use", "id": "t", "name": "record_ranking",
                                          "input": {"candidates": candidates}}]}),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, bodies)
}

/// Run every queued job of the handler's kind for `org` once, as the worker would.
async fn run_jobs(pool: &PgPool, org: Uuid, handler: &dyn crate::worker::JobHandler) -> usize {
    let jobs: Vec<(Uuid, Uuid, String, Value, i32)> = sqlx::query_as(
        "UPDATE job SET status = 'running' WHERE kind = $1 AND status = 'queued' AND org_id = $2
         RETURNING id, org_id, kind, payload, attempts",
    )
    .bind(handler.kind())
    .bind(org)
    .fetch_all(pool)
    .await
    .unwrap();
    for (id, org_id, kind, payload, attempts) in &jobs {
        let job = crate::jobs::Job {
            id: *id,
            org_id: *org_id,
            kind: kind.clone(),
            payload: payload.clone(),
            attempts: *attempts,
        };
        handler.handle(&job).await.unwrap();
        crate::jobs::complete(pool, *id).await.unwrap();
    }
    jobs.len()
}

/// A role with `n` people found in Dubai by a pull (worker included).
/// Returns (app, role uri, ranker, request bodies Claude saw).
async fn role_with_people(
    pool: &PgPool,
    org: Uuid,
    me: &str,
    n: u64,
) -> (Router, String, candidates::RankHandler, Bodies) {
    let (pdl_url, _) = fake_pdl(n).await;
    let (claude_url, bodies) = fake_ranker().await;
    let pdl = Arc::new(PdlClient::with_base_url(Some("k".into()), &pdl_url));
    let ai = Arc::new(Claude::with_base_url(Some("k".into()), &claude_url));
    let mut state = AppState::new(Some(pool.clone()), None);
    state.pdl = pdl.clone();
    state.ai = ai.clone();
    let app = router(state);
    let uri = searchable_role(&app, me).await;
    let s = json_body(
        send(
            &app,
            json_req(
                "POST",
                &format!("{uri}/search/count"),
                me,
                json!({"key": "c"}),
            ),
        )
        .await,
    )
    .await;
    let body = json!({"count_id": s["count"]["id"], "confirmed": true, "key": "p",
        "picks": [{"location": "Dubai", "size": n}]});
    let res = send(
        &app,
        json_req("POST", &format!("{uri}/search/pull"), me, body),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let puller = searching::PullHandler {
        pool: pool.clone(),
        source: pdl,
    };
    assert_eq!(run_jobs(pool, org, &puller).await, 1);
    let ranker = candidates::RankHandler {
        pool: pool.clone(),
        ai,
    };
    (app, uri, ranker, bodies)
}

async fn candidates_of(app: &Router, uri: &str, me: &str, tab: &str) -> Value {
    let res = send(
        app,
        get_req(&format!("{uri}/candidates?tab={tab}"), Some(me)),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    json_body(res).await
}

fn decide_req(me: &str, row: &Value, action: &str, reason: Option<&str>) -> Request<Body> {
    json_req(
        "POST",
        &format!("/api/candidates/{}/decide", row["id"].as_str().unwrap()),
        me,
        json!({"action": action, "reason": reason, "version": row["version"]}),
    )
}
