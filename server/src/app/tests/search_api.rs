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

/// A stand-in for Claude choosing wider searches: wider titles, a nearby
/// market, and one that widens nothing. Returns (url, request bodies).
async fn fake_searches() -> (String, Bodies) {
    let bodies: Bodies = Arc::default();
    let seen = bodies.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(body);
                Json(
                    json!({"content": [{"type": "tool_use", "id": "t", "name": "record_searches",
                    "input": {"searches": [
                        {"name": "Wider titles", "note": "Adds titles the same work goes by.",
                         "add_titles": ["Security Engineer", "IAM Engineer"], "add_levels": [],
                         "add_locations": [], "add_employer_types": [], "tools_to_nice": [],
                         "domains_to_plus": []},
                        {"name": "Nearby markets", "note": "Adds Riyadh.",
                         "add_titles": [], "add_levels": [], "add_locations": ["Riyadh"],
                         "add_employer_types": [], "tools_to_nice": [], "domains_to_plus": []},
                        {"name": "Nothing", "note": "", "add_titles": ["security engineer"],
                         "add_levels": [], "add_locations": [], "add_employer_types": [],
                         "tools_to_nice": [], "domains_to_plus": []}
                    ]}}]}),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, bodies)
}

/// Runs every queued pull job of a pull, as the worker would.
async fn run_pull(pool: &PgPool, pdl: Arc<PdlClient>, pull_id: &str) -> Vec<Value> {
    let jobs: Vec<(Uuid, Uuid, String, Value, i32)> = sqlx::query_as(
        "SELECT id, org_id, kind, payload, attempts FROM job
         WHERE kind = 'search.pull' AND payload->>'pull_id' = $1",
    )
    .bind(pull_id)
    .fetch_all(pool)
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
    jobs.into_iter().map(|j| j.3).collect()
}

