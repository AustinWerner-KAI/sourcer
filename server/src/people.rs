//! Save a LinkedIn profile the resourcer is looking at to a role (SRS F10).
//!
//! The browser button reads only the page in front of the resourcer, and only
//! when they click. Nothing here visits LinkedIn. The details are checked by
//! the resourcer before saving, and the same rules as a search apply: staff of
//! the hiring client and off-limits clients are never added.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use uuid::Uuid;

use crate::{
    app::AppState,
    audit,
    auth::CurrentUser,
    candidates::{one_row, queue_rank},
    domain::{SaveProfile, SavedProfile},
    employer::{self, Verdict},
    policy::normalise_identifier,
    searching::confirmed_brief,
};

const MAX_FIELD_CHARS: usize = 200;

fn refuse(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, msg.into()).into_response()
}

fn server_error(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "saving a profile failed");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

/// "linkedin.com/in/someone" from any form of a profile address, or `None`
/// if it is not a LinkedIn profile.
pub fn profile_url(raw: &str) -> Option<String> {
    let s = normalise_identifier(raw.split(['?', '#']).next().unwrap_or(""));
    // Country sites such as uk.linkedin.com are the same profile.
    let s = match s.split_once('.') {
        Some((sub, rest)) if rest.starts_with("linkedin.com/") && sub.len() <= 3 => {
            rest.to_string()
        }
        _ => s,
    };
    // Sub-pages such as ".../in/someone/details/experience" are the same person.
    let handle = s.strip_prefix("linkedin.com/in/")?.split('/').next()?;
    let ok = !handle.is_empty()
        && handle.len() <= 100
        && handle
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '%');
    ok.then(|| format!("linkedin.com/in/{handle}"))
}

/// Trimmed and capped, or `None` when blank.
fn field(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(MAX_FIELD_CHARS).collect())
}

