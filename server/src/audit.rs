//! Append-only audit log (SRS N6). Every change to a candidate and every send
//! is recorded here. Takes any executor so it can join the caller's transaction.

use anyhow::Result;
use sqlx::PgExecutor;
use uuid::Uuid;

/// Action names, kept in one place so they stay consistent across the codebase.
pub mod action {
    pub const SEARCH_RUN: &str = "search.run";
    pub const JOB_FAILED: &str = "job.failed";
    pub const SIGN_IN: &str = "auth.sign_in";
    pub const SIGN_IN_REFUSED: &str = "auth.sign_in_refused";
    pub const USER_INVITED: &str = "team.invite";
    pub const USER_DISABLED: &str = "team.disable";
    pub const USER_ENABLED: &str = "team.enable";
}

/// Write one audit entry. `actor_id` is `None` for work the system did itself.
pub async fn record<'e, E: PgExecutor<'e>>(
    exec: E,
    org_id: Uuid,
    actor_id: Option<Uuid>,
    action: &str,
    target: &str,
) -> Result<()> {
    sqlx::query("INSERT INTO audit (org_id, actor_id, action, target) VALUES ($1, $2, $3, $4)")
        .bind(org_id)
        .bind(actor_id)
        .bind(action)
        .bind(target)
        .execute(exec)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn records_an_entry() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        record(&pool, org, None, action::SEARCH_RUN, "run:1")
            .await
            .unwrap();
        let (a, t): (String, String) =
            sqlx::query_as("SELECT action, target FROM audit WHERE org_id = $1")
                .bind(org)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((a.as_str(), t.as_str()), (action::SEARCH_RUN, "run:1"));
    }
}
