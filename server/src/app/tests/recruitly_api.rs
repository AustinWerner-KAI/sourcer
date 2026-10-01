//! The Recruitly link: roles from jobs, the check at shortlist, and handover.

use super::*;
use crate::recruitly::Recruitly;
use axum::http::{Method, Uri};
use axum::response::IntoResponse;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// A stand-in for Recruitly. Candidates live in `candidates`; every call is
/// recorded as "METHOD /path?query" without the key.
#[derive(Default)]
struct FakeRc {
    calls: Mutex<Vec<String>>,
    posted: Mutex<Vec<(String, Value)>>,
    candidates: Mutex<Vec<Value>>,
    me_email: Mutex<String>,
    down: AtomicBool,
    refuse_notes: AtomicBool,
}

impl FakeRc {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn posted(&self, prefix: &str) -> Vec<Value> {
        self.posted
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.0.starts_with(prefix))
            .map(|p| p.1.clone())
            .collect()
    }
}

fn ok(data: Value) -> Json<Value> {
    Json(json!({"success": true, "data": data}))
}

async fn fake_recruitly(f: Arc<FakeRc>) -> String {
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, body: axum::body::Bytes| {
            let f = f.clone();
            async move {
                let q: std::collections::HashMap<String, String> = uri
                    .query()
                    .map(|q| {
                        url_pairs(q)
                            .into_iter()
                            .collect::<std::collections::HashMap<_, _>>()
                    })
                    .unwrap_or_default();
                assert_eq!(q.get("apiKey").map(String::as_str), Some("rk"), "key sent");
                let path = uri.path().to_string();
                let words = q.get("query").cloned().unwrap_or_default().to_lowercase();
                f.calls.lock().unwrap().push(format!("{method} {path} {words}"));
                if f.down.load(Ordering::SeqCst) {
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))).into_response();
                }
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                if method == Method::POST {
                    f.posted.lock().unwrap().push((path.clone(), body.clone()));
                }
                let parts: Vec<&str> = path.trim_start_matches("/api/nova/").split('/').collect();
                let res = match (method.as_str(), parts.as_slice()) {
                    ("GET", ["users", "me"]) => ok(json!({"id": "u-kai", "firstName": "Kai"})),
                    ("GET", ["users", "list"]) => {
                        ok(json!([{"id": "u-me", "email": f.me_email.lock().unwrap().clone()},
                                  {"id": "u-teo", "email": "teo@example.com"}]))
                    }
                    ("GET", ["jobs", "search"]) => ok(json!({"content": [
                        {"id": "j1", "title": "Senior IAM Engineer", "reference": "J-1042",
                         "companyName": "Client R", "statusName": "Open"},
                        {"id": "j2", "title": "Platform Lead", "companyName": "Other Co"}]})),
                    ("GET", ["jobs", "j1"]) => ok(json!({"id": "j1", "title": "Senior IAM Engineer",
                        "reference": "J-1042", "companyId": "co1", "companyName": "Client R",
                        "description": "<p>Lead <b>IAM</b> &amp; PAM.</p>", "location": "Dubai",
                        "minPay": 30000, "maxPay": 40000, "payCurrency": "AED"})),
                    ("GET", ["jobs", "j2"]) => ok(json!({"id": "j2", "title": "Platform Lead",
                        "companyId": "co2", "companyName": "Other Co"})),
                    ("GET", ["companies", "co1"]) => {
                        ok(json!({"id": "co1", "name": "Client R", "website": "https://www.client-r.example/"}))
                    }
                    ("GET", ["companies", "co2"]) => ok(json!({"id": "co2", "name": "Other Co"})),
                    ("GET", ["candidates", "search"]) => {
                        let hits: Vec<Value> = f
                            .candidates
                            .lock()
                            .unwrap()
                            .iter()
                            .filter(|c| {
                                let li = c["linkedIn"].as_str().unwrap_or("").to_lowercase();
                                let email = c["email"].as_str().unwrap_or("").to_lowercase();
                                let name = format!(
                                    "{} {}",
                                    c["firstName"].as_str().unwrap_or(""),
                                    c["lastName"].as_str().unwrap_or("")
                                )
                                .to_lowercase();
                                !words.is_empty()
                                    && (li.contains(&words) || email == words || name == words)
                            })
                            .cloned()
                            .collect();
                        ok(json!({"content": hits}))
                    }
                    // A record the search still shows but that cannot be read.
                    ("GET", ["candidates", id]) if id.starts_with("rc-gone") => {
                        return StatusCode::NOT_FOUND.into_response()
                    }
                    ("GET", ["candidates", id]) => {
                        let found = f
                            .candidates
                            .lock()
                            .unwrap()
                            .iter()
                            .find(|c| c["id"] == *id)
                            .cloned();
                        match found {
                            Some(c) => ok(c),
                            None => return StatusCode::NOT_FOUND.into_response(),
                        }
                    }
                    ("POST", ["candidates"]) => {
                        let mut c = body.clone();
                        let id = format!("new-{}", f.candidates.lock().unwrap().len() + 1);
                        c["id"] = json!(id);
                        c["doNotContact"] = json!(false);
                        f.candidates.lock().unwrap().push(c);
                        ok(json!(id))
                    }
                    ("POST", ["jobs", _, "pipeline"]) => ok(json!("pl-1")),
                    ("POST", ["journal", _]) if f.refuse_notes.load(Ordering::SeqCst) => {
                        return (StatusCode::BAD_REQUEST, Json(json!({"success": false, "message": "note refused"})))
                            .into_response()
                    }
                    ("POST", ["journal", _]) => ok(json!({"id": "n-1"})),
                    _ => return StatusCode::NOT_FOUND.into_response(),
                };
                res.into_response()
            }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

