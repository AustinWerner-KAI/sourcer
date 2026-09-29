//! Team management (SRS F1): admins invite people and switch them off or on.
//!
//! Every query is scoped to the admin's own organisation. Changes are audited.
//! Switching someone off ends their open sessions at once. Changes run one at a
//! time per organisation and only by an active admin, and an admin cannot
//! switch themselves off, so there is always at least one active admin.

use axum::{
    extract::{FromRequestParts, Path, State},
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use uuid::Uuid;

use crate::{
    app::AppState,
    audit,
    auth::CurrentUser,
    domain::{MemberStatus, MemberUpdate, NewMember, Role, TeamMember},
};

/// A signed-in admin. Anyone else gets 403 (or 401 when not signed in).
pub struct Admin(pub CurrentUser);

#[axum::async_trait]
impl FromRequestParts<AppState> for Admin {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, StatusCode> {
        let user = CurrentUser::from_request_parts(parts, state).await?;
        if user.role != "admin" {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(Admin(user))
    }
}

type Row = (Uuid, String, String, Role, bool, bool);

/// The `app_user` columns that make a `Row`.
const COLUMNS: &str = "id, name, email, role, ms_oid IS NOT NULL, disabled_at IS NOT NULL";

fn member((id, name, email, role, linked, disabled): Row) -> TeamMember {
    let status = if disabled {
        MemberStatus::Disabled
    } else if linked {
        MemberStatus::Active
    } else {
        MemberStatus::Invited
    };
    TeamMember {
        id,
        name,
        email,
        role,
        status,
    }
}

fn server_error(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "team change failed");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

fn bad_request(msg: &'static str) -> Response {
    (StatusCode::BAD_REQUEST, msg).into_response()
}

/// A plain check that catches typos. Microsoft decides who the address really is.
fn clean_email(raw: &str) -> Option<String> {
    let email = raw.trim().to_lowercase();
    let (local, domain) = email.split_once('@')?;
    let ok = !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && email.len() <= 254
        && !email.chars().any(char::is_whitespace);
    ok.then_some(email)
}

fn clean_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    (!name.is_empty() && name.chars().count() <= 200).then(|| name.to_string())
}

/// GET /api/team: everyone in the admin's organisation.
pub async fn list(State(state): State<AppState>, Admin(admin): Admin) -> Response {
    // The Admin extractor has already needed the database, so it is there.
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let rows: Result<Vec<Row>, _> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM app_user WHERE org_id = $1 ORDER BY disabled_at IS NOT NULL, lower(name)"
    ))
    .bind(admin.org_id)
    .fetch_all(pool)
    .await;
    match rows {
        Ok(rows) => Json(rows.into_iter().map(member).collect::<Vec<_>>()).into_response(),
        Err(e) => server_error(e),
    }
}

/// What a team change came to. Refusals carry the message shown to the admin.
enum Outcome {
    Done(TeamMember),
    Refused(StatusCode, &'static str),
}

impl IntoResponse for Outcome {
    fn into_response(self) -> Response {
        match self {
            Outcome::Done(m) => Json(m).into_response(),
            Outcome::Refused(code, msg) => (code, msg).into_response(),
        }
    }
}

/// Team changes in one organisation run one at a time, and only while the
/// person making them is still an active admin. This stops two admins
/// switching each other off at the same moment.
async fn lock_team(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org_id: Uuid,
    actor_id: Uuid,
) -> anyhow::Result<bool> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('sourcer.team:' || $1::text))")
        .bind(org_id)
        .execute(&mut **tx)
        .await?;
    let still_admin = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM app_user
         WHERE id = $1 AND org_id = $2 AND role = 'admin' AND disabled_at IS NULL)",
    )
    .bind(actor_id)
    .bind(org_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(still_admin)
}

const NOT_ADMIN: Outcome =
    Outcome::Refused(StatusCode::FORBIDDEN, "Only an active admin can do this.");

