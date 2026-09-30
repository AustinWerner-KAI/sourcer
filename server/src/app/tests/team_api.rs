//! Team management by admins.

use super::*;

#[tokio::test]
async fn only_admins_manage_the_team() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, resourcer) = signed_in(&pool, org, "resourcer").await;
    let app = app_for(pool.clone(), "http://127.0.0.1:9", "nobody@example.com");

    let res = send(&app, get_req("/api/team", None)).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = send(&app, get_req("/api/team", Some(&resourcer))).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let invite = json!({"name": "X", "email": "x@example.com", "role": "admin"});
    let res = send(&app, json_req("POST", "/api/team", &resourcer, invite)).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "no self-promotion");
    let uri = format!("/api/team/{}", Uuid::new_v4());
    let res = send(
        &app,
        json_req("PATCH", &uri, &resourcer, json!({"disabled": true})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let made: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app_user WHERE email = 'x@example.com'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(made, 0);
}

#[tokio::test]
async fn admin_invites_then_the_invitee_can_sign_in() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, admin) = signed_in(&pool, org, "admin").await;
    let email = unique_email("new-starter");
    let oid = Uuid::new_v4().to_string();
    let ms = fake_microsoft(email.clone(), oid).await;
    let app = app_for(pool.clone(), &ms, "nobody@example.com");

    let body =
        json!({"name": "  New Starter ", "email": email.to_uppercase(), "role": "resourcer"});
    let res = send(&app, json_req("POST", "/api/team", &admin, body.clone())).await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let m = json_body(res).await;
    assert_eq!(
        (
            m["name"].as_str(),
            m["email"].as_str(),
            m["status"].as_str()
        ),
        (Some("New Starter"), Some(email.as_str()), Some("invited"))
    );
    let again = send(&app, json_req("POST", "/api/team", &admin, body)).await;
    assert_eq!(again.status(), StatusCode::CONFLICT);
    let bad = json!({"name": "X", "email": "not-an-email", "role": "resourcer"});
    let res = send(&app, json_req("POST", "/api/team", &admin, bad)).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let bad_role = json!({"name": "X", "email": "y@example.com", "role": "owner"});
    let res = send(&app, json_req("POST", "/api/team", &admin, bad_role)).await;
    assert!(res.status().is_client_error());

    assert_eq!(location(&sign_in(&app).await), "/");
    let team = json_body(send(&app, get_req("/api/team", Some(&admin))).await).await;
    let starter = team
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["email"] == email.as_str())
        .unwrap();
    assert_eq!(starter["status"], "active");
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'team.invite'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);
}

#[tokio::test]
async fn admin_switches_someone_off_and_back_on() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (admin_id, admin) = signed_in(&pool, org, "admin").await;
    let (staff_id, staff) = signed_in(&pool, org, "resourcer").await;
    let app = app_for(pool.clone(), "http://127.0.0.1:9", "nobody@example.com");
    let uri = format!("/api/team/{staff_id}");

    let res = send(
        &app,
        json_req("PATCH", &uri, &admin, json!({"disabled": true})),
    )
    .await;
    assert_eq!(json_body(res).await["status"], "disabled");
    let res = send(&app, get_req("/api/me", Some(&staff))).await;
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "sessions end at once"
    );
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM user_session WHERE user_id = $1")
        .bind(staff_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);

    let res = send(
        &app,
        json_req("PATCH", &uri, &admin, json!({"disabled": false})),
    )
    .await;
    assert_eq!(json_body(res).await["status"], "active");

    let me = format!("/api/team/{admin_id}");
    let res = send(
        &app,
        json_req("PATCH", &me, &admin, json!({"disabled": true})),
    )
    .await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "cannot switch yourself off"
    );

    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit WHERE org_id = $1 AND action LIKE 'team.%' ORDER BY id",
    )
    .bind(org)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(actions, ["team.disable", "team.enable"]);
}

