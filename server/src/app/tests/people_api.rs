//! Saving a LinkedIn profile to a role.

use super::*;

#[tokio::test]
async fn a_linkedin_profile_is_saved_to_a_role_once_and_never_for_a_locked_out_employer() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let uri = searchable_role(&app, &me).await;
    let role_id = uri[11..].to_string();
    let save = |body: Value| json_req("POST", "/api/people/save", &me, body);
    let profile = |url: &str, employer: &str| {
        json!({"role_id": role_id, "linkedin_url": url, "name": " Sample Person ",
               "title": "Senior IAM Engineer", "employer": employer, "location": "Dubai"})
    };

    let res = send(
        &app,
        save(profile(
            "https://www.linkedin.com/in/sample-person/",
            "Payments firm",
        )),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(
        (
            v["added"].as_bool(),
            v["candidate"]["name"].as_str(),
            v["candidate"]["state"].as_str(),
            v["candidate"]["linkedin_url"].as_str()
        ),
        (
            Some(true),
            Some("Sample Person"),
            Some("found"),
            Some("linkedin.com/in/sample-person")
        )
    );
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM job WHERE org_id = $1 AND kind = 'candidates.rank' AND status = 'queued'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(queued, 1, "ranked like anyone found by a search");

    // The same profile again, from a country site: already on the role.
    let res = send(
        &app,
        save(profile(
            "https://uk.linkedin.com/in/sample-person?trk=x",
            "Payments firm",
        )),
    )
    .await;
    let v = json_body(res).await;
    assert_eq!(v["added"], false);
    let people: i64 = sqlx::query_scalar("SELECT count(*) FROM person WHERE org_id = $1")
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(people, 1);

    // A People Data Labs record keeps its own details; the page only fills gaps.
    sqlx::query(
        "UPDATE person SET pdl_id = 'p9', current_employer = 'Examplepay',
         current_employer_domain = 'examplepay.com' WHERE org_id = $1",
    )
    .bind(org)
    .execute(&pool)
    .await
    .unwrap();
    let res = send(
        &app,
        save(profile(
            "https://www.linkedin.com/in/sample-person/details/experience/",
            "Payments @ Examplepay",
        )),
    )
    .await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "profile sub-pages are accepted"
    );
    let kept: (String, Option<String>) = sqlx::query_as(
        "SELECT current_employer, current_employer_domain FROM person WHERE org_id = $1",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kept, ("Examplepay".into(), Some("examplepay.com".into())));

    // Staff of the hiring client are refused, and nothing is written.
    let res = send(
        &app,
        save(profile(
            "https://www.linkedin.com/in/client-staff",
            "Client S Ltd",
        )),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let written: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM person WHERE org_id = $1 AND linkedin_url = 'linkedin.com/in/client-staff'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(written, 0);
    // Someone already on record at the client is refused even if the page says otherwise.
    sqlx::query("UPDATE person SET current_employer = 'Client S' WHERE org_id = $1")
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();
    let res = send(
        &app,
        save(profile(
            "https://www.linkedin.com/in/sample-person",
            "Elsewhere",
        )),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    for (body, want) in [
        (
            profile("https://www.linkedin.com/company/acme", "X"),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"role_id": role_id, "linkedin_url": "https://www.linkedin.com/in/x", "name": "  "}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"role_id": Uuid::new_v4(), "linkedin_url": "https://www.linkedin.com/in/x", "name": "X"}),
            StatusCode::NOT_FOUND,
        ),
    ] {
        assert_eq!(send(&app, save(body)).await.status(), want);
    }
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'person.save_linkedin'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audited, 3);

    // Another organisation cannot save to this role.
    let other = testutil::org(&pool).await;
    let (_, outsider) = signed_in(&pool, other, "admin").await;
    let res = send(
        &app,
        json_req(
            "POST",
            "/api/people/save",
            &outsider,
            profile("https://www.linkedin.com/in/y", "X"),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_role_without_a_confirmed_brief_cannot_take_saved_profiles() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let app = plain_app(pool.clone());
    let client = new_client(&app, &me, "Client T", false).await;
    let body = json!({"client_id": client["id"], "title": "IAM", "spec_text": ""});
    let role = json_body(send(&app, json_req("POST", "/api/roles", &me, body)).await).await;
    let res = send(
        &app,
        json_req(
            "POST",
            "/api/people/save",
            &me,
            json!({"role_id": role["id"], "linkedin_url": "linkedin.com/in/z", "name": "Z"}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}
