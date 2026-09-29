//! Runs queued jobs. Each job kind has one handler; the worker only claims kinds
//! it has a handler for.

use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use anyhow::Result;
use sqlx::PgPool;
use tokio::sync::watch;

use crate::{
    audit,
    jobs::{self, FailOutcome, Job},
};

pub type HandlerFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

pub trait JobHandler: Send + Sync {
    fn kind(&self) -> &'static str;
    fn handle<'a>(&'a self, job: &'a Job) -> HandlerFuture<'a>;
}

/// A job left running this long is assumed abandoned by a crashed worker.
const STALE_AFTER_SECS: i64 = 15 * 60;

pub struct Worker {
    pool: PgPool,
    handlers: HashMap<&'static str, Arc<dyn JobHandler>>,
    idle: Duration,
}

impl Worker {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            handlers: HashMap::new(),
            idle: Duration::from_secs(2),
        }
    }

    pub fn register(mut self, handler: Arc<dyn JobHandler>) -> Self {
        self.handlers.insert(handler.kind(), handler);
        self
    }

    fn kinds(&self) -> Vec<String> {
        self.handlers.keys().map(|k| k.to_string()).collect()
    }

    /// Run one job if one is ready. Returns whether a job ran.
    pub async fn run_once(&self) -> Result<bool> {
        let Some(job) = jobs::claim(&self.pool, &self.kinds()).await? else {
            return Ok(false);
        };
        let handler = self
            .handlers
            .get(job.kind.as_str())
            .expect("claim only returns registered kinds");
        match handler.handle(&job).await {
            Ok(()) => jobs::complete(&self.pool, job.id).await?,
            Err(e) => {
                let error = format!("{e:#}");
                tracing::warn!(job = %job.id, kind = %job.kind, attempt = job.attempts, %error, "job failed");
                if jobs::fail(&self.pool, &job, &error).await? == FailOutcome::GaveUp {
                    let target = format!("job:{}", job.id);
                    audit::record(
                        &self.pool,
                        job.org_id,
                        None,
                        audit::action::JOB_FAILED,
                        &target,
                    )
                    .await?;
                }
            }
        }
        Ok(true)
    }

    /// Loop until `shutdown` turns true.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        tracing::info!(kinds = ?self.kinds(), "worker started");
        while !*shutdown.borrow() {
            if let Err(e) = jobs::requeue_stale(&self.pool, STALE_AFTER_SECS).await {
                tracing::error!(error = %e, "requeue failed");
            }
            let ran = self.run_once().await.unwrap_or_else(|e| {
                tracing::error!(error = %e, "worker error");
                false
            });
            if !ran {
                tokio::select! {
                    _ = tokio::time::sleep(self.idle) => {}
                    _ = shutdown.changed() => {}
                }
            }
        }
        tracing::info!("worker stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counter {
        kind: &'static str,
        calls: AtomicUsize,
        fail: bool,
    }

    impl JobHandler for Counter {
        fn kind(&self) -> &'static str {
            self.kind
        }
        fn handle<'a>(&'a self, _job: &'a Job) -> HandlerFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if self.fail {
                    anyhow::bail!("handler failed")
                }
                Ok(())
            })
        }
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    #[tokio::test]
    async fn runs_a_job_and_marks_it_done() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let kind = leak(format!("test-{}", uuid::Uuid::new_v4()));
        let handler = Arc::new(Counter {
            kind,
            calls: AtomicUsize::new(0),
            fail: false,
        });
        let worker = Worker::new(pool.clone()).register(handler.clone());
        let id = jobs::enqueue(&pool, org, kind, json!({})).await.unwrap();

        assert!(worker.run_once().await.unwrap());
        assert!(!worker.run_once().await.unwrap(), "nothing left");
        assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
        let status: String = sqlx::query_scalar("SELECT status::text FROM job WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "done");
    }

    #[tokio::test]
    async fn failed_job_is_queued_for_retry_with_the_error() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let kind = leak(format!("test-{}", uuid::Uuid::new_v4()));
        let worker = Worker::new(pool.clone()).register(Arc::new(Counter {
            kind,
            calls: AtomicUsize::new(0),
            fail: true,
        }));
        let id = jobs::enqueue(&pool, org, kind, json!({})).await.unwrap();

        assert!(worker.run_once().await.unwrap());
        let (status, error): (String, Option<String>) =
            sqlx::query_as("SELECT status::text, last_error FROM job WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "queued");
        assert_eq!(error.as_deref(), Some("handler failed"));
    }
}
