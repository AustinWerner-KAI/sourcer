//! Run a people search for a role and save the results (SRS F5, N11, N14).
//!
//! - Refuses when the organisation has paused paid calls.
//! - The same idempotency key never runs or charges twice.
//! - People are merged by provider id or LinkedIn URL, so a person found twice
//!   is one record, with one candidacy per role.
//! - Staff of the hiring client and of off-limits clients are never saved,
//!   whatever the query said.

use anyhow::{bail, Context, Result};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    audit,
    employer::{self, Verdict},
    policy::normalise_identifier,
    sources::{PeopleSource, PersonRecord, SearchQuery},
};

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub org_id: Uuid,
    pub actor_id: Option<Uuid>,
    pub role_id: Uuid,
    pub brief_id: Uuid,
    pub query: SearchQuery,
    /// Same key = same search. A retried click or job reuses it.
    pub idempotency_key: String,
    /// The pull this search belongs to, and its location label.
    pub pull_id: Option<Uuid>,
    pub location: Option<String>,
    /// The wider search this belongs to, or `None` for the brief's own.
    pub search_id: Option<Uuid>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SearchOutcome {
    Ran {
        run_id: Uuid,
        total: u64,
        pulled: usize,
        credits: u32,
        new_candidates: usize,
        /// Records dropped because they work at the hiring or an off-limits client.
        left_out: usize,
        /// New candidates with no known current employer, to check before contact.
        unknown_employer: usize,
    },
    AlreadyRan {
        run_id: Uuid,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct PaidCallsPaused;

impl std::fmt::Display for PaidCallsPaused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("paid calls are paused for this organisation")
    }
}

impl std::error::Error for PaidCallsPaused {}

pub async fn run_search<S: PeopleSource>(
    pool: &PgPool,
    source: &S,
    req: &SearchRequest,
) -> Result<SearchOutcome> {
    let paused: bool = sqlx::query_scalar("SELECT paid_calls_paused FROM org WHERE id = $1")
        .bind(req.org_id)
        .fetch_one(pool)
        .await
        .context("organisation not found")?;
    if paused {
        bail!(PaidCallsPaused);
    }
    // Without a client there is no one to lock out, so never search.
    let has_client: bool =
        sqlx::query_scalar("SELECT client_id IS NOT NULL FROM role WHERE id = $1 AND org_id = $2")
            .bind(req.role_id)
            .bind(req.org_id)
            .fetch_optional(pool)
            .await?
            .unwrap_or(false);
    if !has_client {
        bail!("this role has no client, so it cannot be searched");
    }
    let locked_out = employer::locked_out(pool, req.org_id, req.role_id).await?;

    let Some(run_id) = reserve_run(pool, req).await? else {
        let (run_id, finished): (Uuid, bool) = sqlx::query_as(
            "SELECT id, finished_at IS NOT NULL FROM run WHERE org_id = $1 AND idempotency_key = $2",
        )
        .bind(req.org_id)
        .bind(&req.idempotency_key)
        .fetch_one(pool)
        .await?;
        // An earlier try was charged but did not finish saving. Calling the
        // provider again would charge twice, so report it as failed instead.
        if !finished {
            bail!("run {run_id} was charged but did not finish saving");
        }
        return Ok(SearchOutcome::AlreadyRan { run_id });
    };

    let page = match source.search(&req.query).await {
        Ok(page) => page,
        Err(e) => {
            // Nothing was saved, so free the key and let a retry run.
            sqlx::query("DELETE FROM run WHERE id = $1")
                .bind(run_id)
                .execute(pool)
                .await?;
            return Err(e).context(format!("{} search failed", source.name()));
        }
    };
    // Record the charge at once, so it counts even if saving fails below.
    sqlx::query("UPDATE run SET records_pulled = $2, credits_used = $3 WHERE id = $1")
        .bind(run_id)
        .bind(page.records.len() as i32)
        .bind(page.credits_used as i32)
        .execute(pool)
        .await?;

    let mut tx = pool.begin().await?;
    let mut new_candidates = 0;
    let mut left_out = 0;
    let mut unknown_employer = 0;
    for record in &page.records {
        let current = std::iter::once((
            record.current_employer.as_deref().unwrap_or(""),
            record.current_employer_domain.as_deref(),
        ))
        .chain(
            record
                .experience
                .iter()
                .filter(|x| x.end.is_none())
                .map(|x| (x.employer.as_str(), x.employer_domain.as_deref())),
        );
        let verdict = employer::check(current, &locked_out);
        // Always refresh what we know about the person, so someone saved
        // earlier who has since joined the client is now seen as locked out.
        let person_id = upsert_person(&mut tx, req.org_id, record).await?;
        if verdict == Verdict::LockedOut {
            left_out += 1;
            continue;
        }
        save_contacts(&mut tx, req.org_id, person_id, record).await?;
        let inserted = sqlx::query(
            "INSERT INTO candidacy (org_id, person_id, role_id, brief_id, employer_unknown, search_id)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (person_id, role_id) DO NOTHING",
        )
        .bind(req.org_id)
        .bind(person_id)
        .bind(req.role_id)
        .bind(req.brief_id)
        .bind(verdict == Verdict::Unknown)
        .bind(req.search_id)
        .execute(&mut *tx)
        .await?;
        let added = inserted.rows_affected() as usize;
        new_candidates += added;
        if verdict == Verdict::Unknown {
            unknown_employer += added;
        }
    }
    sqlx::query(
        "UPDATE run SET records_pulled = $2, credits_used = $3, new_candidates = $4,
                        left_out = $5, unknown_employer = $6, finished_at = now()
         WHERE id = $1",
    )
    .bind(run_id)
    .bind(page.records.len() as i32)
    .bind(page.credits_used as i32)
    .bind(new_candidates as i32)
    .bind(left_out as i32)
    .bind(unknown_employer as i32)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        req.org_id,
        req.actor_id,
        audit::action::SEARCH_RUN,
        &format!("run:{run_id}"),
    )
    .await?;
    tx.commit().await?;

    Ok(SearchOutcome::Ran {
        run_id,
        total: page.total,
        pulled: page.records.len(),
        credits: page.credits_used,
        new_candidates,
        left_out,
        unknown_employer,
    })
}

