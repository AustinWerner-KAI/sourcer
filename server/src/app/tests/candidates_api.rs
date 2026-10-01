//! Ranking, the known check and decisions.

use super::*;

#[tokio::test]
async fn people_are_ranked_after_a_pull_without_names_reaching_claude() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let (app, uri, ranker, bodies) = role_with_people(&pool, org, &me, 3).await;

    // Before ranking: found, unranked, and a ranking is waiting.
    let v = candidates_of(&app, &uri, &me, "review").await;
    assert_eq!(
        (
            v["unranked"].as_i64(),
            v["ranking"].as_bool(),
            v["rank_blocked"].clone()
        ),
        (Some(3), Some(true), Value::Null)
    );
    assert_eq!(run_jobs(&pool, org, &ranker).await, 1);
    let v = candidates_of(&app, &uri, &me, "review").await;
    let tiers: Vec<&str> = v["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["tier"].as_str().unwrap())
        .collect();
    assert_eq!(tiers, ["A", "B", "C"], "best first");
    assert_eq!(
        (
            v["unranked"].as_i64(),
            v["ranking"].as_bool(),
            v["to_review"].as_i64()
        ),
        (Some(0), Some(false), Some(3))
    );
    let top = &v["people"][0];
    assert_eq!(top["state"], "ranked");
    assert_eq!(top["unknowns"], json!(["Python or Go"]));
    assert!(top["reason"].as_str().unwrap().contains("**IAM**"));
    // A verdict for each line of the brief, recorded and read back.
    let checks = top["checks"].as_array().unwrap();
    assert_eq!(
        checks
            .iter()
            .map(|c| c["item"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "Cloud security",
            "IAM",
            "Title: Security Engineer",
            "Level: Senior, Lead",
            "5+ years' experience",
            "CyberArk",
            "Privileged access"
        ]
    );
    assert!(checks.iter().all(|c| c["verdict"] == "met"));
    assert_eq!(v["people"][2]["checks"][0]["verdict"], "not_shown");
    let sent = bodies.lock().unwrap()[0].to_string();
    assert!(!sent.contains("Sample Person"), "names never reach Claude");
    assert!(sent.contains("Samplefirm"), "work evidence does");

    // Ranking again ranks no one twice.
    candidates::queue_rank(&pool, org, Uuid::parse_str(&uri[11..]).unwrap())
        .await
        .unwrap();
    run_jobs(&pool, org, &ranker).await;
    assert_eq!(bodies.lock().unwrap().len(), 1, "no second call to Claude");
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'candidates.rank'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);
}