#[tokio::test]
async fn admins_only_see_and_change_their_own_organisation() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let (org_a, org_b) = (testutil::org(&pool).await, testutil::org(&pool).await);
    let (_, admin_a) = signed_in(&pool, org_a, "admin").await;
    let (outsider, _) = signed_in(&pool, org_b, "resourcer").await;
    let app = app_for(pool.clone(), "http://127.0.0.1:9", "nobody@example.com");

    let team = json_body(send(&app, get_req("/api/team", Some(&admin_a))).await).await;
    assert_eq!(team.as_array().unwrap().len(), 1, "only org A");
    let uri = format!("/api/team/{outsider}");
    let res = send(
        &app,
        json_req("PATCH", &uri, &admin_a, json!({"disabled": true})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let disabled: bool =
        sqlx::query_scalar("SELECT disabled_at IS NOT NULL FROM app_user WHERE id = $1")
            .bind(outsider)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!disabled);
}

#[tokio::test]
async fn invited_admin_signs_in_as_admin() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, admin) = signed_in(&pool, org, "admin").await;
    let email = unique_email("second-admin");
    let ms = fake_microsoft(email.clone(), Uuid::new_v4().to_string()).await;
    let app = app_for(pool.clone(), &ms, "nobody@example.com");
    let body = json!({"name": "Second", "email": email, "role": "admin"});
    let res = send(&app, json_req("POST", "/api/team", &admin, body)).await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let target: String =
        sqlx::query_scalar("SELECT target FROM audit WHERE org_id = $1 AND action = 'team.invite'")
            .bind(org)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        target.ends_with("role:admin"),
        "audit says who made an admin"
    );

    let cookie = session_of(&sign_in(&app).await);
    let me = json_body(send(&app, get_req("/api/me", Some(&cookie))).await).await;
    assert_eq!(me["role"], "admin");
}

#[tokio::test]
async fn switched_back_on_user_can_sign_in_again() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, admin) = signed_in(&pool, org, "admin").await;
    let email = unique_email("returner");
    let ms = fake_microsoft(email.clone(), Uuid::new_v4().to_string()).await;
    let app = app_for(pool.clone(), &ms, "nobody@example.com");
    let body = json!({"name": "Returner", "email": email, "role": "resourcer"});
    let m = json_body(send(&app, json_req("POST", "/api/team", &admin, body.clone())).await).await;
    let uri = format!("/api/team/{}", m["id"].as_str().unwrap());

    send(
        &app,
        json_req("PATCH", &uri, &admin, json!({"disabled": true})),
    )
    .await;
    assert_eq!(location(&sign_in(&app).await), "/?signin=disabled");
    let refused: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'auth.sign_in_refused'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(refused, 1, "refusals of switched-off users are audited");
    let res = send(&app, json_req("POST", "/api/team", &admin, body)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let msg =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(msg.contains("switched off"));

    send(
        &app,
        json_req("PATCH", &uri, &admin, json!({"disabled": false})),
    )
    .await;
    assert_eq!(location(&sign_in(&app).await), "/");
}

#[tokio::test]
async fn invite_catches_an_older_mixed_case_address() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, admin) = signed_in(&pool, org, "admin").await;
    let email = unique_email("Mixed.Case");
    sqlx::query("INSERT INTO app_user (org_id, email, name) VALUES ($1, $2, 'Old')")
        .bind(org)
        .bind(&email)
        .execute(&pool)
        .await
        .unwrap();
    let app = app_for(pool.clone(), "http://127.0.0.1:9", "nobody@example.com");
    let body = json!({"name": "Dup", "email": email.to_lowercase(), "role": "resourcer"});
    let res = send(&app, json_req("POST", "/api/team", &admin, body)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn repeating_a_switch_changes_and_audits_nothing() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, admin) = signed_in(&pool, org, "admin").await;
    let (staff, _) = signed_in(&pool, org, "resourcer").await;
    let app = app_for(pool.clone(), "http://127.0.0.1:9", "nobody@example.com");
    let uri = format!("/api/team/{staff}");
    for _ in 0..2 {
        let res = send(
            &app,
            json_req("PATCH", &uri, &admin, json!({"disabled": true})),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
    }
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'team.disable'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 1);
}
