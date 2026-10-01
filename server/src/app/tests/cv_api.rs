//! CV assessment, feedback and the feedback loop. Synthetic CVs only.

use super::*;

/// A made-up CV with the person's name and contact details in it.
fn sample_cv() -> String {
    let mut cv = String::from(
        "SAMPLE PERSON\nDubai | +971 50 111 2222 | sample.person@example.com | linkedin.com/in/sample-person\n\n",
    );
    for year in 2015..2025 {
        cv.push_str(&format!(
            "Security Engineer at Firm {year}, {year}-{}. Built AWS guardrails with Terraform; Person led CyberArk work.\n",
            year + 1
        ));
    }
    cv
}

fn upload_req(me: &str, candidacy: &str, roles: &str, name: &str, body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!(
            "/api/candidates/{candidacy}/cv?name={name}&roles={roles}"
        ))
        .header(header::COOKIE, me)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(CHANGE_HEADER, "1")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn a_cv_is_assessed_without_personal_details_and_feedback_teaches_ranking() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let (app, uri, ranker, bodies) = role_with_people(&pool, org, &me, 1).await;
    run_jobs(&pool, org, &ranker).await;
    let v = candidates_of(&app, &uri, &me, "review").await;
    let person = &v["people"][0];
    let id = person["id"].as_str().unwrap().to_string();
    assert_eq!(person["cv_score"], Value::Null);

    // Not a CV: refused before anything is sent.
    let res = send(&app, upload_req(&me, &id, "", "cv.gif", b"GIF89a".to_vec())).await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let sent_before = bodies.lock().unwrap().len();

    let res = send(
        &app,
        upload_req(&me, &id, "", "cv.txt", sample_cv().into_bytes()),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let view = json_body(res).await;
    let a = &view["cv"]["assessments"];
    assert_eq!(a.as_array().unwrap().len(), 1);
    assert_eq!(
        (
            a[0]["score"].as_i64(),
            a[0]["this_role"].as_bool(),
            a[0]["fit_title"].as_str()
        ),
        (Some(7), Some(true), Some("Fit 1"))
    );
    assert_eq!(
        a[0]["profile_score"], person["score"],
        "the profile rank, for comparison"
    );
    assert_eq!(view["cv"]["call"], "Call this week.");
    assert_eq!(view["cv"]["file_name"], "cv.txt");

    // Claude never saw who it is, and nothing personal was kept.
    let sent = bodies.lock().unwrap()[sent_before].to_string();
    let stored: String = sqlx::query_scalar("SELECT text FROM cv WHERE org_id = $1")
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
    for text in [&sent, &stored] {
        for gone in ["SAMPLE", "Person", "+971", "sample.person@", "linkedin.com"] {
            assert!(!text.contains(gone), "{gone} leaked");
        }
        assert!(text.contains("CyberArk") && text.contains("2015-2016"));
    }
    assert!(!sent.contains("recruiter_feedback"), "no feedback yet");

    // The card shows the score.
    let v = candidates_of(&app, &uri, &me, "review").await;
    assert_eq!(v["people"][0]["cv_score"], 7);

    // Feedback: the score must agree with the verdict.
    let fid = a[0]["id"].as_str().unwrap();
    let fb = |body: Value| {
        json_req(
            "PUT",
            &format!("/api/cv-assessments/{fid}/feedback"),
            &me,
            body,
        )
    };
    let res = send(&app, fb(json!({"verdict": "too_high", "your_score": 8}))).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = send(
        &app,
        fb(json!({"verdict": "too_high", "your_score": 5, "note": "Integrator work, not in-house", "vital": [0, 9]})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let view = json_body(
        send(
            &app,
            get_req(&format!("/api/candidates/{id}/cv"), Some(&me)),
        )
        .await,
    )
    .await;
    let f = &view["cv"]["assessments"][0]["feedback"];
    assert_eq!(
        (
            f["verdict"].as_str(),
            f["your_score"].as_i64(),
            f["vital"].clone()
        ),
        (Some("too_high"), Some(5), json!([0])),
        "unknown question numbers dropped"
    );

    // The loop: the feedback reaches the next ranking as a lesson.
    let lessons = crate::cv::lessons(&pool, org).await.unwrap();
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].contains("too high (their score 5/10)"));
    assert!(lessons[0].contains("Integrator work, not in-house"));
    assert!(lessons[0].contains("Which parts did you deploy yourself?"));
    let role = Uuid::parse_str(&uri[11..]).unwrap();
    sqlx::query("UPDATE candidacy SET state = 'found' WHERE role_id = $1")
        .bind(role)
        .execute(&pool)
        .await
        .unwrap();
    candidates::queue_rank(&pool, org, role).await.unwrap();
    run_jobs(&pool, org, &ranker).await;
    let last = bodies.lock().unwrap().last().unwrap().to_string();
    assert!(last.contains("record_ranking") && last.contains("their score 5/10"));

    // Assessed against a second role: the same CV, calibrated by the feedback.
    let other: Uuid = sqlx::query_scalar(
        "INSERT INTO role (org_id, title, spec_text) VALUES ($1, 'Senior IAM Engineer', 'Lead CyberArk and Entra PIM.') RETURNING id",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("/api/candidates/{id}/cv/assess"),
            &me,
            json!({"role_ids": [other]}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let view = json_body(res).await;
    let a = view["cv"]["assessments"].as_array().unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(
        (a[0]["this_role"].as_bool(), a[1]["role_title"].as_str()),
        (Some(true), Some("Senior IAM Engineer"))
    );
    assert!(
        a[0]["feedback"].is_object(),
        "feedback on this role is kept"
    );
    let last = bodies.lock().unwrap().last().unwrap().to_string();
    assert!(last.contains("recruiter_feedback") && last.contains("Senior IAM Engineer"));

    // Recruitly is not set up here.
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("/api/candidates/{id}/cv/recruitly"),
            &me,
            json!({}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit WHERE org_id = $1 AND action LIKE 'cv.%'")
            .bind(org)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audited, 3, "two assessments and one feedback");

    // Another organisation sees nothing.
    let other_org = testutil::org(&pool).await;
    let (_, outsider) = signed_in(&pool, other_org, "admin").await;
    let res = send(
        &app,
        get_req(&format!("/api/candidates/{id}/cv"), Some(&outsider)),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = send(
        &app,
        json_req(
            "PUT",
            &format!("/api/cv-assessments/{fid}/feedback"),
            &outsider,
            json!({"verdict": "accurate"}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
