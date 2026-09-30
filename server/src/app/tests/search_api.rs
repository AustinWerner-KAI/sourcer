//! Counting and pulling people.

use super::*;

#[tokio::test]
async fn searching_is_blocked_without_a_key_and_says_why() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let uri = searchable_role(&app, &me).await;
    let s = json_body(send(&app, get_req(&format!("{uri}/search"), Some(&me))).await).await;
    assert!(s["blocked"].as_str().unwrap().contains("not set up"));
    assert_eq!(s["locations"], json!(["New York", "Dubai"]));
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("{uri}/search/count"),
            &me,
            json!({"key": "a"}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn count_then_pull_charges_once_and_saves_people() {
    use std::sync::atomic::Ordering;
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let bodies: Bodies = Arc::default();
    let (url, calls) = fake_pdl_seeing(245, bodies.clone()).await;
    let (app, pdl) = searching_app(pool.clone(), &url);
    let uri = searchable_role(&app, &me).await;
    let count_uri = format!("{uri}/search/count");
    let pull_uri = format!("{uri}/search/pull");

    // Count: one call per location; pressing again with the same key is free.
    let key = Uuid::new_v4().to_string();
    let s =
        json_body(send(&app, json_req("POST", &count_uri, &me, json!({"key": key}))).await).await;
    assert_eq!(
        s["count"]["locations"],
        json!([{"label": "New York", "total": 245}, {"label": "Dubai", "total": 245}])
    );
    assert_eq!(
        (
            s["count"]["stale"].as_bool(),
            s["credits_this_month"].as_i64()
        ),
        (Some(false), Some(2))
    );
    send(&app, json_req("POST", &count_uri, &me, json!({"key": key}))).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let count_id = s["count"]["id"].clone();

    // Choices are checked before anything is queued.
    let pick = |ny: u32, dxb: u32, confirmed: bool, key: &str| {
        json!({"count_id": count_id, "confirmed": confirmed, "key": key,
               "picks": [{"location": "New York", "size": ny}, {"location": "Dubai", "size": dxb}]})
    };
    for (body, why) in [
        (pick(0, 0, false, "x1"), "nothing chosen"),
        (pick(101, 0, true, "x2"), "over one page"),
        (pick(40, 20, false, "x3"), "over 50 needs confirming"),
        (
            json!({"count_id": count_id, "confirmed": true, "key": "x4",
                "picks": [{"location": "Paris", "size": 5}]}),
            "not counted",
        ),
    ] {
        let res = send(&app, json_req("POST", &pull_uri, &me, body)).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{why}");
    }

    // Pull 40 + 20, confirmed; a second press queues nothing more.
    let s = json_body(
        send(
            &app,
            json_req("POST", &pull_uri, &me, pick(40, 20, true, "p1")),
        )
        .await,
    )
    .await;
    assert_eq!(
        (s["pull"]["requested"].as_i64(), s["pull"]["done"].as_bool()),
        (Some(60), Some(false))
    );
    send(
        &app,
        json_req("POST", &pull_uri, &me, pick(40, 20, true, "p1")),
    )
    .await;
    let jobs: Vec<(Uuid, Uuid, String, Value, i32)> = sqlx::query_as(
        "SELECT id, org_id, kind, payload, attempts FROM job
         WHERE kind = 'search.pull' AND payload->>'pull_id' = $1",
    )
    .bind(s["pull"]["id"].as_str().unwrap())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(jobs.len(), 2, "one job per location");

    // The worker runs each location; running one again never charges twice.
    let handler = searching::PullHandler {
        pool: pool.clone(),
        source: pdl,
    };
    for (id, org_id, kind, payload, attempts) in jobs.iter().chain(jobs.iter().take(1)) {
        let job = crate::jobs::Job {
            id: *id,
            org_id: *org_id,
            kind: kind.clone(),
            payload: payload.clone(),
            attempts: *attempts,
        };
        crate::worker::JobHandler::handle(&handler, &job)
            .await
            .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4, "2 counts + 2 pulls");
    let s = json_body(send(&app, get_req(&format!("{uri}/search"), Some(&me))).await).await;
    let p = &s["pull"];
    assert_eq!(
        (
            p["pulled"].as_i64(),
            p["new_candidates"].as_i64(),
            p["done"].as_bool(),
            p["locations_done"].as_i64()
        ),
        (Some(60), Some(60), Some(true), Some(2))
    );
    assert_eq!(s["credits_this_month"].as_i64(), Some(62));
    let saved: i64 = sqlx::query_scalar("SELECT count(*) FROM candidacy WHERE org_id = $1")
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(saved, 60);
    // The second location's pull left out everyone the first one found.
    let last = bodies.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        last["query"]["bool"]["must_not"][0]["terms"]["id"]
            .as_array()
            .map(Vec::len),
        jobs[0].3["size"].as_u64().map(|n| n as usize)
    );
    // The same count cannot be pulled twice.
    let res = send(
        &app,
        json_req("POST", &pull_uri, &me, pick(5, 0, false, "p9")),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // Confirming a new version makes the count stale; pulling from it is refused.
    let mut l = lines(Some("required"));
    l["locations"] = json!(["Dubai"]);
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
    let s = json_body(send(&app, get_req(&format!("{uri}/search"), Some(&me))).await).await;
    assert_eq!(
        (s["count"]["stale"].as_bool(), s["brief_version"].as_i64()),
        (Some(true), Some(2))
    );
    let res = send(
        &app,
        json_req("POST", &pull_uri, &me, pick(5, 0, false, "p2")),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // Another organisation cannot see or use this role's search.
    let other = testutil::org(&pool).await;
    let (_, outsider) = signed_in(&pool, other, "admin").await;
    assert_eq!(
        send(&app, get_req(&format!("{uri}/search"), Some(&outsider)))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let res = send(
        &app,
        json_req("POST", &count_uri, &outsider, json!({"key": "o"})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_failed_location_is_named_with_its_unsaved_credits() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let (url, _) = fake_pdl(245).await;
    let (app, pdl) = searching_app(pool.clone(), &url);
    let uri = searchable_role(&app, &me).await;
    let s = json_body(
        send(
            &app,
            json_req(
                "POST",
                &format!("{uri}/search/count"),
                &me,
                json!({"key": "c"}),
            ),
        )
        .await,
    )
    .await;
    let body = json!({"count_id": s["count"]["id"], "confirmed": false, "key": "p",
        "picks": [{"location": "New York", "size": 25}, {"location": "Dubai", "size": 25}]});
    let s = json_body(
        send(
            &app,
            json_req("POST", &format!("{uri}/search/pull"), &me, body),
        )
        .await,
    )
    .await;
    let pull_id = s["pull"]["id"].as_str().unwrap().to_string();
    let jobs: Vec<(Uuid, Uuid, String, Value, i32)> = sqlx::query_as(
        "SELECT id, org_id, kind, payload, attempts FROM job
         WHERE kind = 'search.pull' AND payload->>'pull_id' = $1",
    )
    .bind(&pull_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    let handler = searching::PullHandler {
        pool: pool.clone(),
        source: pdl,
    };
    for (id, org_id, kind, payload, attempts) in &jobs {
        let job = crate::jobs::Job {
            id: *id,
            org_id: *org_id,
            kind: kind.clone(),
            payload: payload.clone(),
            attempts: *attempts,
        };
        crate::worker::JobHandler::handle(&handler, &job)
            .await
            .unwrap();
    }
    // Dubai was charged but its save never finished, and its job gave up.
    sqlx::query(
        "UPDATE run SET finished_at = NULL WHERE pull_id = $1::uuid AND location = 'Dubai'",
    )
    .bind(&pull_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE job SET status = 'failed' WHERE payload->>'pull_id' = $1
         AND payload->>'location' = 'Dubai'",
    )
    .bind(&pull_id)
    .execute(&pool)
    .await
    .unwrap();
    let s = json_body(send(&app, get_req(&format!("{uri}/search"), Some(&me))).await).await;
    let p = &s["pull"];
    assert_eq!(
        (
            p["done"].as_bool(),
            p["failed_locations"].clone(),
            p["pulled"].as_i64(),
            p["credits_used"].as_i64(),
            p["credits_unsaved"].as_i64(),
        ),
        (Some(true), json!(["Dubai"]), Some(25), Some(50), Some(25))
    );
}

#[tokio::test]
async fn paused_organisation_cannot_count() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "admin").await;
    let (url, calls) = fake_pdl(10).await;
    let (app, _) = searching_app(pool.clone(), &url);
    let uri = searchable_role(&app, &me).await;
    sqlx::query("UPDATE org SET paid_calls_paused = true WHERE id = $1")
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("{uri}/search/count"),
            &me,
            json!({"key": "k"}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nothing spent"
    );
}