#[tokio::test]
async fn wider_searches_find_only_new_people_and_never_pay_twice() {
    use std::sync::atomic::Ordering;
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let bodies: Bodies = Arc::default();
    let (pdl_url, calls) = fake_pdl_seeing(30, bodies.clone()).await;
    let (claude_url, asked) = fake_searches().await;
    let pdl = Arc::new(PdlClient::with_base_url(Some("k".into()), &pdl_url));
    let mut state = AppState::new(Some(pool.clone()), None);
    state.pdl = pdl.clone();
    state.ai = Arc::new(Claude::with_base_url(Some("k".into()), &claude_url));
    let app = router(state);
    let uri = searchable_role(&app, &me).await;
    let suggest = || json_req("POST", &format!("{uri}/searches/suggest"), &me, json!({}));
    let post = |path: &str, body: Value| json_req("POST", &format!("{uri}/{path}"), &me, body);

    // Claude is not asked before the brief's search is counted.
    assert_eq!(send(&app, suggest()).await.status(), StatusCode::CONFLICT);
    assert!(asked.lock().unwrap().is_empty());
    let s = json_body(send(&app, post("search/count", json!({"key": "b1"}))).await).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let brief_count = s["count"]["id"].clone();

    // Two usable searches; the one that widens nothing is dropped.
    let s = json_body(send(&app, suggest()).await).await;
    let more = s["more"].as_array().unwrap();
    assert_eq!(more.len(), 2, "{more:?}");
    assert_eq!(
        (
            more[0]["slot"].as_i64(),
            more[0]["name"].as_str(),
            more[0]["by_claude"].as_bool()
        ),
        (Some(1), Some("Wider titles"), Some(true))
    );
    assert_eq!(
        more[0]["widen"]["add_titles"],
        json!(["IAM Engineer"]),
        "the brief's title drops out"
    );
    assert_eq!(more[0]["locations"], json!(["New York", "Dubai"]));
    assert_eq!(
        more[1]["locations"],
        json!(["Riyadh"]),
        "only the new place is searched"
    );
    assert!(more[1]["count"].is_null());
    let sent = asked.lock().unwrap()[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(sent.contains("\"people\": 30"), "{sent}");
    // Asking again returns them without asking Claude again.
    send(&app, suggest()).await;
    assert_eq!(asked.lock().unwrap().len(), 1);

    // Pull 10 from the brief.
    let s = json_body(
        send(
            &app,
            post(
                "search/pull",
                json!({"count_id": brief_count, "confirmed": false, "key": "bp",
                       "picks": [{"location": "Dubai", "size": 10}]}),
            ),
        )
        .await,
    )
    .await;
    assert_eq!(s["pulling"], true);
    assert_eq!(s["count"]["pulled"], true);
    run_pull(&pool, pdl.clone(), s["pull"]["id"].as_str().unwrap()).await;

    // Counting a wider search leaves out the 10 already found.
    let s = json_body(send(&app, post("searches/2/count", json!({"key": "c2"}))).await).await;
    assert_eq!(calls.load(Ordering::SeqCst), 4, "one credit, Riyadh only");
    let last = bodies.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        last["query"]["bool"]["must_not"][0]["terms"]["id"]
            .as_array()
            .map(Vec::len),
        Some(10)
    );
    let c = &s["more"][1]["count"];
    assert_eq!(c["locations"], json!([{"label": "Riyadh", "total": 30}]));
    assert_eq!(
        (c["stale"].as_bool(), c["pulled"].as_bool()),
        (Some(false), Some(false))
    );
    let old_count = c["id"].clone();

    // Changing the search makes its count stale, and a stale count is never pulled.
    let s = json_body(
        send(
            &app,
            json_req(
                "PUT",
                &format!("{uri}/searches/2"),
                &me,
                json!({"widen": {"add_locations": ["Riyadh", "Doha"]}}),
            ),
        )
        .await,
    )
    .await;
    assert_eq!(s["more"][1]["version"], 2);
    assert_eq!(s["more"][1]["count"]["stale"], true);
    let pick = |count: &Value, size: u32, key: &str| json!({"picks": [{"slot": 2, "count_id": count, "size": size}], "confirmed": false, "key": key});
    let res = send(&app, post("searches/pull", pick(&old_count, 10, "x"))).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // Count again: Riyadh and Doha. Pull 40 spread over both.
    let s = json_body(send(&app, post("searches/2/count", json!({"key": "c3"}))).await).await;
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    let count = s["more"][1]["count"]["id"].clone();
    let res = send(&app, post("searches/pull", pick(&count, 61, "y"))).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "more than it has");
    let s = json_body(send(&app, post("searches/pull", pick(&count, 40, "p"))).await).await;
    assert_eq!(s["pull"]["search_name"], "Nearby markets");
    assert_eq!(s["more"][1]["count"]["pulled"], true);
    let jobs = run_pull(&pool, pdl.clone(), s["pull"]["id"].as_str().unwrap()).await;
    let mut sizes: Vec<(String, u64)> = jobs
        .iter()
        .map(|j| {
            (
                j["location"].as_str().unwrap().to_string(),
                j["size"].as_u64().unwrap(),
            )
        })
        .collect();
    sizes.sort();
    assert_eq!(
        sizes,
        [("Doha".to_string(), 20), ("Riyadh".to_string(), 20)]
    );
    assert_eq!(jobs[0]["widen"]["add_locations"], json!(["Riyadh", "Doha"]));
    let found_by: Vec<(Option<String>, i64)> = sqlx::query_as(
        "SELECT e.name, count(*) FROM candidacy c LEFT JOIN extra_search e ON e.id = c.search_id
         WHERE c.org_id = $1 GROUP BY 1 ORDER BY 1",
    )
    .bind(org)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(found_by, [(Some("Nearby markets".into()), 40), (None, 10)]);
    // The worker searched the widened brief for Doha, leaving out everyone found.
    let doha = bodies
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|b| b.to_string().contains("doha"))
        .cloned()
        .unwrap();
    assert!(
        doha["query"]["bool"]["must_not"][0]["terms"]["id"]
            .as_array()
            .unwrap()
            .len()
            >= 10
    );

    // The same press again queues nothing; another press is refused.
    send(&app, post("searches/pull", pick(&count, 40, "p"))).await;
    let pulls: i64 = sqlx::query_scalar("SELECT count(*) FROM pull WHERE org_id = $1")
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pulls, 2);
    let res = send(&app, post("searches/pull", pick(&count, 5, "q"))).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // Your own search: it must widen something; slots stop at 3.
    let put = |slot: u32, widen: Value| {
        json_req(
            "PUT",
            &format!("{uri}/searches/{slot}"),
            &me,
            json!({"widen": widen}),
        )
    };
    let res = send(&app, put(3, json!({"add_titles": ["Security Engineer"]}))).await;
    assert_eq!(
        res.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "already the brief's"
    );
    let s = json_body(
        send(
            &app,
            put(3, json!({"add_levels": ["Principal"], "min_years": 3})),
        )
        .await,
    )
    .await;
    assert_eq!(
        (
            s["more"][2]["name"].as_str(),
            s["more"][2]["by_claude"].as_bool()
        ),
        (Some("Your search"), Some(false))
    );
    assert_eq!(s["more"][2]["widen"]["min_years"], 3);
    assert_eq!(
        send(&app, put(4, json!({"add_levels": ["Staff"]})))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(s["credits_this_month"], 2 + 10 + 1 + 2 + 40);

    // One press on two searches: one pull each, shown together.
    send(&app, post("searches/1/count", json!({"key": "c4"}))).await;
    let s = json_body(send(&app, post("searches/3/count", json!({"key": "c5"}))).await).await;
    let both = json!({"picks": [
        {"slot": 1, "count_id": s["more"][0]["count"]["id"], "size": 10},
        {"slot": 3, "count_id": s["more"][2]["count"]["id"], "size": 10}
    ], "confirmed": false, "key": "both"});
    let s = json_body(send(&app, post("searches/pull", both)).await).await;
    let p = &s["pull"];
    assert_eq!(
        (
            p["requested"].as_i64(),
            p["locations"].as_i64(),
            p["search_name"].as_str()
        ),
        (Some(20), Some(4), Some("Wider titles and Your search"))
    );

    // Confirming the brief again starts afresh.
    let mut l = lines(Some("required"));
    l["locations"] = json!(["Dubai"]);
    send(
        &app,
        post("brief/confirm", json!({"lines": l, "based_on": 1})),
    )
    .await;
    let s = json_body(send(&app, get_req(&format!("{uri}/search"), Some(&me))).await).await;
    assert_eq!(s["more"], json!([]));

    // Another organisation cannot see or use these searches.
    let other = testutil::org(&pool).await;
    let (_, outsider) = signed_in(&pool, other, "admin").await;
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("{uri}/searches/suggest"),
            &outsider,
            json!({}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// A stand-in for Claude's tightening: drop a level, raise the years, and
/// one change the spec does not back. Returns (url, request bodies).
async fn fake_tightening() -> (String, Bodies) {
    let bodies: Bodies = Arc::default();
    let seen = bodies.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(body);
                Json(
                    json!({"content": [{"type": "tool_use", "id": "t", "name": "record_tightening",
                    "input": {"why": "Two places and broad levels.", "moves": [
                        {"kind": "drop_level", "value": "Lead", "quote": "A senior, hands-on engineer"},
                        {"kind": "min_years", "value": "8", "quote": "8+ years in identity security"},
                        {"kind": "require_tool", "value": "Kubernetes", "quote": "Kubernetes is a must"}
                    ]}}]}),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, bodies)
}

#[tokio::test]
async fn too_many_is_tightened_from_the_spec_at_most_twice() {
    use std::sync::atomic::Ordering;
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let (pdl_url, calls) = fake_pdl(500).await;
    let (claude_url, asked) = fake_tightening().await;
    let mut state = AppState::new(Some(pool.clone()), None);
    state.pdl = Arc::new(PdlClient::with_base_url(Some("k".into()), &pdl_url));
    state.ai = Arc::new(Claude::with_base_url(Some("k".into()), &claude_url));
    let app = router(state);
    let uri = searchable_role(&app, &me).await;
    let post = |path: &str, body: Value| json_req("POST", &format!("{uri}/{path}"), &me, body);
    let ask = || post("search/tighten", json!({}));

    // Nothing to read before a count, and nothing to quote without a spec.
    assert_eq!(send(&app, ask()).await.status(), StatusCode::CONFLICT);
    send(&app, post("search/count", json!({"key": "k1"}))).await;
    let res = send(&app, ask()).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert!(body_text(res).await.contains("job spec"));
    assert!(asked.lock().unwrap().is_empty());
    sqlx::query("UPDATE role SET spec_text = $2 WHERE id = $1")
        .bind(Uuid::parse_str(uri.rsplit('/').next().unwrap()).unwrap())
        .bind("IAM Engineer. A senior, hands-on engineer with 8+ years in identity security.")
        .execute(&pool)
        .await
        .unwrap();

    // Claude's changes, each with its quote; the one the spec does not back is gone.
    let t = json_body(send(&app, ask()).await).await;
    assert_eq!(
        (t["round"].as_i64(), t["before"].as_i64()),
        (Some(1), Some(1000))
    );
    assert_eq!(t["why"], "Two places and broad levels.");
    let labels: Vec<&str> = t["moves"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["Drop the level Lead", "8+ years (was 5+)"]);
    let sent = asked.lock().unwrap()[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        sent.contains("<job_spec>") && sent.contains("\"people\": 500"),
        "{sent}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2, "asking searches nothing");

    // Keep only the first change; it becomes brief version 2, round 1.
    let keep = |based_on: i64, moves: Value| {
        post(
            "search/tighten/apply",
            json!({"based_on": based_on, "moves": moves}),
        )
    };
    let first = json!([t["moves"][0]]);
    assert_eq!(
        send(&app, keep(7, first.clone())).await.status(),
        StatusCode::CONFLICT
    );
    let bogus =
        json!([{"kind": "require_tool", "value": "Kubernetes", "quote": "Kubernetes is a must"}]);
    assert_eq!(
        send(&app, keep(1, bogus)).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let s = json_body(send(&app, keep(1, first)).await).await;
    assert_eq!(
        (s["brief_version"].as_i64(), s["tighten_round"].as_i64()),
        (Some(2), Some(1))
    );
    assert_eq!(s["lines"]["levels"], json!(["Senior"]));
    assert_eq!(s["lines"]["min_years"], 5);
    assert_eq!(s["count"]["stale"], true);

    // Round 2, then it stops.
    send(&app, post("search/count", json!({"key": "k2"}))).await;
    let t = json_body(send(&app, ask()).await).await;
    assert_eq!(t["round"], 2);
    assert_eq!(
        t["moves"].as_array().unwrap().len(),
        1,
        "Lead is already gone"
    );
    let s = json_body(send(&app, keep(2, t["moves"].clone())).await).await;
    assert_eq!(
        (
            s["tighten_round"].as_i64(),
            s["lines"]["min_years"].as_i64()
        ),
        (Some(2), Some(8))
    );
    send(&app, post("search/count", json!({"key": "k3"}))).await;
    let res = send(&app, ask()).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert!(body_text(res).await.contains("twice"));
    assert_eq!(asked.lock().unwrap().len(), 2);

    // Confirming the brief by hand starts again at round 0.
    let mut l = lines(Some("required"));
    l["locations"] = json!(["Dubai"]);
    let res = send(
        &app,
        post("brief/confirm", json!({"lines": l, "based_on": 3})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let s = json_body(send(&app, get_req(&format!("{uri}/search"), Some(&me))).await).await;
    assert_eq!(s["tighten_round"], 0);
}
