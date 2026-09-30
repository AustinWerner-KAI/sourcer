//! Clients, roles and briefs.

use super::*;

#[tokio::test]
async fn clients_need_a_real_domain_and_it_is_unique() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let bad = json!({"name": "Acme", "domain": "not a domain", "off_limits": false});
    let res = send(&app, json_req("POST", "/api/clients", &me, bad)).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let domain = format!("{}.example.com", Uuid::new_v4());
    let ok = json!({"name": "Acme", "domain": format!("https://www.{domain}/jobs"), "off_limits": false});
    let res = send(&app, json_req("POST", "/api/clients", &me, ok.clone())).await;
    assert_eq!(
        json_body(res).await["domain"],
        domain.as_str(),
        "domain is cleaned"
    );
    let res = send(&app, json_req("POST", "/api/clients", &me, ok)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn brief_is_confirmed_only_when_every_tool_is_answered_then_locks() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let hiring = new_client(&app, &me, "Client A", false).await;
    new_client(&app, &me, "Blocked Co", true).await;
    new_client(&app, &me, "Unrelated", false).await;

    let body =
        json!({"client_id": hiring["id"], "title": "Senior IAM Engineer", "spec_text": "spec"});
    let res = send(&app, json_req("POST", "/api/roles", &me, body)).await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let role = json_body(res).await;
    assert_eq!(
        role["locked_out"],
        json!([
            {"id": role["locked_out"][0]["id"], "name": "Blocked Co", "hiring": false},
            {"id": hiring["id"], "name": "Client A", "hiring": true}
        ]),
        "hiring client and off-limits clients, nothing else"
    );
    let uri = format!("/api/roles/{}", role["id"].as_str().unwrap());

    let confirm = format!("{uri}/brief/confirm");
    let res = send(
        &app,
        json_req("PUT", &format!("{uri}/brief"), &me, lines(None)),
    )
    .await;
    let saved = json_body(res).await;
    assert_eq!(saved["brief"]["confirmed"], false);
    assert_eq!(
        (
            &saved["brief"]["lines"]["capabilities"],
            &saved["brief"]["lines"]["domains"]
        ),
        (
            &json!(["Stakeholder management"]),
            &json!([{"name": "Privileged access", "weight": "must"}])
        ),
        "capabilities and domains are stored"
    );
    let mut no_domain = lines(Some("required"));
    no_domain["domains"] = json!([]);
    let res = send(
        &app,
        json_req(
            "POST",
            &confirm,
            &me,
            json!({"lines": no_domain, "based_on": 1}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "a domain is needed");
    let res = send(
        &app,
        json_req("POST", &confirm, &me, confirming(None, Some(1))),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let msg =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(msg.contains("Answer 1 named tool"), "{msg}");

    // Confirm saves and locks exactly the lines sent, in one step.
    let res = send(
        &app,
        json_req(
            "POST",
            &confirm,
            &me,
            confirming(Some("replacing"), Some(1)),
        ),
    )
    .await;
    let d = json_body(res).await;
    assert_eq!(
        (
            d["brief"]["confirmed"].as_bool(),
            d["brief"]["version"].as_i64()
        ),
        (Some(true), Some(1))
    );

    // Editing after confirming starts version 2; version 1 never changes.
    let mut edited = lines(Some("required"));
    edited["locations"] = json!(["New York"]);
    let res = send(&app, json_req("PUT", &format!("{uri}/brief"), &me, edited)).await;
    let d = json_body(res).await;
    assert_eq!(
        (
            d["brief"]["confirmed"].as_bool(),
            d["brief"]["version"].as_i64()
        ),
        (Some(false), Some(2))
    );
    let v1: Value =
        sqlx::query_scalar("SELECT locations FROM brief WHERE role_id = $1 AND version = 1")
            .bind(Uuid::parse_str(role["id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(v1, json!(["Dubai"]));
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE org_id = $1 AND action IN ('role.create', 'brief.confirm', 'client.create')",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audited, 5);
}

#[tokio::test]
async fn roles_and_clients_of_another_organisation_are_invisible() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let (org_a, org_b) = (testutil::org(&pool).await, testutil::org(&pool).await);
    let (_, a) = signed_in(&pool, org_a, "resourcer").await;
    let (_, b) = signed_in(&pool, org_b, "admin").await;
    let app = plain_app(pool.clone());
    let client_a = new_client(&app, &a, "A's client", false).await;
    let body = json!({"client_id": client_a["id"], "title": "Role", "spec_text": ""});
    let role = json_body(send(&app, json_req("POST", "/api/roles", &a, body.clone())).await).await;
    let uri = format!("/api/roles/{}", role["id"].as_str().unwrap());

    // B can neither see nor use A's client or role.
    let res = send(&app, json_req("POST", "/api/roles", &b, body)).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        send(&app, get_req(&uri, Some(&b))).await.status(),
        StatusCode::NOT_FOUND
    );
    let res = send(
        &app,
        json_req("PUT", &format!("{uri}/brief"), &b, lines(None)),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("{uri}/brief/confirm"),
            &b,
            confirming(Some("nice"), None),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let lists = json_body(send(&app, get_req("/api/roles", Some(&b))).await).await;
    assert_eq!(lists, json!([]));
    let clients = json_body(send(&app, get_req("/api/clients", Some(&b))).await).await;
    assert_eq!(clients, json!([]));
    assert_eq!(
        send(&app, get_req("/api/roles", None)).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_role_without_a_client_cannot_be_confirmed_until_one_is_set() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let role: Uuid =
        sqlx::query_scalar("INSERT INTO role (org_id, title) VALUES ($1, 'Old role') RETURNING id")
            .bind(org)
            .fetch_one(&pool)
            .await
            .unwrap();
    let uri = format!("/api/roles/{role}");
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("{uri}/brief/confirm"),
            &me,
            confirming(Some("nice"), None),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let client = new_client(&app, &me, "Late Client", false).await;
    let body = json!({"title": "Old role", "spec_text": "", "client_id": client["id"]});
    let d = json_body(send(&app, json_req("PATCH", &uri, &me, body)).await).await;
    assert_eq!(d["locked_out"][0]["hiring"], true);
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("{uri}/brief/confirm"),
            &me,
            confirming(Some("nice"), None),
        ),
    )
    .await;
    assert_eq!(json_body(res).await["brief"]["confirmed"], true);

    // A window still showing "no brief" cannot confirm over version 1.
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("{uri}/brief/confirm"),
            &me,
            confirming(Some("required"), None),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    // And the client, once set, stays.
    let other = new_client(&app, &me, "Other Client", false).await;
    let body = json!({"title": "Old role", "spec_text": "", "client_id": other["id"]});
    let res = send(&app, json_req("PATCH", &uri, &me, body)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn claude_drafts_the_brief_from_the_spec() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let fake = Router::new().route(
        "/v1/messages",
        post(|| async {
            Json(json!({"content": [{"type": "tool_use", "id": "t", "name": "record_brief", "input": {
                "levels": ["Senior"], "must_haves": ["IAM"], "tools": ["Okta"],
                "locations": ["Dubai"], "remote": false
            }}]}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });

    let without = plain_app(pool.clone());
    let mut state = AppState::new(Some(pool.clone()), None);
    state.ai = Arc::new(Claude::with_base_url(Some("k".into()), &url));
    let app = router(state);
    let client = new_client(&app, &me, "Client B", false).await;
    let body = json!({"client_id": client["id"], "title": "IAM", "spec_text": "Senior IAM Engineer in Dubai"});
    let role = json_body(send(&app, json_req("POST", "/api/roles", &me, body)).await).await;
    let draft = format!("/api/roles/{}/brief/draft", role["id"].as_str().unwrap());

    let res = send(&without, json_req("POST", &draft, &me, json!({}))).await;
    assert_eq!(
        res.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "no key, no call"
    );

    let d = json_body(send(&app, json_req("POST", &draft, &me, json!({}))).await).await;
    assert_eq!(d["brief"]["drafted_by_ai"], true);
    assert_eq!(
        d["brief"]["lines"]["tools"],
        json!([{"name": "Okta", "status": null}])
    );
    assert!(
        d["brief"]["lines"]["excluded_titles"]
            .as_array()
            .unwrap()
            .len()
            >= 5
    );
}
