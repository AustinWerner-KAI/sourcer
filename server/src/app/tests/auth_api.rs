//! Sign-in, sessions and the change header.

use super::*;

#[tokio::test]
async fn health_responds_without_db() {
    let app = router(AppState::new(None, None));
    let res = send(&app, get_req("/api/health", None)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(v["status"], "ok");
    assert_eq!(v["database"], false);
}

#[tokio::test]
async fn me_needs_a_session() {
    let app = router(AppState::new(None, None));
    let res = send(&app, get_req("/api/me", None)).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_without_config_says_so() {
    let app = router(AppState::new(None, None));
    let res = send(&app, get_req("/api/auth/login", None)).await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn first_admin_signs_in_sees_me_and_signs_out() {
    let Some(pool) = testutil::fresh_pool().await else {
        return;
    };
    let email = unique_email("admin");
    let ms = fake_microsoft(email.clone(), Uuid::new_v4().to_string()).await;
    let app = app_for(pool.clone(), &ms, &email);

    let (state, browser) = begin(&app).await;
    let cb = format!("/api/auth/callback?code=good-code&state={state}");
    let res = send(&app, get_req(&cb, Some(&browser))).await;
    assert_eq!(location(&res), "/");
    let set = set_cookie(&res, "sourcer_session").unwrap();
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax"));
    let cleared = set_cookie(&res, "sourcer_oauth").unwrap();
    assert!(
        cleared.contains("Max-Age=0"),
        "sign-in cookie is single use"
    );
    let cookie = session_of(&res);

    let me = json_body(send(&app, get_req("/api/me", Some(&cookie))).await).await;
    assert_eq!(
        (me["email"].as_str(), me["role"].as_str()),
        (Some(email.as_str()), Some("admin"))
    );

    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit WHERE action = 'auth.sign_in'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audited, 1);

    // The same state cannot be used twice.
    let again = send(&app, get_req(&cb, Some(&browser))).await;
    assert_eq!(location(&again), "/?signin=expired");

    let out = Request::builder()
        .method("POST")
        .uri("/api/auth/logout")
        .header(header::COOKIE, &cookie)
        .header(CHANGE_HEADER, "1")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, out).await.status(), StatusCode::NO_CONTENT);
    let res = send(&app, get_req("/api/me", Some(&cookie))).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_email_does_not_create_a_second_admin() {
    let Some(pool) = testutil::fresh_pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    sqlx::query("INSERT INTO app_user (org_id, email, name, role) VALUES ($1, 'boss@example.com', 'Boss', 'admin')")
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();
    // The address in ADMIN_EMAIL now belongs to someone new.
    let ms = fake_microsoft("reused@example.com".into(), Uuid::new_v4().to_string()).await;
    let app = app_for(pool.clone(), &ms, "reused@example.com");

    let res = sign_in(&app).await;
    assert_eq!(location(&res), "/?signin=not-invited");
    let admins: i64 = sqlx::query_scalar("SELECT count(*) FROM app_user WHERE role = 'admin'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(admins, 1);
}

#[tokio::test]
async fn invited_user_is_linked_on_first_sign_in() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let email = unique_email("invited");
    let oid = Uuid::new_v4().to_string();
    let user = add_user(&pool, &email, None).await;
    let ms = fake_microsoft(email.to_uppercase(), oid.clone()).await;
    let app = app_for(pool.clone(), &ms, "nobody@example.com");

    let res = sign_in(&app).await;
    assert_eq!(location(&res), "/");
    let me = json_body(send(&app, get_req("/api/me", Some(&session_of(&res)))).await).await;
    assert_eq!(me["role"], "resourcer");
    let linked: Option<String> = sqlx::query_scalar("SELECT ms_oid FROM app_user WHERE id = $1")
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(linked.as_deref(), Some(oid.as_str()));
}

#[tokio::test]
async fn reused_email_cannot_take_over_an_account() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let email = unique_email("leaver");
    let old_account = format!("old-{}", Uuid::new_v4());
    let user = add_user(&pool, &email, Some(&old_account)).await;
    // A different Microsoft account now holds the same address.
    let ms = fake_microsoft(email.clone(), Uuid::new_v4().to_string()).await;
    let app = app_for(pool.clone(), &ms, &email);

    let res = sign_in(&app).await;
    assert_eq!(location(&res), "/?signin=mismatch");
    assert!(set_cookie(&res, "sourcer_session").is_none());
    let (linked, refused): (Option<String>, i64) = sqlx::query_as(
        "SELECT u.ms_oid, (SELECT count(*) FROM audit a WHERE a.target = 'user:' || u.id
                           AND a.action = 'auth.sign_in_refused')
         FROM app_user u WHERE u.id = $1",
    )
    .bind(user)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        linked.as_deref(),
        Some(old_account.as_str()),
        "link unchanged"
    );
    assert_eq!(refused, 1);
}

