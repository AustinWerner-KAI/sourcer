//! Background job queue stored in Postgres (SRS N14).
//!
//! Workers claim jobs with `FOR UPDATE SKIP LOCKED`, so two workers never run
//! the same job. Failed jobs retry with backoff up to `MAX_ATTEMPTS`, then stop.

use anyhow::Result;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub const MAX_ATTEMPTS: i32 = 5;

#[derive(Debug, Clone)]
pub struct Job {
    pub id: Uuid,
    pub org_id: Uuid,
    pub kind: String,
    pub payload: Value,
    /// Attempts so far, including the current one.
    pub attempts: i32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FailOutcome {
    WillRetry,
    GaveUp,
}

pub async fn enqueue(pool: &PgPool, org_id: Uuid, kind: &str, payload: Value) -> Result<Uuid> {
    let id = sqlx::query_scalar(
        "INSERT INTO job (org_id, kind, payload) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(org_id)
    .bind(kind)
    .bind(payload)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Claim the next ready job of one of `kinds`, or `None` if there is nothing to do.
pub async fn claim(pool: &PgPool, kinds: &[String]) -> Result<Option<Job>> {
    let row = sqlx::query(
        "UPDATE job SET status = 'running', attempts = attempts + 1, started_at = now()
         WHERE id = (
           SELECT id FROM job
           WHERE status = 'queued' AND run_after <= now() AND kind = ANY($1)
           ORDER BY run_after, created_at
           FOR UPDATE SKIP LOCKED
           LIMIT 1)
         RETURNING id, org_id, kind, payload, attempts",
    )
    .bind(kinds)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| Job {
        id: r.get("id"),
        org_id: r.get("org_id"),
        kind: r.get("kind"),
        payload: r.get("payload"),
        attempts: r.get("attempts"),
    }))
}

pub async fn complete(pool: &PgPool, job_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE job SET status = 'done', last_error = NULL WHERE id = $1")
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record a failure. Retries later unless the job has used all its attempts.
pub async fn fail(pool: &PgPool, job: &Job, error: &str) -> Result<FailOutcome> {
    if job.attempts >= MAX_ATTEMPTS {
        sqlx::query("UPDATE job SET status = 'failed', last_error = $2 WHERE id = $1")
            .bind(job.id)
            .bind(error)
            .execute(pool)
            .await?;
        return Ok(FailOutcome::GaveUp);
    }
    sqlx::query(
        "UPDATE job SET status = 'queued', last_error = $2,
                run_after = now() + make_interval(secs => $3)
         WHERE id = $1",
    )
    .bind(job.id)
    .bind(error)
    .bind(backoff_secs(job.attempts) as f64)
    .execute(pool)
    .await?;
    Ok(FailOutcome::WillRetry)
}

/// Put back jobs left 'running' longer than `older_than_secs` (a worker crashed).
pub async fn requeue_stale(pool: &PgPool, older_than_secs: i64) -> Result<u64> {
    let done = sqlx::query(
        "UPDATE job SET status = 'queued', last_error = 'worker stopped mid-job'
         WHERE status = 'running' AND started_at < now() - make_interval(secs => $1)",
    )
    .bind(older_than_secs as f64)
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

/// 30s, 60s, 120s ... capped at one hour.
pub fn backoff_secs(attempts: i32) -> i64 {
    let step = (attempts.max(1) - 1).min(7) as u32;
    (30 * 2_i64.pow(step)).min(3600)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use serde_json::json;

    #[test]
    fn backoff_doubles_then_caps() {
        assert_eq!(backoff_secs(1), 30);
        assert_eq!(backoff_secs(2), 60);
        assert_eq!(backoff_secs(3), 120);
        assert_eq!(backoff_secs(20), 3600);
    }

    fn kind() -> String {
        format!("test-{}", Uuid::new_v4())
    }

    #[tokio::test]
    async fn claim_runs_each_job_once() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let k = kind();
        let id = enqueue(&pool, org, &k, json!({"n": 1})).await.unwrap();

        let job = claim(&pool, std::slice::from_ref(&k))
            .await
            .unwrap()
            .expect("job ready");
        assert_eq!((job.id, job.attempts), (id, 1));
        assert!(
            claim(&pool, std::slice::from_ref(&k))
                .await
                .unwrap()
                .is_none(),
            "already claimed"
        );

        complete(&pool, id).await.unwrap();
        assert!(claim(&pool, &[k]).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn failure_retries_later_then_gives_up() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let k = kind();
        enqueue(&pool, org, &k, json!({})).await.unwrap();

        let job = claim(&pool, std::slice::from_ref(&k))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            fail(&pool, &job, "boom").await.unwrap(),
            FailOutcome::WillRetry
        );
        assert!(
            claim(&pool, std::slice::from_ref(&k))
                .await
                .unwrap()
                .is_none(),
            "waits for backoff"
        );

        let last = Job {
            attempts: MAX_ATTEMPTS,
            ..job
        };
        assert_eq!(
            fail(&pool, &last, "boom").await.unwrap(),
            FailOutcome::GaveUp
        );
        let status: String = sqlx::query_scalar("SELECT status::text FROM job WHERE id = $1")
            .bind(last.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "failed");
    }

    #[tokio::test]
    async fn stale_running_jobs_are_requeued() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let k = kind();
        let id = enqueue(&pool, org, &k, json!({})).await.unwrap();
        claim(&pool, std::slice::from_ref(&k))
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE job SET started_at = now() - interval '1 hour' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();

        assert!(requeue_stale(&pool, 600).await.unwrap() >= 1);
        let again = claim(&pool, &[k])
            .await
            .unwrap()
            .expect("back in the queue");
        assert_eq!((again.id, again.attempts), (id, 2));
    }
}