/// Insert the run row, or `None` if this idempotency key has already run.
async fn reserve_run(pool: &PgPool, req: &SearchRequest) -> Result<Option<Uuid>> {
    let id = sqlx::query_scalar(
        "INSERT INTO run (org_id, role_id, brief_id, queries, idempotency_key, pull_id, location)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (org_id, idempotency_key) DO NOTHING
         RETURNING id",
    )
    .bind(req.org_id)
    .bind(req.role_id)
    .bind(req.brief_id)
    .bind(serde_json::to_value(&req.query)?)
    .bind(&req.idempotency_key)
    .bind(req.pull_id)
    .bind(&req.location)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

/// Find the person by provider id or LinkedIn URL, refresh them, or add them.
/// Work history is replaced with the latest the provider holds.
async fn upsert_person(
    tx: &mut Transaction<'_, Postgres>,
    org_id: Uuid,
    r: &PersonRecord,
) -> Result<Uuid> {
    let linkedin = r.linkedin_url.as_deref().map(normalise_identifier);
    let by_pdl: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM person WHERE org_id = $1 AND pdl_id = $2")
            .bind(org_id)
            .bind(&r.source_id)
            .fetch_optional(&mut **tx)
            .await?;
    let by_linkedin: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM person WHERE org_id = $1 AND linkedin_url = $2")
            .bind(org_id)
            .bind(&linkedin)
            .fetch_optional(&mut **tx)
            .await?;
    let existing = match (by_pdl, by_linkedin) {
        // Found once by a search and once from LinkedIn: the same person, so
        // one record, or they could be approached twice for one role.
        (Some(pdl), Some(li)) if pdl != li => {
            merge_people(tx, org_id, li, pdl).await?;
            Some(li)
        }
        (Some(id), _) | (None, Some(id)) => Some(id),
        (None, None) => None,
    };

    let person_id = match existing {
        Some(id) => {
            sqlx::query(
                "UPDATE person SET
                   pdl_id = COALESCE(pdl_id, $2),
                   linkedin_url = COALESCE(linkedin_url, $3),
                   full_name = $4,
                   current_title = COALESCE($5, current_title),
                   current_employer = COALESCE($6, current_employer),
                   current_employer_domain = CASE WHEN $6 IS NOT NULL THEN $8 ELSE current_employer_domain END,
                   location = COALESCE($7, location),
                   skills = CASE WHEN jsonb_array_length($9) > 0 THEN $9 ELSE skills END,
                   last_seen = now()
                 WHERE id = $1",
            )
            .bind(id)
            .bind(&r.source_id)
            .bind(&linkedin)
            .bind(&r.full_name)
            .bind(&r.current_title)
            .bind(&r.current_employer)
            .bind(&r.location)
            .bind(&r.current_employer_domain)
            .bind(sqlx::types::Json(&r.skills))
            .execute(&mut **tx)
            .await?;
            id
        }
        None => {
            sqlx::query_scalar(
                "INSERT INTO person (org_id, pdl_id, linkedin_url, full_name, current_title,
                                     current_employer, location, current_employer_domain, skills, last_seen)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
                 RETURNING id",
            )
            .bind(org_id)
            .bind(&r.source_id)
            .bind(&linkedin)
            .bind(&r.full_name)
            .bind(&r.current_title)
            .bind(&r.current_employer)
            .bind(&r.location)
            .bind(&r.current_employer_domain)
            .bind(sqlx::types::Json(&r.skills))
            .fetch_one(&mut **tx)
            .await?
        }
    };

    if !r.experience.is_empty() {
        sqlx::query("DELETE FROM employment WHERE person_id = $1")
            .bind(person_id)
            .execute(&mut **tx)
            .await?;
        for x in &r.experience {
            sqlx::query(
                "INSERT INTO employment (org_id, person_id, employer, title, start_date, end_date, employer_domain)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(org_id)
            .bind(person_id)
            .bind(&x.employer)
            .bind(&x.title)
            .bind(x.start.as_deref().and_then(to_date))
            .bind(x.end.as_deref().and_then(to_date))
            .bind(&x.employer_domain)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(person_id)
}

/// Fold `drop` into `keep` (same org). Where both are on the same role, the
/// candidacy further along is kept, so no decision or outreach is lost.
async fn merge_people(
    tx: &mut Transaction<'_, Postgres>,
    org_id: Uuid,
    keep: Uuid,
    drop: Uuid,
) -> Result<()> {
    let progress = |alias: &str| {
        format!(
            "array_position(ARRAY['found', 'known_checked', 'ranked', 'rejected', 'shortlisted',
             'drafted', 'approved', 'contacted', 'no_reply', 'replied', 'handed_to_ats']::candidacy_state[],
             {alias}.state)"
        )
    };
    // Of each pair on the same role, remove the one less far along.
    sqlx::query(&format!(
        "DELETE FROM candidacy c USING candidacy k, candidacy d
         WHERE k.person_id = $1 AND d.person_id = $2 AND k.role_id = d.role_id
           AND c.id = CASE WHEN {} > {} THEN k.id ELSE d.id END",
        progress("d"),
        progress("k")
    ))
    .bind(keep)
    .bind(drop)
    .execute(&mut **tx)
    .await?;
    for table in ["candidacy", "contact", "touch"] {
        sqlx::query(&format!(
            "UPDATE {table} SET person_id = $1 WHERE person_id = $2 AND org_id = $3"
        ))
        .bind(keep)
        .bind(drop)
        .bind(org_id)
        .execute(&mut **tx)
        .await?;
    }
    // An opt-out on either record stays an opt-out, here and in Recruitly.
    let (pdl_id, opted_out, recruitly_id, recruitly_dnc): (
        Option<String>,
        bool,
        Option<String>,
        bool,
    ) = sqlx::query_as(
        "DELETE FROM person WHERE id = $1 AND org_id = $2
             RETURNING pdl_id, opted_out, recruitly_id, recruitly_dnc",
    )
    .bind(drop)
    .bind(org_id)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE person SET pdl_id = COALESCE(pdl_id, $2), opted_out = opted_out OR $3,
                recruitly_id = COALESCE(recruitly_id, $4), recruitly_dnc = recruitly_dnc OR $5
         WHERE id = $1",
    )
    .bind(keep)
    .bind(pdl_id)
    .bind(opted_out)
    .bind(recruitly_id)
    .bind(recruitly_dnc)
    .execute(&mut **tx)
    .await?;
    audit::record(
        &mut **tx,
        org_id,
        None,
        audit::action::PEOPLE_MERGED,
        &format!("person:{keep} merged:{drop}"),
    )
    .await?;
    Ok(())
}