#[tokio::test]
async fn shortlist_reject_and_reconsider_with_reasons_and_versions() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let (app, uri, ranker, _) = role_with_people(&pool, org, &me, 3).await;

    // Unranked people cannot be decided yet.
    let v = candidates_of(&app, &uri, &me, "review").await;
    let res = send(&app, decide_req(&me, &v["people"][0], "shortlist", None)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    run_jobs(&pool, org, &ranker).await;
    let v = candidates_of(&app, &uri, &me, "review").await;
    let (a, b) = (&v["people"][0], &v["people"][1]);
    let res = send(&app, decide_req(&me, a, "shortlist", None)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let shortlisted = json_body(res).await;
    assert_eq!(shortlisted["state"], "shortlisted");
    // The screen's old copy is stale now.
    let res = send(&app, decide_req(&me, a, "reject", Some("FIT"))).await;
    assert_eq!(res.status(), StatusCode::CONFLICT, "stale version refused");

    let res = send(&app, decide_req(&me, b, "reject", None)).await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "a reason is required"
    );
    let res = send(&app, decide_req(&me, b, "reject", Some("SENIOR"))).await;
    let rejected = json_body(res).await;
    assert_eq!(
        (
            rejected["state"].as_str(),
            rejected["reject_reason"].as_str()
        ),
        (Some("rejected"), Some("SENIOR"))
    );
    let v = candidates_of(&app, &uri, &me, "review").await;
    assert_eq!(
        (
            v["to_review"].as_i64(),
            v["shortlisted"].as_i64(),
            v["rejected"].as_i64()
        ),
        (Some(1), Some(1), Some(1))
    );
    let r = candidates_of(&app, &uri, &me, "rejected").await;
    assert_eq!(r["people"][0]["reject_reason"], "SENIOR");

    // Changed their mind: back to review, reason cleared.
    let res = send(&app, decide_req(&me, &rejected, "reconsider", None)).await;
    let back = json_body(res).await;
    assert_eq!(
        (back["state"].as_str(), back["reject_reason"].clone()),
        (Some("ranked"), Value::Null)
    );
    // Reconsider only undoes a rejection.
    let res = send(&app, decide_req(&me, &shortlisted, "reconsider", None)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action || ' ' || target FROM audit WHERE org_id = $1 AND action LIKE 'candidate.%' ORDER BY id",
    )
    .bind(org)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(actions.len(), 3);
    assert!(actions[1].starts_with("candidate.reject") && actions[1].ends_with("reason:SENIOR"));

    // Another organisation can neither see nor decide.
    let other = testutil::org(&pool).await;
    let (_, outsider) = signed_in(&pool, other, "admin").await;
    let res = send(&app, get_req(&format!("{uri}/candidates"), Some(&outsider))).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = send(&app, decide_req(&outsider, &back, "reject", Some("FIT"))).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn known_people_are_flagged_and_blocked_people_cannot_be_shortlisted() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let (app, uri, ranker, _) = role_with_people(&pool, org, &me, 3).await;
    run_jobs(&pool, org, &ranker).await;
    let v = candidates_of(&app, &uri, &me, "review").await;
    let ids: Vec<Uuid> = v["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| Uuid::parse_str(p["id"].as_str().unwrap()).unwrap())
        .collect();
    let person = |i: usize| {
        let pool = pool.clone();
        let id = ids[i];
        async move {
            sqlx::query_scalar::<_, Uuid>("SELECT person_id FROM candidacy WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };

    // Person 0 was contacted for another role; person 1 opted out;
    // person 2 has since joined the hiring client.
    let (p0, p1, p2) = (person(0).await, person(1).await, person(2).await);
    let other_role: Uuid = sqlx::query_scalar(
        "INSERT INTO role (org_id, title) VALUES ($1, 'Platform Lead') RETURNING id",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    let brief: Uuid = sqlx::query_scalar("SELECT brief_id FROM candidacy WHERE id = $1")
        .bind(ids[0])
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO candidacy (org_id, person_id, role_id, brief_id, state)
         VALUES ($1, $2, $3, $4, 'contacted')",
    )
    .bind(org)
    .bind(p0)
    .bind(other_role)
    .bind(brief)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE person SET opted_out = true WHERE id = $1")
        .bind(p1)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE person SET current_employer = 'Client S' WHERE id = $1")
        .bind(p2)
        .execute(&pool)
        .await
        .unwrap();

    for (who, kind, value) in [
        (p0, "phone", "+971 50 000 0000"),
        (p0, "personal_email", "zed.home@gmail.com"),
        (p0, "work_email", "zed@samplefirm.com"),
        (p1, "personal_email", "opted.out@gmail.com"),
    ] {
        sqlx::query(
            "INSERT INTO contact (org_id, person_id, kind, value, source)
             VALUES ($1, $2, $3::contact_kind, $4, 'pdl')",
        )
        .bind(org)
        .bind(who)
        .bind(kind)
        .bind(value)
        .execute(&pool)
        .await
        .unwrap();
    }

    let v = candidates_of(&app, &uri, &me, "review").await;
    let people = v["people"].as_array().unwrap();
    assert_eq!(
        people[0]["known"], "Known: contacted for Platform Lead",
        "flagged, still listed"
    );
    assert_eq!(
        people[0]["contacts"],
        json!([
            {"kind": "work_email", "value": "zed@samplefirm.com"},
            {"kind": "personal_email", "value": "zed.home@gmail.com"},
            {"kind": "phone", "value": "+971 50 000 0000"}
        ]),
        "work first, then personal, then phone"
    );
    assert_eq!(people[1]["do_not_contact"], true);
    assert_eq!(
        people[1]["contacts"],
        json!([]),
        "never shown for someone who must not be contacted"
    );
    // The resourcer decides for known people.
    let res = send(&app, decide_req(&me, &people[0], "shortlist", None)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = send(&app, decide_req(&me, &people[1], "shortlist", None)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT, "do not contact");
    let res = send(&app, decide_req(&me, &people[2], "shortlist", None)).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "now at the hiring client"
    );
    let res = send(
        &app,
        decide_req(&me, &people[2], "reject", Some("EMPLOYER")),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK, "rejecting is always allowed");
}

#[tokio::test]
async fn useless_replies_or_a_pause_stop_the_ranker_paying_again() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let (app, uri, _, _) = role_with_people(&pool, org, &me, 25).await;
    // A Claude that answers with nothing usable.
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let fake = Router::new().route(
        "/v1/messages",
        post(move || {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Json(json!({"content": [{"type": "tool_use", "id": "t", "name": "record_ranking",
                                          "input": {"candidates": [{"id": "zz", "tier": "A", "score": 1,
                                                                    "reason": "x", "unknowns": []}]}}]}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });
    let useless = candidates::RankHandler {
        pool: pool.clone(),
        ai: Arc::new(Claude::with_base_url(Some("k".into()), &url)),
    };
    run_jobs(&pool, org, &useless).await;
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "stops after a batch ranks no one"
    );
    let v = candidates_of(&app, &uri, &me, "review").await;
    assert_eq!(
        (v["unranked"].as_i64(), v["ranking"].as_bool()),
        (Some(25), Some(false))
    );

    // Paused: no call at all.
    sqlx::query("UPDATE org SET paid_calls_paused = true WHERE id = $1")
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();
    let role = Uuid::parse_str(&uri[11..]).unwrap();
    candidates::queue_rank(&pool, org, role).await.unwrap();
    run_jobs(&pool, org, &useless).await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let v = candidates_of(&app, &uri, &me, "review").await;
    assert!(v["rank_blocked"].as_str().unwrap().contains("paused"));
}

#[tokio::test]
async fn without_an_ai_key_people_wait_unranked_and_the_screen_says_why() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let (_, uri, _, bodies) = role_with_people(&pool, org, &me, 2).await;
    let keyless = candidates::RankHandler {
        pool: pool.clone(),
        ai: Arc::new(Claude::new(None, None)),
    };
    run_jobs(&pool, org, &keyless).await;
    let app = plain_app(pool.clone());
    let v = candidates_of(&app, &uri, &me, "review").await;
    assert_eq!(v["unranked"], 2);
    assert!(v["rank_blocked"].as_str().unwrap().contains("no AI key"));
    let res = send(
        &app,
        json_req("POST", &format!("{uri}/candidates/rank"), &me, json!({})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert!(bodies.lock().unwrap().is_empty(), "nothing sent to Claude");
}

#[tokio::test]
async fn a_new_brief_re_ranks_everyone_in_play_and_keeps_their_place() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let (app, uri, ranker, bodies) = role_with_people(&pool, org, &me, 3).await;
    run_jobs(&pool, org, &ranker).await;
    let v = candidates_of(&app, &uri, &me, "review").await;
    let res = send(&app, decide_req(&me, &v["people"][0], "shortlist", None)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = send(
        &app,
        decide_req(&me, &v["people"][2], "reject", Some("FIT")),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(v["stale"], 0);

    // The brief changes: version 2 is confirmed.
    let mut l = lines(Some("required"));
    l["locations"] = json!(["New York", "Dubai"]);
    l["must_haves"] = json!(["Cloud security", "IAM", "Terraform"]);
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("{uri}/brief/confirm"),
            &me,
            json!({"lines": l, "based_on": 1}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    // The first ranking was a while ago; a recent one would hold the re-rank back.
    sqlx::query("UPDATE job SET created_at = now() - interval '1 hour' WHERE org_id = $1")
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();
    let v = candidates_of(&app, &uri, &me, "review").await;
    assert_eq!(
        (v["stale"].as_i64(), v["ranking"].as_bool()),
        (Some(2), Some(true)),
        "the shortlisted and the one to review; not the rejected"
    );
    assert_eq!(v["people"][0]["stale_rank"], true);
    candidates_of(&app, &uri, &me, "review").await;
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM job WHERE org_id = $1 AND kind = 'candidates.rank' AND status = 'queued'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(queued, 1, "one re-rank, however often the list is opened");

    let sent_before = bodies.lock().unwrap().len();
    run_jobs(&pool, org, &ranker).await;
    let sent = bodies.lock().unwrap()[sent_before].to_string();
    assert!(sent.contains("Terraform"), "ranked against the new brief");
    let v = candidates_of(&app, &uri, &me, "review").await;
    assert_eq!(
        (
            v["stale"].as_i64(),
            v["to_review"].as_i64(),
            v["shortlisted"].as_i64(),
            v["rejected"].as_i64()
        ),
        (Some(0), Some(1), Some(1), Some(1)),
        "everyone keeps their place"
    );
    assert_eq!(v["people"][0]["stale_rank"], false);
    let s = candidates_of(&app, &uri, &me, "shortlisted").await;
    assert_eq!(s["people"][0]["state"], "shortlisted");
    assert_eq!(s["people"][0]["stale_rank"], false);
    let checks = s["people"][0]["checks"].as_array().unwrap();
    assert!(checks.iter().any(|c| c["item"] == "Terraform"));
}
