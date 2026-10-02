//! Admin: the kill switches, editing clients and the do-not-contact list.

use super::*;

#[tokio::test]
async fn admins_pause_sending_and_paid_calls() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, admin) = signed_in(&pool, org, "admin").await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());

    let res = send(&app, get_req("/api/admin/controls", Some(&me))).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "admins only");
    let v = json_body(send(&app, get_req("/api/admin/controls", Some(&admin))).await).await;
    assert_eq!(
        v,
        json!({"sending_paused": false, "paid_calls_paused": false, "first_emails_per_day": 25})
    );

    let on = json!({"sending_paused": true, "paid_calls_paused": true, "first_emails_per_day": 10});
    let res = send(
        &app,
        json_req("PUT", "/api/admin/controls", &me, on.clone()),
    )
    .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let too_many =
        json!({"sending_paused": true, "paid_calls_paused": true, "first_emails_per_day": 500});
    let res = send(
        &app,
        json_req("PUT", "/api/admin/controls", &admin, too_many),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = send(&app, json_req("PUT", "/api/admin/controls", &admin, on)).await;
    assert_eq!(res.status(), StatusCode::OK);
    // A change to one switch leaves the others as they are.
    let res = send(
        &app,
        json_req(
            "PUT",
            "/api/admin/controls",
            &admin,
            json!({"paid_calls_paused": false}),
        ),
    )
    .await;
    assert_eq!(
        json_body(res).await,
        json!({"sending_paused": true, "paid_calls_paused": false, "first_emails_per_day": 10})
    );
    let (s, p, n): (bool, bool, i32) = sqlx::query_as(
        "SELECT sending_paused, paid_calls_paused, first_emails_per_day FROM org WHERE id = $1",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(s && !p && n == 10);
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'org.controls'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audited, 2);
}

#[tokio::test]
async fn admins_edit_clients_and_the_off_limits_flag() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, admin) = signed_in(&pool, org, "admin").await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let a = new_client(&app, &me, "Client A", false).await;
    let b = new_client(&app, &me, "Client B", false).await;
    let uri = format!("/api/clients/{}", a["id"].as_str().unwrap());

    let change = json!({"name": "Client A Ltd", "domain": "https://www.client-a.example/", "off_limits": true});
    let res = send(&app, json_req("PATCH", &uri, &me, change.clone())).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "admins only");
    let res = send(&app, json_req("PATCH", &uri, &admin, change)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let c = json_body(res).await;
    assert_eq!(
        (
            c["name"].as_str(),
            c["domain"].as_str(),
            c["off_limits"].as_bool()
        ),
        (Some("Client A Ltd"), Some("client-a.example"), Some(true))
    );

    // Another client's domain, or no domain, is refused.
    let clash = json!({"name": "Client A", "domain": b["domain"], "off_limits": false});
    let res = send(&app, json_req("PATCH", &uri, &admin, clash)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let blank = json!({"name": "Client A", "domain": "", "off_limits": false});
    let res = send(&app, json_req("PATCH", &uri, &admin, blank)).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Another organisation cannot touch it.
    let (_, outsider) = signed_in(&pool, testutil::org(&pool).await, "admin").await;
    let res = send(
        &app,
        json_req(
            "PATCH",
            &uri,
            &outsider,
            json!({"name": "X", "domain": "x.example", "off_limits": false}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn do_not_contact_is_added_for_good_and_stops_emails() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (admin_id, admin) = signed_in(&pool, org, "admin").await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());

    // Someone with approved emails waiting to go.
    let (role, brief) = testutil::role_with_brief(&pool, org).await;
    let email = format!("sam-{}@mail.example", Uuid::new_v4());
    let candidacy: Uuid = sqlx::query_scalar(
        "WITH p AS (INSERT INTO person (org_id, full_name) VALUES ($1, 'Sam Sample') RETURNING id),
              k AS (INSERT INTO contact (org_id, person_id, kind, value, source)
                    SELECT $1, id, 'personal_email', $4, 'pdl' FROM p)
         INSERT INTO candidacy (org_id, person_id, role_id, brief_id, state)
         SELECT $1, id, $2, $3, 'approved' FROM p RETURNING id",
    )
    .bind(org)
    .bind(role)
    .bind(brief)
    .bind(&email)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO outreach (org_id, candidacy_id, sender_id, to_email, status)
         VALUES ($1, $2, $3, $4, 'approved')",
    )
    .bind(org)
    .bind(candidacy)
    .bind(admin_id)
    .bind(&email)
    .execute(&pool)
    .await
    .unwrap();

    let add = |who: &str, id: &str| {
        json_req(
            "POST",
            "/api/admin/do-not-contact",
            who,
            json!({"identifier": id, "reason": "opt_out"}),
        )
    };
    let res = send(&app, add(&me, &email)).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "admins only");
    let res = send(&app, add(&admin, &email.to_uppercase())).await;
    assert_eq!(res.status(), StatusCode::CREATED);
    assert_eq!(json_body(res).await["identifier"], email.as_str());
    // Twice is fine; nothing is doubled.
    let res = send(&app, add(&admin, &email)).await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let res = send(&app, add(&admin, "not an address")).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = send(
        &app,
        add(&admin, "https://uk.linkedin.com/in/Someone-Else/"),
    )
    .await;
    assert_eq!(
        json_body(res).await["identifier"],
        "linkedin.com/in/someone-else"
    );

    // The emails stop and the person is back on the shortlist, flagged.
    let (status, reason): (String, Option<String>) =
        sqlx::query_as("SELECT status::text, stop_reason FROM outreach WHERE candidacy_id = $1")
            .bind(candidacy)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (status.as_str(), reason.as_deref()),
        ("stopped", Some("Do not contact"))
    );
    let state: String = sqlx::query_scalar("SELECT state::text FROM candidacy WHERE id = $1")
        .bind(candidacy)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "shortlisted");

    // The list, newest first, searchable.
    let v = json_body(send(&app, get_req("/api/admin/do-not-contact", Some(&admin))).await).await;
    assert_eq!(v["total"], 2);
    let v = json_body(
        send(
            &app,
            get_req("/api/admin/do-not-contact?q=MAIL.example", Some(&admin)),
        )
        .await,
    )
    .await;
    assert_eq!(v["total"], 1);
    assert_eq!(v["entries"][0]["reason"], "opt_out");
    // A search for "_" matches only a real underscore.
    let v =
        json_body(send(&app, get_req("/api/admin/do-not-contact?q=_", Some(&admin))).await).await;
    assert_eq!(v["total"], 0);

    // Another organisation sees none of it, and the audit keeps no address.
    let (_, outsider) = signed_in(&pool, testutil::org(&pool).await, "admin").await;
    let v =
        json_body(send(&app, get_req("/api/admin/do-not-contact", Some(&outsider))).await).await;
    assert_eq!(v["total"], 0);
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'dnc.add' AND target LIKE '%@%'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(leaked, 0);
}