#[tokio::test]
async fn disabled_user_cannot_sign_in_and_loses_open_sessions() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let email = unique_email("staff");
    let user = add_user(&pool, &email, None).await;
    let ms = fake_microsoft(email, Uuid::new_v4().to_string()).await;
    let app = app_for(pool.clone(), &ms, "nobody@example.com");
    let cookie = session_of(&sign_in(&app).await);
    assert_eq!(
        send(&app, get_req("/api/me", Some(&cookie))).await.status(),
        StatusCode::OK
    );

    sqlx::query("UPDATE app_user SET disabled_at = now() WHERE id = $1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    let res = send(&app, get_req("/api/me", Some(&cookie))).await;
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "open session ends at once"
    );
    assert_eq!(location(&sign_in(&app).await), "/?signin=disabled");
}

#[tokio::test]
async fn idle_or_over_age_sessions_end() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let email = unique_email("idle");
    add_user(&pool, &email, None).await;
    let ms = fake_microsoft(email, Uuid::new_v4().to_string()).await;
    let app = app_for(pool.clone(), &ms, "nobody@example.com");

    let idle = session_of(&sign_in(&app).await);
    let old = session_of(&sign_in(&app).await);
    let hash = |c: &str| {
        use base64::Engine;
        use sha2::Digest;
        let token = c.split_once('=').unwrap().1;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(token.as_bytes()))
    };
    sqlx::query(
        "UPDATE user_session SET expires_at = now() - interval '1 minute' WHERE token_hash = $1",
    )
    .bind(hash(&idle))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE user_session SET absolute_expires_at = now() - interval '1 minute' WHERE token_hash = $1")
        .bind(hash(&old))
        .execute(&pool)
        .await
        .unwrap();
    for c in [idle, old] {
        assert_eq!(
            send(&app, get_req("/api/me", Some(&c))).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn sign_in_link_from_another_browser_is_refused() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let email = unique_email("victim");
    add_user(&pool, &email, None).await;
    let ms = fake_microsoft(email, Uuid::new_v4().to_string()).await;
    let app = app_for(pool.clone(), &ms, "nobody@example.com");

    // The attacker starts a sign-in and sends the callback link to a colleague.
    let (state, _attackers_cookie) = begin(&app).await;
    let cb = format!("/api/auth/callback?code=good-code&state={state}");
    let res = send(&app, get_req(&cb, None)).await;
    assert_eq!(location(&res), "/?signin=browser");
    assert!(set_cookie(&res, "sourcer_session").is_none());
    let res = send(&app, get_req(&cb, Some("sourcer_oauth=someone-elses"))).await;
    assert_eq!(location(&res), "/?signin=browser");
}

#[tokio::test]
async fn someone_not_invited_is_turned_away() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let stranger = unique_email("stranger");
    let ms = fake_microsoft(stranger.clone(), Uuid::new_v4().to_string()).await;
    let app = app_for(pool.clone(), &ms, "someone-else@example.com");

    let res = sign_in(&app).await;
    assert_eq!(location(&res), "/?signin=not-invited");
    assert!(set_cookie(&res, "sourcer_session").is_none());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM app_user WHERE email = $1")
        .bind(&stranger)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn repeated_sign_in_starts_are_slowed_down_per_address() {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;
    let mut state = AppState::new(None, None);
    state.sign_in_limit = Arc::new(RateLimiter::new(2, std::time::Duration::from_secs(60)));
    let app = router(state);
    let from = |uri: &str, addr: &str| {
        let mut req = get_req(uri, None);
        req.extensions_mut()
            .insert(ConnectInfo(addr.parse::<SocketAddr>().unwrap()));
        req
    };
    for _ in 0..2 {
        let res = send(&app, from("/api/auth/login", "10.0.0.1:5000")).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    let res = send(&app, from("/api/auth/login", "10.0.0.1:5001")).await;
    assert_eq!(location(&res), "/?signin=busy");
    let res = send(&app, from("/api/auth/login", "10.0.0.2:5000")).await;
    assert_eq!(
        res.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "other addresses unaffected"
    );
    let res = send(&app, from("/api/auth/callback", "10.0.0.1:5000")).await;
    assert_eq!(
        res.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "callback is not limited"
    );
    let res = send(&app, get_req("/api/health", None)).await;
    assert_eq!(res.status(), StatusCode::OK, "other routes are not limited");
}

#[tokio::test]
async fn forged_state_and_cancelled_sign_in_fail_safely() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let app = app_for(pool, "http://127.0.0.1:9", "a@example.com");
    let forged = get_req(
        "/api/auth/callback?code=x&state=forged",
        Some("sourcer_oauth=forged"),
    );
    assert_eq!(location(&send(&app, forged).await), "/?signin=expired");
    let res = send(
        &app,
        get_req("/api/auth/callback?error=access_denied", None),
    )
    .await;
    assert_eq!(location(&res), "/?signin=cancelled");
}

#[tokio::test]
async fn changes_without_the_app_header_are_refused() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let forged = Request::builder()
        .method("POST")
        .uri("/api/clients")
        .header(header::COOKIE, &me)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"name": "X", "domain": "x.example", "off_limits": false}).to_string(),
        ))
        .unwrap();
    assert_eq!(send(&app, forged).await.status(), StatusCode::FORBIDDEN);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM client WHERE org_id = $1")
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(
        send(&app, get_req("/api/clients", Some(&me)))
            .await
            .status(),
        StatusCode::OK,
        "reads need no header"
    );
}