/// POST /api/team: invite someone. They can sign in once invited.
pub async fn invite(
    State(state): State<AppState>,
    Admin(admin): Admin,
    Json(new): Json<NewMember>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(email) = clean_email(&new.email) else {
        return bad_request("Enter a valid email address.");
    };
    let Some(name) = clean_name(&new.name) else {
        return bad_request("Enter a name.");
    };
    let result = async {
        let mut tx = pool.begin().await?;
        if !lock_team(&mut tx, admin.org_id, admin.id).await? {
            return Ok(NOT_ADMIN);
        }
        // Sign-in finds people by email, so an address may exist only once,
        // whatever its case and whichever organisation holds it.
        let existing: Option<bool> = sqlx::query_scalar(
            "SELECT disabled_at IS NOT NULL FROM app_user WHERE lower(email) = $1 LIMIT 1",
        )
        .bind(&email)
        .fetch_optional(&mut *tx)
        .await?;
        match existing {
            Some(true) => return Ok(Outcome::Refused(
                StatusCode::CONFLICT,
                "That address belongs to someone who is switched off. Switch them back on instead.",
            )),
            Some(false) => {
                return Ok(Outcome::Refused(
                    StatusCode::CONFLICT,
                    "That person is already on the team.",
                ))
            }
            None => {}
        }
        let row: Row = sqlx::query_as(&format!(
            "INSERT INTO app_user (org_id, email, name, role, mailbox_provider)
             VALUES ($1, $2, $3, $4, 'microsoft')
             RETURNING {COLUMNS}"
        ))
        .bind(admin.org_id)
        .bind(&email)
        .bind(&name)
        .bind(new.role)
        .fetch_one(&mut *tx)
        .await?;
        let role = if new.role == Role::Admin {
            "admin"
        } else {
            "resourcer"
        };
        audit::record(
            &mut *tx,
            admin.org_id,
            Some(admin.id),
            audit::action::USER_INVITED,
            &format!("user:{} role:{role}", row.0),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(Outcome::Done(member(row)))
    }
    .await;
    match result {
        Ok(Outcome::Done(m)) => (StatusCode::CREATED, Json(m)).into_response(),
        Ok(refused) => refused.into_response(),
        Err(e) => server_error(e),
    }
}

/// PATCH /api/team/:id: switch someone off (ends their sessions) or back on.
pub async fn update(
    State(state): State<AppState>,
    Admin(admin): Admin,
    Path(id): Path<Uuid>,
    Json(change): Json<MemberUpdate>,
) -> Response {
    let Some(pool) = state.pool.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if id == admin.id && change.disabled {
        return bad_request("You cannot switch yourself off.");
    }
    let result = async {
        let mut tx = pool.begin().await?;
        if !lock_team(&mut tx, admin.org_id, admin.id).await? {
            return Ok(NOT_ADMIN);
        }
        let current: Option<Row> = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM app_user WHERE id = $1 AND org_id = $2"
        ))
        .bind(id)
        .bind(admin.org_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(current) = current else {
            return Ok(Outcome::Refused(
                StatusCode::NOT_FOUND,
                "No such team member.",
            ));
        };
        // Already as asked: nothing to change or audit.
        if current.5 == change.disabled {
            return Ok(Outcome::Done(member(current)));
        }
        let row: Row = sqlx::query_as(&format!(
            "UPDATE app_user
             SET disabled_at = CASE WHEN $2 THEN now() END
             WHERE id = $1
             RETURNING {COLUMNS}"
        ))
        .bind(id)
        .bind(change.disabled)
        .fetch_one(&mut *tx)
        .await?;
        if change.disabled {
            sqlx::query("DELETE FROM user_session WHERE user_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        let action = if change.disabled {
            audit::action::USER_DISABLED
        } else {
            audit::action::USER_ENABLED
        };
        audit::record(
            &mut *tx,
            admin.org_id,
            Some(admin.id),
            action,
            &format!("user:{id}"),
        )
        .await?;
        tx.commit().await?;
        anyhow::Ok(Outcome::Done(member(row)))
    }
    .await;
    match result {
        Ok(outcome) => outcome.into_response(),
        Err(e) => server_error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn only_an_active_admin_passes_the_team_lock() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let add = |role: &'static str, off: bool| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO app_user (org_id, email, name, role, disabled_at)
                     VALUES ($1, $2, 'A', $3::user_role, CASE WHEN $4 THEN now() END) RETURNING id",
                )
                .bind(org)
                .bind(format!("{}@example.com", Uuid::new_v4()))
                .bind(role)
                .bind(off)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let (active, off, resourcer) = (
            add("admin", false).await,
            add("admin", true).await,
            add("resourcer", false).await,
        );
        for (who, expected) in [(active, true), (off, false), (resourcer, false)] {
            let mut tx = pool.begin().await.unwrap();
            assert_eq!(lock_team(&mut tx, org, who).await.unwrap(), expected);
        }
    }

    #[test]
    fn emails_are_trimmed_lowercased_and_checked() {
        assert_eq!(
            clean_email("  Jo.Bloggs@AustinWerner.io ").as_deref(),
            Some("jo.bloggs@austinwerner.io")
        );
        for bad in [
            "",
            "jo",
            "@x.io",
            "jo@",
            "jo@io",
            "jo@.io",
            "jo@x.io.",
            "jo@@x.io",
            "jo b@x.io",
        ] {
            assert_eq!(clean_email(bad), None, "{bad:?} should be refused");
        }
    }

    #[test]
    fn names_must_be_present_and_short() {
        assert_eq!(clean_name("  Jo  ").as_deref(), Some("Jo"));
        assert_eq!(clean_name("   "), None);
        assert_eq!(clean_name(&"x".repeat(201)), None);
    }

    #[test]
    fn status_follows_link_and_switch() {
        let row = |linked, disabled| {
            member((
                Uuid::nil(),
                "n".into(),
                "e".into(),
                Role::Resourcer,
                linked,
                disabled,
            ))
            .status
        };
        assert_eq!(row(false, false), MemberStatus::Invited);
        assert_eq!(row(true, false), MemberStatus::Active);
        assert_eq!(row(true, true), MemberStatus::Disabled);
        assert_eq!(row(false, true), MemberStatus::Disabled);
    }
}
