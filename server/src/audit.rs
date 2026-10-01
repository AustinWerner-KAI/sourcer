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
    pub const CLIENT_CREATED: &str = "client.create";
    pub const ROLE_CREATED: &str = "role.create";
    pub const ROLE_CLIENT_SET: &str = "role.client_set";
    pub const BRIEF_DRAFTED: &str = "brief.draft";
    pub const BRIEF_SAVED: &str = "brief.save";
    pub const BRIEF_CONFIRMED: &str = "brief.confirm";
    pub const SEARCH_COUNTED: &str = "search.count";
    pub const SEARCH_RETUNED: &str = "search.retune";
    pub const SEARCH_PULLED: &str = "search.pull";
    pub const CANDIDATES_RANKED: &str = "candidates.rank";
    pub const CANDIDATE_SHORTLISTED: &str = "candidate.shortlist";
    pub const CANDIDATE_REJECTED: &str = "candidate.reject";
    pub const CANDIDATE_RECONSIDERED: &str = "candidate.reconsider";
    pub const PROFILE_SAVED: &str = "person.save_linkedin";
    pub const PEOPLE_MERGED: &str = "person.merge";
    pub const ROLE_IMPORTED: &str = "role.recruitly_import";
    pub const ROLE_JOB_LINKED: &str = "role.recruitly_link";
    pub const PERSON_RECRUITLY_CHECKED: &str = "person.recruitly_check";
    pub const CANDIDATE_HANDED_OVER: &str = "candidate.handover";
    pub const CV_ASSESSED: &str = "cv.assess";
    pub const CV_FEEDBACK: &str = "cv.feedback";
    pub const CV_NOTED: &str = "cv.recruitly_note";
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
