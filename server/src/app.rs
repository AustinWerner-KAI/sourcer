use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, State},
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
    admin,
    ai::Claude,
    auth,
    auth::AuthConfig,
    candidates, crm, cv,
    domain::Health,
    outreach, people,
    ratelimit::{self, RateLimiter},
    recruitly::Recruitly,
    retune, roles, searching,
    sources::pdl::PdlClient,
    team,
};

/// Brief drafts each person may run in ten minutes.
const DRAFTS_PER_TEN_MINUTES: u32 = 20;
/// Paid counts each person may run in ten minutes.
const COUNTS_PER_TEN_MINUTES: u32 = 30;

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
    /// Finds people. Not configured until PDL_API_KEY is set.
    pub pdl: Arc<PdlClient>,
    /// Limits paid counts per user.
    pub search_limit: Arc<RateLimiter<uuid::Uuid>>,
    /// The team's CRM and ATS. Not configured until RECRUITLY_API_KEY is set.
    pub recruitly: Arc<Recruitly>,
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
            pdl: Arc::new(PdlClient::new(None)),
            search_limit: Arc::new(RateLimiter::new(
                COUNTS_PER_TEN_MINUTES,
                std::time::Duration::from_secs(600),
            )),
            recruitly: Arc::new(Recruitly::new(None, None)),
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
        .route("/api/clients/:id", patch(admin::update_client))
        .route(
            "/api/admin/controls",
            get(admin::get_controls).put(admin::put_controls),
        )
        .route(
            "/api/admin/do-not-contact",
            get(admin::list_dnc).post(admin::add_dnc),
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
        .route("/api/roles/:id/search", get(searching::get_search))
        .route("/api/roles/:id/search/count", post(searching::count))
        .route("/api/roles/:id/search/retune", post(retune::retune))
        .route("/api/roles/:id/search/pull", post(searching::pull))
        .route("/api/roles/:id/candidates", get(candidates::list))
        .route("/api/roles/:id/candidates/rank", post(candidates::rank_now))
        .route("/api/candidates/:id/decide", post(candidates::decide))
        .route("/api/people/save", post(people::save))
        .route("/api/recruitly/status", get(crm::status))
        .route("/api/recruitly/test", post(crm::test))
        .route("/api/recruitly/jobs", get(crm::jobs))
        .route("/api/recruitly/jobs/:id", get(crm::job_preview))
        .route("/api/roles/from-recruitly", post(crm::import_role))
        .route("/api/roles/:id/recruitly", put(crm::link_job))
        .route(
            "/api/candidates/:id/recruitly-check",
            post(crm::check_again),
        )
        .route("/api/candidates/:id/handover", post(crm::handover))
        .route(
            "/api/candidates/:id/cv",
            get(cv::get_cv)
                .post(cv::upload)
                .layer(DefaultBodyLimit::max(cv::MAX_CV_BYTES + 1024)),
        )
        .route("/api/candidates/:id/cv/assess", post(cv::assess_more))
        .route("/api/candidates/:id/cv/recruitly", post(cv::to_recruitly))
        .route("/api/cv-assessments/:id/feedback", put(cv::feedback))
        .route(
            "/api/candidates/:id/outreach",
            get(outreach::get).post(outreach::start).put(outreach::save),
        )
        .route(
            "/api/candidates/:id/outreach/approve",
            post(outreach::approve),
        )
        .route("/api/candidates/:id/outreach/stop", post(outreach::stop))
        .route(
            "/api/me/outreach",
            get(outreach::get_settings).put(outreach::put_settings),
        )
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
mod tests;
