//! Test helpers for tests that need Postgres.
//!
//! Set `TEST_DATABASE_URL` to run them. Without it they return early, so
//! `cargo test` still works on a machine with no database. Each test makes its
//! own organisation, so tests never see each other's rows.

use sqlx::PgPool;
use uuid::Uuid;

pub async fn pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL not set; skipping database test");
        return None;
    };
    let pool = PgPool::connect(&url)
        .await
        .expect("TEST_DATABASE_URL is set but the database is not reachable");
    crate::db::migrate(&pool).await.expect("migrations apply");
    Some(pool)
}

/// A brand-new, empty database, for tests of behaviour that is global to the
/// whole database (such as creating the first admin).
pub async fn fresh_pool() -> Option<PgPool> {
    let base = std::env::var("TEST_DATABASE_URL").ok()?;
    let admin = PgPool::connect(&base)
        .await
        .expect("TEST_DATABASE_URL is set but the database is not reachable");
    let name = format!("sourcer_t_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .expect("can create a test database");
    let mut url = reqwest::Url::parse(&base).expect("TEST_DATABASE_URL is a URL");
    url.set_path(&name);
    let pool = PgPool::connect(url.as_str()).await.expect("fresh database");
    crate::db::migrate(&pool).await.expect("migrations apply");
    Some(pool)
}

pub async fn org(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("INSERT INTO org (name) VALUES ('test') RETURNING id")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A role for "Test Client" (test-client.example) with a confirmed brief,
/// ready to search against. Returns (role, brief).
pub async fn role_with_brief(pool: &PgPool, org_id: Uuid) -> (Uuid, Uuid) {
    let role: Uuid = sqlx::query_scalar(
        "WITH c AS (INSERT INTO client (org_id, name, domain) VALUES ($1, 'Test Client', 'test-client.example') RETURNING id)
         INSERT INTO role (org_id, title, client_id) SELECT $1, 'Senior IAM Engineer', id FROM c RETURNING id",
    )
    .bind(org_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let brief: Uuid = sqlx::query_scalar(
        "INSERT INTO brief (org_id, role_id, version, levels, must_haves, tools, locations, employer_types, confirmed_at)
         VALUES ($1, $2, 1, '[\"Senior\"]', '[\"IAM\"]', '[]', '[\"Dubai\"]', '[\"Trading firms\"]', now()) RETURNING id",
    )
    .bind(org_id)
    .bind(role)
    .fetch_one(pool)
    .await
    .unwrap();
    (role, brief)
}
