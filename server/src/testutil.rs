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

pub async fn org(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("INSERT INTO org (name) VALUES ('test') RETURNING id")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A role with a confirmed brief, ready to search against. Returns (role, brief).
pub async fn role_with_brief(pool: &PgPool, org_id: Uuid) -> (Uuid, Uuid) {
    let role: Uuid = sqlx::query_scalar(
        "INSERT INTO role (org_id, title) VALUES ($1, 'Senior IAM Engineer') RETURNING id",
    )
    .bind(org_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let brief: Uuid = sqlx::query_scalar(
        "INSERT INTO brief (org_id, role_id, version, level, must_haves, tools, locations, employer_types)
         VALUES ($1, $2, 1, 'senior', '[]', '[]', '[]', '[]') RETURNING id",
    )
    .bind(org_id)
    .bind(role)
    .fetch_one(pool)
    .await
    .unwrap();
    (role, brief)
}