/// POST /api/people/save
pub async fn save(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<SaveProfile>,
) -> Response {
    let Some(pool) = state.pool.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(linkedin) = profile_url(&req.linkedin_url) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "That is not a LinkedIn profile address.",
        );
    };
    let Some(name) = field(&Some(req.name.clone())) else {
        return refuse(StatusCode::BAD_REQUEST, "Add the person's name.");
    };
    let (title, employer_name, location) = (
        field(&req.title),
        field(&req.employer),
        field(&req.location),
    );

    let has_client: Result<Option<bool>, _> =
        sqlx::query_scalar("SELECT client_id IS NOT NULL FROM role WHERE id = $1 AND org_id = $2")
            .bind(req.role_id)
            .bind(user.org_id)
            .fetch_optional(&pool)
            .await;
    match has_client {
        Ok(Some(true)) => {}
        Ok(Some(false)) => {
            return refuse(
                StatusCode::CONFLICT,
                "Choose the client for this role first, so their staff are kept out.",
            )
        }
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return server_error(e),
    }
    let brief = match confirmed_brief(&pool, req.role_id).await {
        Ok(Some((id, _, _))) => id,
        Ok(None) => return refuse(StatusCode::CONFLICT, "Confirm this role's brief first."),
        Err(e) => return server_error(e),
    };

    // Check before writing anything: the employer on the page, plus any
    // current job already on record for this person.
    let known_jobs: Result<Vec<(String, Option<String>)>, _> = sqlx::query_as(
        "SELECT coalesce(p.current_employer, ''), p.current_employer_domain FROM person p
         WHERE p.org_id = $1 AND p.linkedin_url = $2
         UNION ALL
         SELECT e.employer, e.employer_domain FROM employment e JOIN person p ON p.id = e.person_id
         WHERE p.org_id = $1 AND p.linkedin_url = $2 AND e.end_date IS NULL",
    )
    .bind(user.org_id)
    .bind(&linkedin)
    .fetch_all(&pool)
    .await;
    let known_jobs = match known_jobs {
        Ok(j) => j,
        Err(e) => return server_error(e),
    };
    let locked = match employer::locked_out(&pool, user.org_id, req.role_id).await {
        Ok(l) => l,
        Err(e) => return server_error(e),
    };
    let jobs = employer_name
        .iter()
        .map(|n| (n.as_str(), None))
        .chain(known_jobs.iter().map(|(n, d)| (n.as_str(), d.as_deref())));
    let verdict = employer::check(jobs, &locked);
    if verdict == Verdict::LockedOut {
        return refuse(
            StatusCode::CONFLICT,
            "Works at a company that is locked out for this role. Not saved.",
        );
    }

    let result = async {
        let mut tx = pool.begin().await?;
        let person: Uuid = sqlx::query_scalar(
            "INSERT INTO person (org_id, linkedin_url, full_name, current_title, current_employer,
                                 location, last_seen)
             VALUES ($1, $2, $3, $4, $5, $6, now())
             ON CONFLICT (org_id, linkedin_url) WHERE linkedin_url IS NOT NULL DO UPDATE SET
               -- A record from People Data Labs is better than a headline:
               -- only fill its gaps, and never drop its employer domain.
               full_name = CASE WHEN person.pdl_id IS NULL THEN EXCLUDED.full_name
                 ELSE person.full_name END,
               current_title = CASE WHEN person.pdl_id IS NULL
                 THEN COALESCE(EXCLUDED.current_title, person.current_title)
                 ELSE COALESCE(person.current_title, EXCLUDED.current_title) END,
               current_employer = CASE WHEN person.pdl_id IS NULL
                 THEN COALESCE(EXCLUDED.current_employer, person.current_employer)
                 ELSE COALESCE(person.current_employer, EXCLUDED.current_employer) END,
               current_employer_domain = CASE WHEN person.pdl_id IS NULL
                 AND EXCLUDED.current_employer IS NOT NULL
                 AND EXCLUDED.current_employer IS DISTINCT FROM person.current_employer
                 THEN NULL ELSE person.current_employer_domain END,
               location = CASE WHEN person.pdl_id IS NULL
                 THEN COALESCE(EXCLUDED.location, person.location)
                 ELSE COALESCE(person.location, EXCLUDED.location) END,
               last_seen = now()
             RETURNING id",
        )
        .bind(user.org_id)
        .bind(&linkedin)
        .bind(&name)
        .bind(&title)
        .bind(&employer_name)
        .bind(&location)
        .fetch_one(&mut *tx)
        .await?;
        let candidacy: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO candidacy (org_id, person_id, role_id, brief_id, employer_unknown)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (person_id, role_id) DO NOTHING RETURNING id",
        )
        .bind(user.org_id)
        .bind(person)
        .bind(req.role_id)
        .bind(brief)
        .bind(verdict == Verdict::Unknown)
        .fetch_optional(&mut *tx)
        .await?;
        let (candidacy, added) = match candidacy {
            Some(id) => (id, true),
            None => (
                sqlx::query_scalar(
                    "SELECT id FROM candidacy WHERE person_id = $1 AND role_id = $2",
                )
                .bind(person)
                .bind(req.role_id)
                .fetch_one(&mut *tx)
                .await?,
                false,
            ),
        };
        audit::record(
            &mut *tx,
            user.org_id,
            Some(user.id),
            audit::action::PROFILE_SAVED,
            &format!("candidacy:{candidacy} role:{} added:{added}", req.role_id),
        )
        .await?;
        tx.commit().await?;
        if added {
            queue_rank(&pool, user.org_id, req.role_id).await?;
        }
        anyhow::Ok((candidacy, added))
    }
    .await;
    match result {
        Ok((candidacy, added)) => match one_row(&pool, user.org_id, candidacy).await {
            Ok(candidate) => Json(SavedProfile { added, candidate }).into_response(),
            Err(e) => server_error(e),
        },
        Err(e) => server_error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::profile_url;

    #[test]
    fn only_linkedin_profiles_are_accepted() {
        for (raw, want) in [
            (
                "https://www.linkedin.com/in/sample-person/",
                Some("linkedin.com/in/sample-person"),
            ),
            (
                "https://uk.linkedin.com/in/Sample_P?trk=x#top",
                Some("linkedin.com/in/sample_p"),
            ),
            ("linkedin.com/in/someone", Some("linkedin.com/in/someone")),
            ("https://www.linkedin.com/company/acme", None),
            ("https://www.linkedin.com/in/", None),
            ("https://evil.example/linkedin.com/in/x", None),
            ("https://notlinkedin.com/in/x", None),
            (
                "https://www.linkedin.com/in/jane/details/experience/",
                Some("linkedin.com/in/jane"),
            ),
            (
                "https://www.linkedin.com/in/jane/overlay/contact-info/",
                Some("linkedin.com/in/jane"),
            ),
            ("javascript:alert(1)", None),
        ] {
            assert_eq!(profile_url(raw).as_deref(), want, "{raw}");
        }
    }
}