/// Keep the work email, personal emails and phone numbers the provider gave.
/// A value already held for another person is left with them.
async fn save_contacts(
    tx: &mut Transaction<'_, Postgres>,
    org_id: Uuid,
    person_id: Uuid,
    r: &PersonRecord,
) -> Result<()> {
    let found = r
        .work_email
        .iter()
        .map(|e| ("work_email", e.to_lowercase()))
        .chain(
            r.personal_emails
                .iter()
                .map(|e| ("personal_email", e.to_lowercase())),
        )
        .chain(r.phones.iter().map(|p| ("phone", p.clone())));
    for (kind, value) in found {
        sqlx::query(
            "INSERT INTO contact (org_id, person_id, kind, value, source)
             VALUES ($1, $2, $3::contact_kind, $4, 'pdl')
             ON CONFLICT (org_id, kind, value) DO NOTHING",
        )
        .bind(org_id)
        .bind(person_id)
        .bind(kind)
        .bind(value)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// "2021", "2021-03" or "2021-03-15" to a date (first of the month or year).
fn to_date(s: &str) -> Option<chrono::NaiveDate> {
    let mut parts = s.split('-').map(|p| p.parse::<u32>().ok());
    let year = parts.next()?? as i32;
    let month = parts.next().flatten().unwrap_or(1);
    let day = parts.next().flatten().unwrap_or(1);
    chrono::NaiveDate::from_ymd_opt(year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::{ExperienceRecord, SearchPage, SourceError};
    use crate::testutil;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn provider_dates_parse() {
        let d = |y, m, dd| chrono::NaiveDate::from_ymd_opt(y, m, dd);
        assert_eq!(to_date("2021"), d(2021, 1, 1));
        assert_eq!(to_date("2021-03"), d(2021, 3, 1));
        assert_eq!(to_date("2021-03-15"), d(2021, 3, 15));
        assert_eq!(to_date("soon"), None);
        assert_eq!(to_date("2021-13"), None);
    }

    struct FakeSource {
        records: Vec<PersonRecord>,
        calls: AtomicUsize,
        fail: bool,
    }

    impl FakeSource {
        fn new(records: Vec<PersonRecord>) -> Self {
            Self {
                records,
                calls: AtomicUsize::new(0),
                fail: false,
            }
        }
    }

    impl PeopleSource for FakeSource {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn estimate_credits(&self, q: &SearchQuery) -> u32 {
            q.size
        }
        async fn search(&self, _q: &SearchQuery) -> Result<SearchPage, SourceError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(SourceError::Network("down".into()));
            }
            Ok(SearchPage {
                total: 99,
                records: self.records.clone(),
                scroll_token: None,
                credits_used: self.records.len().max(1) as u32,
            })
        }
    }

    fn person(id: &str, linkedin: &str) -> PersonRecord {
        PersonRecord {
            source_id: id.into(),
            full_name: format!("Person {id}"),
            current_title: Some("Senior Security Engineer".into()),
            current_employer: Some("ExamplePay".into()),
            current_employer_domain: None,
            location: None,
            linkedin_url: Some(linkedin.into()),
            experience: vec![ExperienceRecord {
                employer: "ExamplePay".into(),
                employer_domain: None,
                title: Some("Senior Security Engineer".into()),
                start: Some("2022-01".into()),
                end: None,
            }],
            work_email: None,
            personal_emails: vec![],
            phones: vec![],
            skills: vec![],
        }
    }

    async fn setup() -> Option<(PgPool, SearchRequest)> {
        let pool = testutil::pool().await?;
        let org = testutil::org(&pool).await;
        let (role, brief) = testutil::role_with_brief(&pool, org).await;
        let req = SearchRequest {
            org_id: org,
            actor_id: None,
            role_id: role,
            brief_id: brief,
            query: SearchQuery {
                query: serde_json::json!({"match_all": {}}),
                size: 2,
                scroll_token: None,
            },
            idempotency_key: format!("k-{}", Uuid::new_v4()),
            pull_id: None,
            location: None,
            search_id: None,
        };
        Some((pool, req))
    }

    async fn count(pool: &PgPool, sql: &str, org: Uuid) -> i64 {
        sqlx::query_scalar(sql)
            .bind(org)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn hiring_client_and_off_limits_staff_are_never_saved() {
        let Some((pool, req)) = setup().await else {
            return;
        };
        // The role is for ExamplePay; OtherBank is off-limits for every role.
        let client: Uuid = sqlx::query_scalar(
            "INSERT INTO client (org_id, name, domain) VALUES ($1, 'ExamplePay Inc.', 'examplepay.com') RETURNING id",
        )
        .bind(req.org_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO client (org_id, name, off_limits) VALUES ($1, 'OtherBank', true)")
            .bind(req.org_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE role SET client_id = $2 WHERE id = $1")
            .bind(req.role_id)
            .bind(client)
            .execute(&pool)
            .await
            .unwrap();
        let mut at_bank = person("p2", "linkedin.com/in/p-two");
        at_bank.work_email = Some("p2@otherbank.com".into());
        at_bank.current_employer = Some("otherbank".into());
        at_bank.experience.clear();
        let mut by_domain = person("p3", "linkedin.com/in/p-three");
        by_domain.current_employer = Some("EP Global".into());
        by_domain.current_employer_domain = Some("careers.examplepay.com".into());
        by_domain.experience.clear();
        // Headline job elsewhere, but still has a current job at the client.
        let mut second_job = person("p4", "linkedin.com/in/p-four");
        second_job.current_employer = Some("Kraken".into());
        let mut elsewhere = person("p5", "linkedin.com/in/p-five");
        elsewhere.current_employer = Some("Kraken".into());
        elsewhere.experience.clear();
        let mut unknown = person("p6", "linkedin.com/in/p-six");
        unknown.current_employer = None;
        unknown.experience.clear();
        let src = FakeSource::new(vec![
            person("p1", "linkedin.com/in/p-one"),
            at_bank,
            by_domain,
            second_job,
            elsewhere,
            unknown,
        ]);

        let out = run_search(&pool, &src, &req).await.unwrap();
        assert!(matches!(
            out,
            SearchOutcome::Ran {
                pulled: 6,
                new_candidates: 2,
                left_out: 4,
                unknown_employer: 1,
                ..
            }
        ));
        let saved: Vec<(String, bool)> = sqlx::query_as(
            "SELECT p.full_name, c.employer_unknown FROM candidacy c JOIN person p ON p.id = c.person_id
             WHERE c.role_id = $1 ORDER BY p.full_name",
        )
        .bind(req.role_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            saved,
            [
                ("Person p5".to_string(), false),
                ("Person p6".to_string(), true)
            ],
            "only the Kraken person, and the unknown one flagged for a check"
        );
        let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM contact WHERE org_id = $1")
            .bind(req.org_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kept, 0, "no contact details kept for locked-out people");
    }

    #[tokio::test]
    async fn someone_who_joins_the_client_later_is_then_locked_out() {
        let Some((pool, req)) = setup().await else {
            return;
        };
        let mut jane = person("p1", "linkedin.com/in/jane");
        jane.current_employer = Some("Kraken".into());
        jane.experience.clear();
        run_search(&pool, &FakeSource::new(vec![jane.clone()]), &req)
            .await
            .unwrap();
        let person: Uuid = sqlx::query_scalar("SELECT id FROM person WHERE org_id = $1")
            .bind(req.org_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let check = || employer::check_person(&pool, req.org_id, req.role_id, person);
        assert_eq!(check().await.unwrap(), Verdict::Clear);

        // She joins the role's client (testutil's "Test Client"); a later search sees it.
        jane.current_employer = Some("Test Client".into());
        let again = SearchRequest {
            idempotency_key: format!("k-{}", Uuid::new_v4()),
            ..req.clone()
        };
        run_search(&pool, &FakeSource::new(vec![jane]), &again)
            .await
            .unwrap();
        assert_eq!(check().await.unwrap(), Verdict::LockedOut);
    }

    #[tokio::test]
    async fn a_role_without_a_client_is_never_searched() {
        let Some((pool, req)) = setup().await else {
            return;
        };
        sqlx::query("UPDATE role SET client_id = NULL WHERE id = $1")
            .bind(req.role_id)
            .execute(&pool)
            .await
            .unwrap();
        let src = FakeSource::new(vec![person("p1", "linkedin.com/in/p-one")]);
        assert!(run_search(&pool, &src, &req).await.is_err());
        assert_eq!(src.calls.load(Ordering::SeqCst), 0, "no credits spent");
    }

    #[tokio::test]
    async fn saves_people_candidacies_run_and_audit() {
        let Some((pool, req)) = setup().await else {
            return;
        };
        let mut with_contacts = person("p1", "https://www.linkedin.com/in/p-one/");
        with_contacts.work_email = Some("P1@ExamplePay.com".into());
        with_contacts.personal_emails = vec!["P1.Home@Gmail.com".into()];
        with_contacts.phones = vec!["+971 50 000 0000".into()];
        let src = FakeSource::new(vec![with_contacts, person("p2", "linkedin.com/in/p-two")]);

        let out = run_search(&pool, &src, &req).await.unwrap();
        let SearchOutcome::Ran {
            run_id,
            total,
            pulled,
            credits,
            new_candidates,
            ..
        } = out
        else {
            panic!("expected a run")
        };
        assert_eq!((total, pulled, credits, new_candidates), (99, 2, 2, 2));

        let o = req.org_id;
        assert_eq!(
            count(&pool, "SELECT count(*) FROM person WHERE org_id = $1", o).await,
            2
        );
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM employment WHERE org_id = $1",
                o
            )
            .await,
            2
        );
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM candidacy WHERE org_id = $1 AND state = 'found'",
                o
            )
            .await,
            2
        );
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'search.run'",
                o
            )
            .await,
            1
        );
        let (pulled, credits): (i32, i32) =
            sqlx::query_as("SELECT records_pulled, credits_used FROM run WHERE id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((pulled, credits), (2, 2));
        let li: String = sqlx::query_scalar(
            "SELECT linkedin_url FROM person WHERE org_id = $1 AND pdl_id = 'p1'",
        )
        .bind(o)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(li, "linkedin.com/in/p-one", "stored normalised");
        let contacts: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT kind::text AS k, value, source FROM contact WHERE org_id = $1 ORDER BY k DESC",
        )
        .bind(o)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            contacts,
            [
                (
                    "work_email".into(),
                    "p1@examplepay.com".into(),
                    "pdl".into()
                ),
                ("phone".into(), "+971 50 000 0000".into(), "pdl".into()),
                (
                    "personal_email".into(),
                    "p1.home@gmail.com".into(),
                    "pdl".into()
                )
            ]
        );
    }

    #[tokio::test]
    async fn same_key_never_runs_or_charges_twice() {
        let Some((pool, req)) = setup().await else {
            return;
        };
        let src = FakeSource::new(vec![person("p1", "linkedin.com/in/p-one")]);

        let first = run_search(&pool, &src, &req).await.unwrap();
        let second = run_search(&pool, &src, &req).await.unwrap();
        let SearchOutcome::Ran { run_id, .. } = first else {
            panic!()
        };
        assert_eq!(second, SearchOutcome::AlreadyRan { run_id });
        assert_eq!(src.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_charged_run_that_did_not_finish_is_not_charged_again() {
        let Some((pool, req)) = setup().await else {
            return;
        };
        let src = FakeSource::new(vec![person("p1", "linkedin.com/in/p-one")]);
        run_search(&pool, &src, &req).await.unwrap();
        // As if the save had failed after the provider charged.
        sqlx::query("UPDATE run SET finished_at = NULL WHERE idempotency_key = $1")
            .bind(&req.idempotency_key)
            .execute(&pool)
            .await
            .unwrap();
        assert!(run_search(&pool, &src, &req).await.is_err());
        assert_eq!(src.calls.load(Ordering::SeqCst), 1, "never charged twice");
        let credits: i32 =
            sqlx::query_scalar("SELECT credits_used FROM run WHERE idempotency_key = $1")
                .bind(&req.idempotency_key)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(credits, 1, "the charge is still counted");
    }

    #[tokio::test]
    async fn person_found_again_is_merged_not_duplicated() {
        let Some((pool, req)) = setup().await else {
            return;
        };
        let src = FakeSource::new(vec![person("p1", "linkedin.com/in/p-one")]);
        run_search(&pool, &src, &req).await.unwrap();

        // Found again in a new search, under a different provider id but the same LinkedIn.
        let again = FakeSource::new(vec![person("p1-new", "https://linkedin.com/in/P-One")]);
        let req2 = SearchRequest {
            idempotency_key: format!("k-{}", Uuid::new_v4()),
            ..req.clone()
        };
        let out = run_search(&pool, &again, &req2).await.unwrap();

        assert!(matches!(
            out,
            SearchOutcome::Ran {
                new_candidates: 0,
                ..
            }
        ));
        let o = req.org_id;
        assert_eq!(
            count(&pool, "SELECT count(*) FROM person WHERE org_id = $1", o).await,
            1
        );
        assert_eq!(
            count(&pool, "SELECT count(*) FROM candidacy WHERE org_id = $1", o).await,
            1
        );
    }

    #[tokio::test]
    async fn a_person_saved_from_linkedin_and_found_by_a_search_becomes_one_record() {
        let Some((pool, req)) = setup().await else {
            return;
        };
        let o = req.org_id;
        // Saved from LinkedIn and shortlisted, with no PDL id.
        let saved: Uuid = sqlx::query_scalar(
            "INSERT INTO person (org_id, linkedin_url, full_name) VALUES ($1, 'linkedin.com/in/jane', 'Jane') RETURNING id",
        )
        .bind(o)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO candidacy (org_id, person_id, role_id, brief_id, state) VALUES ($1, $2, $3, $4, 'shortlisted')",
        )
        .bind(o)
        .bind(saved)
        .bind(req.role_id)
        .bind(req.brief_id)
        .execute(&pool)
        .await
        .unwrap();
        // A search finds her without a LinkedIn address, so she looks new.
        let mut jane = person("p1", "linkedin.com/in/jane");
        jane.current_employer = Some("Kraken".into());
        jane.linkedin_url = None;
        jane.phones = vec!["+971 50 111 1111".into()];
        run_search(&pool, &FakeSource::new(vec![jane.clone()]), &req)
            .await
            .unwrap();
        assert_eq!(
            count(&pool, "SELECT count(*) FROM person WHERE org_id = $1", o).await,
            2
        );
        // A later search returns her LinkedIn address: the two are merged.
        jane.linkedin_url = Some("https://www.linkedin.com/in/jane/".into());
        let again = SearchRequest {
            idempotency_key: format!("k-{}", Uuid::new_v4()),
            ..req.clone()
        };
        run_search(&pool, &FakeSource::new(vec![jane]), &again)
            .await
            .unwrap();
        let people: Vec<(Uuid, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT id, pdl_id, linkedin_url FROM person WHERE org_id = $1")
                .bind(o)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            people,
            [(
                saved,
                Some("p1".into()),
                Some("linkedin.com/in/jane".into())
            )]
        );
        let states: Vec<String> = sqlx::query_scalar(
            "SELECT state::text FROM candidacy WHERE org_id = $1 AND role_id = $2",
        )
        .bind(o)
        .bind(req.role_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            states,
            ["shortlisted"],
            "one candidacy, the one further along"
        );
        assert_eq!(
            count(&pool, "SELECT count(*) FROM contact WHERE org_id = $1", o).await,
            1,
            "contact details moved over"
        );
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'person.merge'",
                o
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn paused_org_makes_no_call() {
        let Some((pool, req)) = setup().await else {
            return;
        };
        sqlx::query("UPDATE org SET paid_calls_paused = true WHERE id = $1")
            .bind(req.org_id)
            .execute(&pool)
            .await
            .unwrap();
        let src = FakeSource::new(vec![person("p1", "linkedin.com/in/p-one")]);

        let err = run_search(&pool, &src, &req).await.unwrap_err();
        assert!(err.downcast_ref::<PaidCallsPaused>().is_some());
        assert_eq!(src.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn provider_failure_frees_the_key_for_a_retry() {
        let Some((pool, req)) = setup().await else {
            return;
        };
        let mut src = FakeSource::new(vec![person("p1", "linkedin.com/in/p-one")]);
        src.fail = true;
        assert!(run_search(&pool, &src, &req).await.is_err());
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM run WHERE org_id = $1",
                req.org_id
            )
            .await,
            0
        );

        src.fail = false;
        assert!(matches!(
            run_search(&pool, &src, &req).await.unwrap(),
            SearchOutcome::Ran { pulled: 1, .. }
        ));
    }
}