fn url_pairs(q: &str) -> Vec<(String, String)> {
    reqwest::Url::parse(&format!("http://x/?{q}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

fn recruitly_app(pool: PgPool, url: &str, cap: Option<i64>) -> Router {
    let mut state = AppState::new(Some(pool), None);
    state.recruitly = Arc::new(Recruitly::with_base_url(Some("rk".into()), url, cap));
    router(state)
}

async fn email_of(pool: &PgPool, user: Uuid) -> String {
    sqlx::query_scalar("SELECT email FROM app_user WHERE id = $1")
        .bind(user)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_role_starts_from_a_recruitly_job_with_its_client_and_spec() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let f = Arc::new(FakeRc::default());
    let app = recruitly_app(pool.clone(), &fake_recruitly(f.clone()).await, None);

    let jobs = json_body(send(&app, get_req("/api/recruitly/jobs?q=iam", Some(&me))).await).await;
    assert_eq!(jobs[0]["id"], "j1");
    assert_eq!(jobs[0]["role_id"], Value::Null);

    // The client is not in Sourcer yet: the preview gives its name and domain.
    let p = json_body(send(&app, get_req("/api/recruitly/jobs/j1", Some(&me))).await).await;
    assert_eq!(
        (
            p["label"].as_str(),
            p["company_name"].as_str(),
            p["company_domain"].as_str(),
            p["client_id"].clone()
        ),
        (
            Some("Senior IAM Engineer (J-1042)"),
            Some("Client R"),
            Some("client-r.example"),
            Value::Null
        )
    );
    assert_eq!(
        p["spec_text"],
        "Lead IAM & PAM.\n\nLocation: Dubai\nPay: 30,000 to 40,000 AED\nRecruitly job: J-1042"
    );
    let nothing: i64 = sqlx::query_scalar("SELECT count(*) FROM role WHERE org_id = $1")
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(nothing, 0, "a preview saves nothing");

    // Added the usual way, the client is then matched by its domain.
    let body = json!({"name": "Client R", "domain": "client-r.example", "off_limits": false});
    let client = json_body(send(&app, json_req("POST", "/api/clients", &me, body)).await).await;
    let p = json_body(send(&app, get_req("/api/recruitly/jobs/j1", Some(&me))).await).await;
    assert_eq!(p["client_id"], client["id"]);

    let save = json!({"job_id": "j1", "client_id": client["id"], "title": p["title"], "spec_text": p["spec_text"]});
    let res = send(
        &app,
        json_req("POST", "/api/roles/from-recruitly", &me, save.clone()),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let role = json_body(res).await;
    assert_eq!(
        role["recruitly_job"],
        json!({"id": "j1", "label": "Senior IAM Engineer (J-1042)"})
    );
    assert_eq!(role["client"]["id"], client["id"]);
    let res = send(
        &app,
        json_req("POST", "/api/roles/from-recruitly", &me, save),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT, "one role per job");
    let jobs = json_body(send(&app, get_req("/api/recruitly/jobs", Some(&me))).await).await;
    assert_eq!(jobs[0]["role_id"], role["id"]);
    // The company is now remembered for that client.
    let p = json_body(send(&app, get_req("/api/recruitly/jobs/j1", Some(&me))).await).await;
    assert_eq!(p["client_id"], client["id"]);

    // A job for another company cannot be linked to this role.
    let uri = format!("/api/roles/{}/recruitly", role["id"].as_str().unwrap());
    let res = send(&app, json_req("PUT", &uri, &me, json!({"job_id": "j2"}))).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let res = send(&app, json_req("PUT", &uri, &me, json!({"job_id": null}))).await;
    assert_eq!(json_body(res).await["recruitly_job"], Value::Null);
    // Odd ids never reach Recruitly.
    let before = f.calls().len();
    let res = send(
        &app,
        json_req("PUT", &uri, &me, json!({"job_id": "../users"})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(f.calls().len(), before);

    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE org_id = $1 AND action IN ('role.recruitly_import', 'role.recruitly_link')",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audited, 2);

    // Another organisation cannot use this role.
    let (_, outsider) = signed_in(&pool, testutil::org(&pool).await, "admin").await;
    let res = send(
        &app,
        json_req("PUT", &uri, &outsider, json!({"job_id": "j1"})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// A role with three ranked people, each with a LinkedIn address, on an app
/// that talks to the fake Recruitly. Returns (app, role uri, people rows).
async fn ranked_role(
    pool: &PgPool,
    org: Uuid,
    me: &str,
    f: Arc<FakeRc>,
    cap: Option<i64>,
) -> (Router, String, Vec<Value>) {
    let (plain, uri, ranker, _) = role_with_people(pool, org, me, 3).await;
    run_jobs(pool, org, &ranker).await;
    let v = candidates_of(&plain, &uri, me, "review").await;
    for (i, p) in v["people"].as_array().unwrap().iter().enumerate() {
        sqlx::query(
            "UPDATE person SET linkedin_url = $2, full_name = $3
             WHERE id = (SELECT person_id FROM candidacy WHERE id = $1)",
        )
        .bind(Uuid::parse_str(p["id"].as_str().unwrap()).unwrap())
        .bind(format!("linkedin.com/in/person-{i}-{}", Uuid::new_v4()))
        .bind(format!("Person {i}"))
        .execute(pool)
        .await
        .unwrap();
    }
    let app = recruitly_app(pool.clone(), &fake_recruitly(f).await, cap);
    let v = candidates_of(&app, &uri, me, "review").await;
    (app, uri, v["people"].as_array().unwrap().clone())
}

async fn linkedin_of(pool: &PgPool, row: &Value) -> String {
    sqlx::query_scalar(
        "SELECT p.linkedin_url FROM person p JOIN candidacy c ON c.person_id = p.id WHERE c.id = $1",
    )
    .bind(Uuid::parse_str(row["id"].as_str().unwrap()).unwrap())
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn shortlisting_checks_recruitly_and_do_not_contact_there_blocks_it() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let f = Arc::new(FakeRc::default());
    let (app, uri, people) = ranked_role(&pool, org, &me, f.clone(), None).await;
    assert_eq!(
        candidates_of(&app, &uri, &me, "review").await["recruitly"],
        true
    );
    let li0 = linkedin_of(&pool, &people[0]).await;
    let li1 = linkedin_of(&pool, &people[1]).await;
    f.candidates.lock().unwrap().extend([
        json!({"id": "rc-0", "firstName": "Person", "lastName": "0", "linkedIn": format!("https://www.{li0}/"),
               "ownerName": "Teo", "ownerId": "u-teo", "statusName": "Interviewing",
               "lastActivityDate": "2026-09-12T10:00:00Z", "doNotContact": false}),
        json!({"id": "rc-1", "firstName": "Person", "lastName": "1", "linkedIn": li1, "doNotContact": true}),
        // Same name as person 2, nothing else in common.
        json!({"id": "rc-2", "firstName": "Person", "lastName": "2", "currentEmployer": "Elsewhere"}),
    ]);

    let res = send(&app, decide_req(&me, &people[0], "shortlist", None)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let row = json_body(res).await;
    assert_eq!(
        row["recruitly_note"],
        "In Recruitly: owned by Teo · Interviewing · last activity 12 Sep 2026"
    );
    assert_eq!(row["recruitly_checked"], true);

    let res = send(&app, decide_req(&me, &people[1], "shortlist", None)).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "do not contact in Recruitly"
    );
    // Later the search stops saying so and the record cannot be read: the flag stays.
    {
        let mut c = f.candidates.lock().unwrap();
        c[1].as_object_mut().unwrap().remove("doNotContact");
        c[1]["id"] = json!("rc-gone");
    }
    let res = send(&app, decide_req(&me, &people[1], "shortlist", None)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT, "still do not contact");
    let kept: bool = sqlx::query_scalar(
        "SELECT p.recruitly_check_failed AND p.recruitly_dnc FROM person p
         JOIN candidacy c ON c.person_id = p.id WHERE c.id = $1",
    )
    .bind(Uuid::parse_str(people[1]["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(kept, "marked unchecked, flag kept");

    // A duplicate record of the same person that says "contactable" cannot clear
    // the flag set on the first record.
    {
        let mut c = f.candidates.lock().unwrap();
        c.remove(1);
        c.push(json!({"id": "rc-1b", "firstName": "Person", "lastName": "1", "linkedIn": li1, "doNotContact": false}));
    }
    let res = send(&app, decide_req(&me, &people[1], "shortlist", None)).await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "duplicate cannot clear it"
    );
    let dnc: bool = sqlx::query_scalar(
        "SELECT p.recruitly_dnc FROM person p JOIN candidacy c ON c.person_id = p.id WHERE c.id = $1",
    )
    .bind(Uuid::parse_str(people[1]["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(dnc);

    let res = send(&app, decide_req(&me, &people[2], "shortlist", None)).await;
    let row = json_body(res).await;
    assert_eq!(
        row["recruitly_note"],
        "Possible match in Recruitly: Person 2 at Elsewhere. Check before contact."
    );

    // Recruitly down: the shortlist still goes ahead, flagged as not checked.
    f.down.store(true, Ordering::SeqCst);
    let res = send(&app, decide_req(&me, &row, "reject", Some("FIT"))).await;
    let rejected = json_body(res).await;
    let res = send(&app, decide_req(&me, &rejected, "reconsider", None)).await;
    let back = json_body(res).await;
    let res = send(&app, decide_req(&me, &back, "shortlist", None)).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(json_body(res).await["recruitly_check_failed"], true);
    let uri2 = format!(
        "/api/candidates/{}/recruitly-check",
        back["id"].as_str().unwrap()
    );
    let res = send(&app, json_req("POST", &uri2, &me, json!({}))).await;
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let msg =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(!msg.contains("rk") && msg.contains("Recruitly"), "{msg}");
    let s = json_body(send(&app, get_req("/api/recruitly/status", Some(&me))).await).await;
    assert_eq!(
        s["connected"], false,
        "Recruitly failing shows as not connected"
    );
    f.down.store(false, Ordering::SeqCst);
    let res = send(&app, json_req("POST", &uri2, &me, json!({}))).await;
    assert_eq!(json_body(res).await["recruitly_check_failed"], false);

    // A record that does not say whether they can be contacted is a failed check.
    f.candidates.lock().unwrap()[0]
        .as_object_mut()
        .unwrap()
        .remove("doNotContact");
    let uri0 = format!(
        "/api/candidates/{}/recruitly-check",
        people[0]["id"].as_str().unwrap()
    );
    let res = send(&app, json_req("POST", &uri0, &me, json!({}))).await;
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let failed: bool = sqlx::query_scalar(
        "SELECT p.recruitly_check_failed FROM person p JOIN candidacy c ON c.person_id = p.id WHERE c.id = $1",
    )
    .bind(Uuid::parse_str(people[0]["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(failed, "unknown is never contactable");

    let checks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'person.recruitly_check'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(checks, 5);
}

#[tokio::test]
async fn handover_creates_once_and_asks_before_sending_a_colleagues_person() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (me_id, me) = signed_in(&pool, org, "resourcer").await;
    let f = Arc::new(FakeRc::default());
    *f.me_email.lock().unwrap() = email_of(&pool, me_id).await;
    let (app, uri, people) = ranked_role(&pool, org, &me, f.clone(), None).await;
    let role_id = &uri[11..];
    sqlx::query("UPDATE role SET recruitly_job_id = 'j1', recruitly_job_label = 'Senior IAM Engineer (J-1042)' WHERE id = $1::uuid")
        .bind(role_id)
        .execute(&pool)
        .await
        .unwrap();
    let hand = |row: &Value, confirmed: &[&str]| {
        json_req(
            "POST",
            &format!("/api/candidates/{}/handover", row["id"].as_str().unwrap()),
            &me,
            json!({ "confirmed": confirmed }),
        )
    };

    // Not shortlisted yet.
    let res = send(&app, hand(&people[0], &[])).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // Person 0 is new to Recruitly: created, put in the job, noted.
    sqlx::query(
        "INSERT INTO contact (org_id, person_id, kind, value, source)
         SELECT $1, person_id, 'work_email', $3, 'pdl' FROM candidacy WHERE id = $2",
    )
    .bind(org)
    .bind(Uuid::parse_str(people[0]["id"].as_str().unwrap()).unwrap())
    .bind(format!("p0-{}@payments.example", Uuid::new_v4()))
    .execute(&pool)
    .await
    .unwrap();
    let row = json_body(send(&app, decide_req(&me, &people[0], "shortlist", None)).await).await;
    assert_eq!(row["recruitly_note"], Value::Null, "not in Recruitly");
    let v = json_body(send(&app, hand(&row, &[])).await).await;
    let sent = &v["candidate"];
    assert!(sent["sent_to_recruitly"].is_string());
    assert_eq!(sent["in_recruitly_pipeline"], true);
    let created = f.posted("/api/nova/candidates");
    assert_eq!(created.len(), 1);
    let c = &created[0];
    assert_eq!(
        (
            c["firstName"].as_str(),
            c["lastName"].as_str(),
            c["ownerId"].as_str()
        ),
        (Some("Person"), Some("0"), Some("u-me"))
    );
    assert!(c["email"].as_str().unwrap().ends_with("@payments.example"));
    assert!(c["linkedIn"]
        .as_str()
        .unwrap()
        .starts_with("https://www.linkedin.com/in/person-0-"));
    assert_eq!(
        f.posted("/api/nova/jobs/j1/pipeline"),
        [json!({"candidateId": "new-1"})]
    );
    let notes = f.posted("/api/nova/journal/new-1");
    assert!(notes[0]["note"]
        .as_str()
        .unwrap()
        .starts_with("From Sourcer: shortlisted by Someone for IAM at Client S. Tier A"));
    // Pressed again: nothing more is sent.
    let before = f.calls().len();
    let v = json_body(send(&app, hand(&row, &[])).await).await;
    assert!(v["candidate"]["sent_to_recruitly"].is_string());
    assert_eq!(f.calls().len(), before);

    // Person 1 is Teo's in Recruitly: ask first, then use Teo's record.
    let li1 = linkedin_of(&pool, &people[1]).await;
    f.candidates.lock().unwrap().push(json!({"id": "rc-teo", "firstName": "Person", "lastName": "1",
        "linkedIn": li1, "ownerName": "Teo", "ownerId": "u-teo", "statusName": "Interviewing", "doNotContact": false}));
    let row = json_body(send(&app, decide_req(&me, &people[1], "shortlist", None)).await).await;
    // The screen compares the owner with the signed-in user's own Recruitly user.
    assert_eq!(row["recruitly_owner_id"], "u-teo");
    let who = json_body(send(&app, get_req("/api/me", Some(&me))).await).await;
    assert_eq!(who["recruitly_user_id"], "u-me", "learnt once and kept");
    let v = json_body(send(&app, hand(&row, &[])).await).await;
    assert_eq!(
        v["confirm"],
        "Teo owns this person in Recruitly (Interviewing). Add them to Senior IAM Engineer (J-1042) anyway?"
    );
    assert_eq!(v["candidate"], Value::Null);
    assert_eq!(
        f.posted("/api/nova/jobs/j1/pipeline").len(),
        1,
        "nothing sent yet"
    );
    assert_eq!(v["confirm_key"], "owner:u-teo");
    // A yes to a different question does not pass this one.
    let v = json_body(send(&app, hand(&row, &["namesake"])).await).await;
    assert_eq!(v["confirm_key"], "owner:u-teo");
    // Recruitly refuses the note: nothing was stored, so the retry sends it.
    f.refuse_notes.store(true, Ordering::SeqCst);
    let res = send(&app, hand(&row, &["owner:u-teo"])).await;
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    f.refuse_notes.store(false, Ordering::SeqCst);
    let v = json_body(send(&app, hand(&row, &["owner:u-teo"])).await).await;
    assert!(v["candidate"]["sent_to_recruitly"].is_string());
    let teo_notes: Vec<Value> = f
        .posted
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.0 == "/api/nova/journal/rc-teo")
        .map(|p| p.1.clone())
        .collect();
    assert_eq!(teo_notes.len(), 2, "refused once, then stored once");
    assert_eq!(
        f.posted("/api/nova/candidates").len(),
        1,
        "no second record"
    );
    assert_eq!(
        f.posted("/api/nova/jobs/j1/pipeline")[1],
        json!({"candidateId": "rc-teo"})
    );

    // Person 2 shares a name with someone in Recruitly: ask before making a second record.
    f.candidates
        .lock()
        .unwrap()
        .push(json!({"id": "rc-namesake", "firstName": "Person", "lastName": "2"}));
    let row2 = json_body(send(&app, decide_req(&me, &people[2], "shortlist", None)).await).await;
    let v = json_body(send(&app, hand(&row2, &[])).await).await;
    assert!(v["confirm"].as_str().unwrap().contains("same name"));
    assert_eq!(f.posted("/api/nova/candidates").len(), 1, "no record made");
    // An earlier try made a record but its answer was lost: ask before another.
    sqlx::query(
        "INSERT INTO recruitly_handover (candidacy_id, org_id, create_attempted, claimed_at)
         VALUES ($1, $2, true, 'epoch')",
    )
    .bind(Uuid::parse_str(row2["id"].as_str().unwrap()).unwrap())
    .bind(org)
    .execute(&pool)
    .await
    .unwrap();
    let v = json_body(send(&app, hand(&row2, &["namesake"])).await).await;
    assert_eq!(v["confirm_key"], "again");
    let v = json_body(send(&app, hand(&row2, &["namesake", "again"])).await).await;
    assert!(v["candidate"]["sent_to_recruitly"].is_string());
    assert_eq!(f.posted("/api/nova/candidates").len(), 2);

    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE org_id = $1 AND action = 'candidate.handover'",
    )
    .bind(org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audited, 3);
    assert_eq!(
        f.calls()
            .iter()
            .filter(|c| c.starts_with("GET /api/nova/users/list"))
            .count(),
        1,
        "the user list is read once, then kept"
    );

    // Another organisation cannot send these people.
    let (_, outsider) = signed_in(&pool, testutil::org(&pool).await, "admin").await;
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("/api/candidates/{}/handover", row["id"].as_str().unwrap()),
            &outsider,
            json!({"confirmed": ["again"]}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_daily_cap_stops_calls_and_status_shows_the_count() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let (_, admin) = signed_in(&pool, org, "admin").await;
    let f = Arc::new(FakeRc::default());
    let app = recruitly_app(pool.clone(), &fake_recruitly(f.clone()).await, Some(2));

    let res = send(
        &app,
        json_req("POST", "/api/recruitly/test", &me, json!({})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "admins only");
    let v = json_body(
        send(
            &app,
            json_req("POST", "/api/recruitly/test", &admin, json!({})),
        )
        .await,
    )
    .await;
    assert_eq!(v["connected_as"], "Kai");
    send(&app, get_req("/api/recruitly/jobs", Some(&me))).await;
    let res = send(&app, get_req("/api/recruitly/jobs", Some(&me))).await;
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(f.calls().len(), 2, "the third call was never made");
    let s = json_body(send(&app, get_req("/api/recruitly/status", Some(&me))).await).await;
    assert_eq!(
        (
            s["configured"].as_bool(),
            s["calls_today"].as_i64(),
            s["daily_cap"].as_i64()
        ),
        (Some(true), Some(2), Some(2))
    );
    assert_eq!(s["connected"], true, "known from the calls just made");

    // Without a key nothing is called, and the screens say so.
    let off = plain_app(pool.clone());
    let s = json_body(send(&off, get_req("/api/recruitly/status", Some(&me))).await).await;
    assert_eq!(s["configured"], false);
    assert_eq!(s["connected"], Value::Null);
    let res = send(&off, get_req("/api/recruitly/jobs", Some(&me))).await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}
