//! Outreach, part one: drafting, editing, approving and stopping the emails.
//! Nothing here sends anything.

use super::*;

/// A shortlisted candidate for "Senior IAM Engineer" at Test Client, with a
/// personal email unless `email` is None. Returns the candidacy id.
async fn shortlisted(pool: &PgPool, org: Uuid, email: Option<&str>) -> Uuid {
    let (role, brief) = testutil::role_with_brief(pool, org).await;
    let person: Uuid = sqlx::query_scalar(
        "INSERT INTO person (org_id, full_name, current_employer) VALUES ($1, 'Sam Sample', 'Otherco')
         RETURNING id",
    )
    .bind(org)
    .fetch_one(pool)
    .await
    .unwrap();
    // A work address never counts.
    sqlx::query(
        "INSERT INTO contact (org_id, person_id, kind, value, source)
         VALUES ($1, $2, 'work_email', $3, 'pdl')",
    )
    .bind(org)
    .bind(person)
    .bind(format!("sam-{}@otherco.example", Uuid::new_v4()))
    .execute(pool)
    .await
    .unwrap();
    if let Some(e) = email {
        sqlx::query(
            "INSERT INTO contact (org_id, person_id, kind, value, source)
             VALUES ($1, $2, 'personal_email', $3, 'pdl')",
        )
        .bind(org)
        .bind(person)
        .bind(e)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query_scalar(
        "INSERT INTO candidacy (org_id, person_id, role_id, brief_id, state, tier)
         VALUES ($1, $2, $3, $4, 'shortlisted', 'A') RETURNING id",
    )
    .bind(org)
    .bind(person)
    .bind(role)
    .bind(brief)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn personal() -> String {
    format!("sam-{}@mail.example", Uuid::new_v4())
}

async fn state_of(pool: &PgPool, candidacy: Uuid) -> String {
    sqlx::query_scalar("SELECT state::text FROM candidacy WHERE id = $1")
        .bind(candidacy)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn edit(v: &Value, step: usize, body: &str) -> Value {
    let s = &v["steps"][step - 1];
    json!({"version": v["version"], "steps": [{"step": step, "subject": s["subject"], "body": body}]})
}

#[tokio::test]
async fn draft_edit_approve_and_stop() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let to = personal();
    let c = shortlisted(&pool, org, Some(&to)).await;
    let uri = format!("/api/candidates/{c}/outreach");

    // Before drafting: nothing yet, and the address it would go to.
    let v = json_body(send(&app, get_req(&uri, Some(&me))).await).await;
    assert_eq!(
        (v["status"].clone(), v["to_email"].as_str()),
        (Value::Null, Some(to.as_str()))
    );

    // Draft: three emails in Kai's words, no client name, signature missing.
    let res = send(&app, json_req("POST", &uri, &me, json!({}))).await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(v["status"], "draft");
    assert_eq!(state_of(&pool, c).await, "drafted");
    let steps = v["steps"].as_array().unwrap();
    assert_eq!(
        steps
            .iter()
            .map(|s| s["delay_days"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [0, 3, 4]
    );
    let first = steps[0]["body"].as_str().unwrap();
    assert!(first.starts_with("Hey Sam,"), "{first}");
    assert!(first.contains("Senior IAM Engineer role in Dubai"));
    assert!(first.contains("- IAM"));
    assert_eq!(steps[0]["subject"], "Senior IAM Engineer role, Dubai");
    assert_eq!(steps[1]["subject"], "Re: Senior IAM Engineer role, Dubai");
    let all = v["steps"].to_string().to_lowercase();
    assert!(!all.contains("test client") && !all.contains("test-client"));
    assert!(steps[0]["html"]
        .as_str()
        .unwrap()
        .contains("professional data provider"));
    assert!(!steps[1]["html"]
        .as_str()
        .unwrap()
        .contains("professional data provider"));
    assert_eq!(
        v["problems"],
        json!(["Someone has no email signature yet. Add it in Settings first."])
    );
    assert_eq!(v["sending_ready"], false);

    // Approval is refused while there is a problem.
    let approve = format!("{uri}/approve");
    let res = send(
        &app,
        json_req("POST", &approve, &me, json!({"version": v["version"]})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // Settings: signature and introduction.
    let sig = "--\nRegards\nSam Recruiter\n[Linkedin](https://www.linkedin.com/in/example/)";
    let body =
        json!({"signature": sig, "intro": "I'm a Director at Example Ltd, a recruitment business"});
    let res = send(&app, json_req("PUT", "/api/me/outreach", &me, body)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let s = json_body(send(&app, get_req("/api/me/outreach", Some(&me))).await).await;
    assert_eq!(s["signature"], sig);

    // A fresh draft picks up the introduction; the signature shows in the preview.
    let res = send(
        &app,
        json_req("POST", &format!("{uri}?fresh=true"), &me, json!({})),
    )
    .await;
    let v = json_body(res).await;
    assert!(v["steps"][0]["body"]
        .as_str()
        .unwrap()
        .contains("I'm a Director at Example Ltd"));
    assert!(v["steps"][2]["html"]
        .as_str()
        .unwrap()
        .contains("<a href=\"https://www.linkedin.com/in/example/\">Linkedin</a>"));
    assert_eq!(v["problems"], json!([]));

    // Naming the client in an edit blocks approval.
    let res = send(
        &app,
        json_req(
            "PUT",
            &uri,
            &me,
            edit(&v, 2, "Hey Sam, it's for Test Client."),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(v["steps"][1]["body"], "Hey Sam, it's for Test Client.");
    assert!(v["problems"][0]
        .as_str()
        .unwrap()
        .starts_with("Email 2 names the client"));
    let res = send(
        &app,
        json_req("POST", &approve, &me, json!({"version": v["version"]})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // A stale edit is refused.
    let stale = json!({"version": 0, "steps": []});
    let res = send(&app, json_req("PUT", &uri, &me, stale)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // Fixed, then approved: one approval for all three.
    let res = send(
        &app,
        json_req("PUT", &uri, &me, edit(&v, 2, "Hey Sam, just bumping this.")),
    )
    .await;
    let v = json_body(res).await;
    assert_eq!(v["problems"], json!([]));
    let res = send(
        &app,
        json_req("POST", &approve, &me, json!({"version": v["version"]})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(v["status"], "approved");
    assert!(v["approved_at"].is_string());
    assert_eq!(state_of(&pool, c).await, "approved");
    // Approved emails cannot be edited or drafted again.
    let res = send(&app, json_req("PUT", &uri, &me, edit(&v, 1, "Changed"))).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let res = send(
        &app,
        json_req("POST", &format!("{uri}?fresh=true"), &me, json!({})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // Stop: back to the shortlist, with who stopped it.
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("{uri}/stop"),
            &me,
            json!({"version": v["version"]}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(
        (v["status"].as_str(), v["stop_reason"].as_str()),
        (Some("stopped"), Some("Stopped by Someone"))
    );
    assert_eq!(state_of(&pool, c).await, "shortlisted");

    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit WHERE org_id = $1 AND action LIKE 'outreach.%' ORDER BY at, action",
    )
    .bind(org)
    .fetch_all(&pool)
    .await
    .unwrap();
    for a in [
        "outreach.draft",
        "outreach.edit",
        "outreach.approve",
        "outreach.stop",
        "outreach.settings",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} audited: {actions:?}");
    }
}

#[tokio::test]
async fn no_personal_email_means_no_email() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let c = shortlisted(&pool, org, None).await;
    let uri = format!("/api/candidates/{c}/outreach");
    let v = json_body(send(&app, get_req(&uri, Some(&me))).await).await;
    assert_eq!(v["to_email"], Value::Null);
    assert_eq!(
        v["problems"],
        json!(["No personal email on record, so no email can be sent."])
    );
    let res = send(&app, json_req("POST", &uri, &me, json!({}))).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(state_of(&pool, c).await, "shortlisted");

    // A personal email on the do-not-contact list does not count either.
    let to = personal();
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let c = shortlisted(&pool, org, Some(&to)).await;
    sqlx::query(
        "INSERT INTO do_not_contact (org_id, identifier, reason) VALUES ($1, lower($2), 'opt_out')",
    )
    .bind(org)
    .bind(&to)
    .execute(&pool)
    .await
    .unwrap();
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("/api/candidates/{c}/outreach"),
            &me,
            json!({}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn only_shortlisted_people_get_a_draft() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let c = shortlisted(&pool, org, Some(&personal())).await;
    sqlx::query("UPDATE candidacy SET state = 'ranked' WHERE id = $1")
        .bind(c)
        .execute(&pool)
        .await
        .unwrap();
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("/api/candidates/{c}/outreach"),
            &me,
            json!({}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn rejecting_stops_the_emails() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let c = shortlisted(&pool, org, Some(&personal())).await;
    let uri = format!("/api/candidates/{c}/outreach");
    let res = send(&app, json_req("POST", &uri, &me, json!({}))).await;
    assert_eq!(res.status(), StatusCode::OK);
    let version: i32 = sqlx::query_scalar("SELECT version FROM candidacy WHERE id = $1")
        .bind(c)
        .fetch_one(&pool)
        .await
        .unwrap();
    let row = json!({"id": c, "version": version});
    let res = send(&app, decide_req(&me, &row, "reject", Some("FIT"))).await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(send(&app, get_req(&uri, Some(&me))).await).await;
    assert_eq!(
        (v["status"].as_str(), v["stop_reason"].as_str()),
        (Some("stopped"), Some("Rejected"))
    );
    assert_eq!(state_of(&pool, c).await, "rejected");
}

async fn set_signature(app: &Router, me: &str, sig: &str) {
    let body = json!({"signature": sig, "intro": ""});
    let res = send(app, json_req("PUT", "/api/me/outreach", me, body)).await;
    assert_eq!(res.status(), StatusCode::OK);
}

async fn draft(app: &Router, me: &str, c: Uuid) -> Value {
    let res = send(
        app,
        json_req(
            "POST",
            &format!("/api/candidates/{c}/outreach"),
            me,
            json!({}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    json_body(res).await
}

#[tokio::test]
async fn another_organisation_cannot_touch_a_draft() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let c = shortlisted(&pool, org, Some(&personal())).await;
    let v = draft(&app, &me, c).await;
    let version = v["version"].clone();

    let other = testutil::org(&pool).await;
    let (_, them) = signed_in(&pool, other, "admin").await;
    let uri = format!("/api/candidates/{c}/outreach");
    assert_eq!(
        send(&app, get_req(&uri, Some(&them))).await.status(),
        StatusCode::NOT_FOUND
    );
    for (method, path, body) in [
        ("POST", uri.clone(), json!({})),
        ("POST", format!("{uri}?fresh=true"), json!({})),
        ("PUT", uri.clone(), edit(&v, 1, "Changed by someone else")),
        (
            "POST",
            format!("{uri}/approve"),
            json!({"version": version}),
        ),
        ("POST", format!("{uri}/stop"), json!({"version": version})),
    ] {
        let res = send(&app, json_req(method, &path, &them, body)).await;
        assert_ne!(res.status(), StatusCode::OK, "{method} {path}");
    }
    let after = json_body(send(&app, get_req(&uri, Some(&me))).await).await;
    assert_eq!(after["status"], "draft");
    assert_eq!(after["version"], version);
    assert_eq!(after["steps"], v["steps"]);
    assert_eq!(state_of(&pool, c).await, "drafted");
}

#[tokio::test]
async fn the_signature_is_checked_and_kept_as_approved() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let c = shortlisted(&pool, org, Some(&personal())).await;
    let uri = format!("/api/candidates/{c}/outreach");

    // A signature naming the client blocks approval.
    set_signature(
        &app,
        &me,
        "Regards\nSam\nRecruiting for test-client.example",
    )
    .await;
    let v = draft(&app, &me, c).await;
    assert!(
        v["problems"].as_array().unwrap().iter().any(|p| p
            .as_str()
            .unwrap()
            .starts_with("The signature names the client")),
        "{}",
        v["problems"]
    );
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("{uri}/approve"),
            &me,
            json!({"version": v["version"]}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // Approved with a clean signature; a later edit does not change it.
    set_signature(&app, &me, "Regards\nSam Recruiter").await;
    let v = json_body(send(&app, get_req(&uri, Some(&me))).await).await;
    assert_eq!(v["problems"], json!([]));
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("{uri}/approve"),
            &me,
            json!({"version": v["version"]}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    set_signature(&app, &me, "Regards\nSomething new").await;
    let v = json_body(send(&app, get_req(&uri, Some(&me))).await).await;
    let html = v["steps"][0]["html"].as_str().unwrap();
    assert!(html.contains("Sam Recruiter") && !html.contains("Something new"));

    // Rejecting clears the approval and records the stop.
    let version: i32 = sqlx::query_scalar("SELECT version FROM candidacy WHERE id = $1")
        .bind(c)
        .fetch_one(&pool)
        .await
        .unwrap();
    let row = json!({"id": c, "version": version});
    let res = send(&app, decide_req(&me, &row, "reject", Some("FIT"))).await;
    assert_eq!(res.status(), StatusCode::OK);
    let approved_by: Option<Uuid> =
        sqlx::query_scalar("SELECT sequence_approved_by FROM candidacy WHERE id = $1")
            .bind(c)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(approved_by, None);
    let stops: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'outreach.stop'
           AND target LIKE '%reason:rejected'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stops, 1);
}

#[tokio::test]
async fn only_stopping_moves_someone_back_to_the_shortlist() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let c = shortlisted(&pool, org, Some(&personal())).await;
    draft(&app, &me, c).await;
    let version: i32 = sqlx::query_scalar("SELECT version FROM candidacy WHERE id = $1")
        .bind(c)
        .fetch_one(&pool)
        .await
        .unwrap();
    let row = json!({"id": c, "version": version});
    let res = send(&app, decide_req(&me, &row, "shortlist", None)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(state_of(&pool, c).await, "drafted");
}

#[tokio::test]
async fn a_fresh_draft_never_replaces_newer_work() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let c = shortlisted(&pool, org, Some(&personal())).await;
    let uri = format!("/api/candidates/{c}/outreach");
    let v = draft(&app, &me, c).await;
    let res = send(
        &app,
        json_req("PUT", &uri, &me, edit(&v, 2, "My own words.")),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    // A window still showing the first version cannot start again over the edit.
    let stale = format!("{uri}?fresh=true&version={}", v["version"]);
    let res = send(&app, json_req("POST", &stale, &me, json!({}))).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let now = json_body(send(&app, get_req(&uri, Some(&me))).await).await;
    assert_eq!(now["steps"][1]["body"], "My own words.");
}

#[tokio::test]
async fn do_not_contact_after_drafting_blocks_approval() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    set_signature(&app, &me, "Regards\nSam").await;
    let to = personal();
    let c = shortlisted(&pool, org, Some(&to)).await;
    let v = draft(&app, &me, c).await;
    assert_eq!(v["problems"], json!([]));
    sqlx::query(
        "INSERT INTO do_not_contact (org_id, identifier, reason) VALUES ($1, lower($2), 'opt_out')",
    )
    .bind(org)
    .bind(&to)
    .execute(&pool)
    .await
    .unwrap();
    let uri = format!("/api/candidates/{c}/outreach");
    let v = json_body(send(&app, get_req(&uri, Some(&me))).await).await;
    assert_eq!(v["problems"], json!(["On the do-not-contact list."]));
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("{uri}/approve"),
            &me,
            json!({"version": v["version"]}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(state_of(&pool, c).await, "drafted");
}

#[tokio::test]
async fn a_closed_role_sends_nothing_and_drops_off_the_open_list() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    set_signature(&app, &me, "Regards\nSam").await;
    let c = shortlisted(&pool, org, Some(&personal())).await;
    let v = draft(&app, &me, c).await;
    assert_eq!(v["problems"], json!([]));
    let uri = format!("/api/candidates/{c}/outreach");
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("{uri}/approve"),
            &me,
            json!({"version": v["version"]}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let role: Uuid = sqlx::query_scalar("SELECT role_id FROM candidacy WHERE id = $1")
        .bind(c)
        .fetch_one(&pool)
        .await
        .unwrap();
    let close = |closed: bool| {
        json_req(
            "POST",
            &format!("/api/roles/{role}/close"),
            &me,
            json!({"closed": closed}),
        )
    };

    // The list says what closing stops.
    let roles = json_body(send(&app, get_req("/api/roles", Some(&me))).await).await;
    let mine = |rs: &Value| {
        rs.as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == json!(role))
            .cloned()
            .unwrap()
    };
    assert_eq!(
        (
            mine(&roles)["closed"].as_bool(),
            mine(&roles)["active_sequences"].as_i64()
        ),
        (Some(false), Some(1))
    );

    let going = |t: &Value| {
        t["going_out"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["candidacy_id"] == json!(c))
    };
    let today = json_body(send(&app, get_req("/api/today", Some(&me))).await).await;
    assert!(going(&today), "{today}");

    let roles = json_body(send(&app, close(true)).await).await;
    assert_eq!(mine(&roles)["closed"], true);
    let today = json_body(send(&app, get_req("/api/today", Some(&me))).await).await;
    assert!(
        !going(&today),
        "a closed role's emails are not shown as going out"
    );
    // Before any send, every check runs again and now stops it.
    let problems = crate::outreach::problems_before_send(
        &pool,
        org,
        c,
        sqlx::query_scalar("SELECT sender_id FROM outreach WHERE candidacy_id = $1")
            .bind(c)
            .fetch_one(&pool)
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert!(
        problems.contains(&"This role is closed.".to_string()),
        "{problems:?}"
    );
    let s = json_body(
        send(
            &app,
            get_req(&format!("/api/roles/{role}/search"), Some(&me)),
        )
        .await,
    )
    .await;
    assert!(s["blocked"].as_str().unwrap().contains("closed"));
    let detail =
        json_body(send(&app, get_req(&format!("/api/roles/{role}"), Some(&me))).await).await;
    assert_eq!(detail["closed"], true);
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'role.close'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audits, 1);
    // Closing twice changes nothing.
    send(&app, close(true)).await;

    let roles = json_body(send(&app, close(false)).await).await;
    assert_eq!(mine(&roles)["closed"], false);

    // Another organisation cannot close it.
    let other = testutil::org(&pool).await;
    let (_, outsider) = signed_in(&pool, other, "admin").await;
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("/api/roles/{role}/close"),
            &outsider,
            json!({"closed": true}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let closed: bool = sqlx::query_scalar("SELECT closed_at IS NOT NULL FROM role WHERE id = $1")
        .bind(role)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!closed);
}
