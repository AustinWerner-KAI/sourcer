//! Outlook: connecting a mailbox, sending approved emails, and stopping on a reply.
//! Microsoft is a stand-in server; nothing real is sent. The sender works
//! across every organisation, so the sending tests each get their own database.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::extract::{Form, Path as UrlPath, State as Shared};
use chrono::{DateTime, Utc};

use super::*;
use crate::mail::{Mail, MailConfig};
use crate::sending::Sender;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SendMode {
    Ok,
    /// Sends, but the answer is lost.
    LostAnswer,
    /// Fails without sending.
    Fail,
    /// Outlook refuses the email (400) and does not send it.
    Refuse,
}

#[derive(Clone)]
struct Msg {
    to: Vec<String>,
    cc: usize,
    subject: String,
    html: String,
    conv: String,
    draft: bool,
    reply_to: Option<String>,
    folder: &'static str,
}

struct Fake {
    me: String,
    next: usize,
    msgs: HashMap<String, Msg>,
    sent: Vec<String>,
    inbox: Vec<Value>,
    send_mode: SendMode,
    revoked: bool,
    /// How long a send takes, to test what happens meanwhile.
    send_ms: u64,
}

type FakeState = Arc<Mutex<Fake>>;

async fn token(
    Shared(f): Shared<FakeState>,
    Form(form): Form<HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let f = f.lock().unwrap();
    assert!(form["scope"].contains("Mail.Send") && form["scope"].contains("offline_access"));
    if form["grant_type"] == "refresh_token" && f.revoked {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant"})),
        )
            .into_response();
    }
    Json(json!({"access_token": "at", "refresh_token": "rt-next", "expires_in": 3600}))
        .into_response()
}

fn add(f: &mut Fake, m: Msg) -> Value {
    f.next += 1;
    let id = format!("m{}", f.next);
    let v = json!({"id": id, "conversationId": m.conv, "internetMessageId": format!("<{id}@x>")});
    f.msgs.insert(id, m);
    v
}

async fn create(Shared(f): Shared<FakeState>, Json(b): Json<Value>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut f = f.lock().unwrap();
    let to: String = b["toRecipients"][0]["emailAddress"]["address"]
        .as_str()
        .unwrap()
        .into();
    // Outlook refuses an address it cannot use.
    if to.starts_with("bad") {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": {"code": "ErrorInvalidRecipients"}})),
        )
            .into_response();
    }
    let conv = format!("conv-{}", f.next + 1);
    let m = Msg {
        to: vec![to],
        cc: 0,
        subject: b["subject"].as_str().unwrap().into(),
        html: b["body"]["content"].as_str().unwrap().into(),
        conv,
        draft: true,
        reply_to: None,
        folder: "drafts-id",
    };
    Json(add(&mut f, m)).into_response()
}

async fn create_reply(Shared(f): Shared<FakeState>, UrlPath(id): UrlPath<String>) -> Json<Value> {
    let mut f = f.lock().unwrap();
    let original = f.msgs[&id].clone();
    // Outlook addresses a reply to your own email back to you.
    let me = f.me.clone();
    let m = Msg {
        to: vec![me],
        cc: 1,
        subject: format!("RE: {}", original.subject),
        html: "quoted".into(),
        conv: original.conv,
        draft: true,
        reply_to: Some(id),
        folder: "drafts-id",
    };
    Json(add(&mut f, m))
}

async fn patch(
    Shared(f): Shared<FakeState>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<Value>,
) -> Json<Value> {
    let mut f = f.lock().unwrap();
    let m = f.msgs.get_mut(&id).unwrap();
    m.to = vec![b["toRecipients"][0]["emailAddress"]["address"]
        .as_str()
        .unwrap()
        .into()];
    m.cc = b["ccRecipients"].as_array().unwrap().len();
    m.subject = b["subject"].as_str().unwrap().into();
    m.html = b["body"]["content"].as_str().unwrap().into();
    Json(json!({"id": id}))
}

async fn send_msg(
    Shared(f): Shared<FakeState>,
    UrlPath(id): UrlPath<String>,
) -> axum::http::StatusCode {
    let wait = f.lock().unwrap().send_ms;
    tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
    let mut f = f.lock().unwrap();
    if f.send_mode == SendMode::Fail {
        return axum::http::StatusCode::INTERNAL_SERVER_ERROR;
    }
    if f.send_mode == SendMode::Refuse {
        return axum::http::StatusCode::BAD_REQUEST;
    }
    let m = f.msgs.get_mut(&id).unwrap();
    assert!(m.draft, "sent twice");
    m.draft = false;
    m.folder = "sent-id";
    f.sent.push(id);
    if f.send_mode == SendMode::LostAnswer {
        return axum::http::StatusCode::INTERNAL_SERVER_ERROR;
    }
    axum::http::StatusCode::ACCEPTED
}

async fn get_msg(
    Shared(f): Shared<FakeState>,
    UrlPath(id): UrlPath<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let f = f.lock().unwrap();
    match f.msgs.get(&id) {
        Some(m) => Json(json!({"isDraft": m.draft, "parentFolderId": m.folder})).into_response(),
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

async fn delete_msg(
    Shared(f): Shared<FakeState>,
    UrlPath(id): UrlPath<String>,
) -> axum::http::StatusCode {
    f.lock().unwrap().msgs.remove(&id);
    axum::http::StatusCode::NO_CONTENT
}

async fn list(Shared(f): Shared<FakeState>) -> Json<Value> {
    Json(json!({"value": f.lock().unwrap().inbox.clone()}))
}

async fn graph_me(Shared(f): Shared<FakeState>) -> Json<Value> {
    Json(json!({"mail": f.lock().unwrap().me.clone()}))
}

async fn fake_outlook(me: &str) -> (String, FakeState) {
    let state: FakeState = Arc::new(Mutex::new(Fake {
        me: me.into(),
        next: 0,
        msgs: HashMap::new(),
        sent: Vec::new(),
        inbox: Vec::new(),
        send_mode: SendMode::Ok,
        revoked: false,
        send_ms: 0,
    }));
    let app = Router::new()
        .route("/tenant/oauth2/v2.0/token", post(token))
        .route("/v1.0/me", get(graph_me))
        .route("/v1.0/me/messages", post(create).get(list))
        .route(
            "/v1.0/me/messages/:id",
            get(get_msg).patch(patch).delete(delete_msg),
        )
        .route("/v1.0/me/messages/:id/createReply", post(create_reply))
        .route("/v1.0/me/messages/:id/send", post(send_msg))
        .route(
            "/v1.0/me/mailFolders/drafts",
            get(|| async { Json(json!({"id": "drafts-id"})) }),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, state)
}

fn mail_config(base: &str) -> MailConfig {
    let mut auth = AuthConfig::new(
        "tenant".into(),
        "client".into(),
        "secret".into(),
        "http://localhost:8080",
        None,
    );
    auth.login_base = base.into();
    auth.graph_base = base.into();
    use base64::Engine;
    let key = base64::engine::general_purpose::STANDARD.encode([9u8; 32]);
    MailConfig::new(&auth, "http://localhost:8080", &key).unwrap()
}

fn mail_app(pool: PgPool, base: &str) -> (Router, Arc<Mail>) {
    let mail = Arc::new(Mail::new(Some(mail_config(base))));
    let mut state = AppState::new(Some(pool), None);
    state.mail = mail.clone();
    (router(state), mail)
}

/// A sender with a connected mailbox.
async fn connected(pool: &PgPool, mail: &Mail, org: Uuid) -> (Uuid, String, String) {
    let (id, cookie) = signed_in(pool, org, "resourcer").await;
    let email: String = sqlx::query_scalar(
        "UPDATE app_user SET signature = 'Regards\nKai', intro = 'I''m at Austin Werner'
         WHERE id = $1 RETURNING email",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO mailbox (user_id, org_id, address, refresh_token, checked_at)
         VALUES ($1, $2, $3, $4, now())",
    )
    .bind(id)
    .bind(org)
    .bind(&email)
    .bind(mail.cfg.as_ref().unwrap().cipher.seal("rt-1"))
    .execute(pool)
    .await
    .unwrap();
    (id, cookie, email)
}

/// An approved sequence of three for a new person. Returns (candidacy, outreach).
async fn approved(pool: &PgPool, org: Uuid, sender: Uuid, to: &str) -> (Uuid, Uuid) {
    let existing: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT r.id, b.id FROM role r JOIN brief b ON b.role_id = r.id WHERE r.org_id = $1 LIMIT 1",
    )
    .bind(org)
    .fetch_optional(pool)
    .await
    .unwrap();
    let (role, brief) = match existing {
        Some(x) => x,
        None => testutil::role_with_brief(pool, org).await,
    };
    let candidacy: Uuid = sqlx::query_scalar(
        "WITH p AS (INSERT INTO person (org_id, full_name) VALUES ($1, 'Sam Sample') RETURNING id),
              k AS (INSERT INTO contact (org_id, person_id, kind, value, source)
                    SELECT $1, id, 'personal_email', $4, 'pdl' FROM p)
         INSERT INTO candidacy (org_id, person_id, role_id, brief_id, state)
         SELECT $1, id, $2, $3, 'approved' FROM p RETURNING id",
    )
    .bind(org)
    .bind(role)
    .bind(brief)
    .bind(to)
    .fetch_one(pool)
    .await
    .unwrap();
    let outreach: Uuid = sqlx::query_scalar(
        "INSERT INTO outreach (org_id, candidacy_id, sender_id, to_email, status, approved_by,
                               approved_at, signature)
         VALUES ($1, $2, $3, $4, 'approved', $3, now(), 'Regards\nKai') RETURNING id",
    )
    .bind(org)
    .bind(candidacy)
    .bind(sender)
    .bind(to)
    .fetch_one(pool)
    .await
    .unwrap();
    for (step, delay, subject, body) in [
        (1, 0, "IAM role, Dubai", "Hey Sam,\n\nA role in Dubai."),
        (2, 3, "Re: IAM role, Dubai", "Hey Sam, bumping this."),
        (3, 4, "Re: IAM role, Dubai", "Hey Sam, last note."),
    ] {
        sqlx::query(
            "INSERT INTO outreach_step (org_id, outreach_id, step, delay_days, subject, body)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(org)
        .bind(outreach)
        .bind(step)
        .bind(delay)
        .bind(subject)
        .bind(body)
        .execute(pool)
        .await
        .unwrap();
    }
    (candidacy, outreach)
}

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// Monday 7 Jan 2030, 10:00 in Dubai.
const MONDAY: &str = "2030-01-07T06:00:00Z";

async fn state_of(pool: &PgPool, outreach: Uuid) -> (String, Option<String>, String) {
    sqlx::query_as(
        "SELECT o.status::text, o.stop_reason, c.state::text FROM outreach o
         JOIN candidacy c ON c.id = o.candidacy_id WHERE o.id = $1",
    )
    .bind(outreach)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn connecting_outlook_keeps_the_token_encrypted_and_only_your_own() {
    let Some(pool) = testutil::pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (_, me) = signed_in(&pool, org, "resourcer").await;
    let email: String =
        sqlx::query_scalar("SELECT u.email FROM app_user u JOIN user_session s ON s.user_id = u.id WHERE u.org_id = $1")
            .bind(org)
            .fetch_one(&pool)
            .await
            .unwrap();
    let (base, fake) = fake_outlook(&email).await;
    let (app, _) = mail_app(pool.clone(), &base);

    let start = |cookie: String| {
        let app = app.clone();
        async move {
            let res = send(&app, get_req("/api/mail/connect", Some(&cookie))).await;
            assert_eq!(res.status(), StatusCode::SEE_OTHER);
            let url = reqwest::Url::parse(&location(&res)).unwrap();
            let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
            assert!(q["scope"].contains("Mail.ReadWrite") && q["scope"].contains("Mail.Send"));
            assert_eq!(q["redirect_uri"], "http://localhost:8080/api/mail/callback");
            let c = set_cookie(&res, "sourcer_mail").unwrap();
            assert!(c.contains("Path=/api/mail/callback") && c.contains("HttpOnly"));
            (q["state"].clone(), c.split(';').next().unwrap().to_string())
        }
    };
    let finish = |cookie: String, st: String, browser: String| {
        let app = app.clone();
        async move {
            let req = Request::builder()
                .uri(format!("/api/mail/callback?code=c&state={st}"))
                .header(header::COOKIE, format!("{cookie}; {browser}"))
                .body(Body::empty())
                .unwrap();
            location(&send(&app, req).await)
        }
    };

    // Someone else's mailbox is refused.
    fake.lock().unwrap().me = "someone.else@example.com".into();
    let (st, browser) = start(me.clone()).await;
    assert_eq!(
        finish(me.clone(), st, browser).await,
        "/settings?outlook=other-account"
    );
    // Another person cannot finish your connection.
    fake.lock().unwrap().me = email.clone();
    let (st, browser) = start(me.clone()).await;
    let (_, other) = signed_in(&pool, org, "resourcer").await;
    assert_eq!(
        finish(other, st.clone(), browser.clone()).await,
        "/settings?outlook=expired"
    );
    let none: i64 = sqlx::query_scalar("SELECT count(*) FROM mailbox WHERE org_id = $1")
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(none, 0);

    let (st, browser) = start(me.clone()).await;
    assert_eq!(
        finish(me.clone(), st, browser).await,
        "/settings?outlook=connected"
    );
    let sealed: Vec<u8> = sqlx::query_scalar("SELECT refresh_token FROM mailbox WHERE org_id = $1")
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        !sealed.windows(7).any(|w| w == b"rt-next"),
        "stored encrypted"
    );
    let v = json_body(send(&app, get_req("/api/mail", Some(&me))).await).await;
    assert_eq!(v["connected"], true);
    assert_eq!(v["address"], email.as_str());
    assert_eq!(v["first_emails_per_day"], 25);

    let req = Request::builder()
        .method("DELETE")
        .uri("/api/mail")
        .header(header::COOKIE, &me)
        .header(CHANGE_HEADER, "1")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await.status(), StatusCode::NO_CONTENT);
    let v = json_body(send(&app, get_req("/api/mail", Some(&me))).await).await;
    assert_eq!(v["connected"], false);
}

#[tokio::test]
async fn three_emails_go_in_one_thread_and_a_reply_stops_the_rest() {
    let Some(pool) = testutil::fresh_pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (base, fake) = fake_outlook("").await;
    let (app, mail) = mail_app(pool.clone(), &base);
    let (sender_id, me, email) = connected(&pool, &mail, org).await;
    fake.lock().unwrap().me = email.clone();
    let to = format!("sam-{}@mail.example", Uuid::new_v4());
    let (candidacy, outreach) = approved(&pool, org, sender_id, &to).await;
    let sender = Sender {
        pool: pool.clone(),
        mail: mail.clone(),
    };
    let monday = at(MONDAY);

    sender.tick(monday).await.unwrap();
    {
        let f = fake.lock().unwrap();
        assert_eq!(f.sent.len(), 1);
        let m = &f.msgs[&f.sent[0]];
        assert_eq!(
            (m.to.as_slice(), m.subject.as_str()),
            ([to.clone()].as_slice(), "IAM role, Dubai")
        );
        assert!(m.html.contains("A role in Dubai") && m.html.contains("Kai"));
        assert!(m.html.contains("data provider"), "source line on the first");
    }
    assert_eq!(
        state_of(&pool, outreach).await,
        ("active".into(), None, "contacted".into())
    );
    // Nothing more until the follow-up is due.
    sender
        .tick(monday + chrono::Duration::days(2))
        .await
        .unwrap();
    assert_eq!(fake.lock().unwrap().sent.len(), 1);

    sender
        .tick(monday + chrono::Duration::days(3) + chrono::Duration::minutes(1))
        .await
        .unwrap();
    {
        let f = fake.lock().unwrap();
        assert_eq!(f.sent.len(), 2);
        let m = &f.msgs[&f.sent[1]];
        assert_eq!(
            m.reply_to.as_deref(),
            Some(f.sent[0].as_str()),
            "same thread"
        );
        assert_eq!((m.to.as_slice(), m.cc), ([to.clone()].as_slice(), 0));
        assert_eq!(m.subject, "Re: IAM role, Dubai");
        assert!(
            !m.html.contains("data provider"),
            "source line on the first only"
        );
    }

    // The candidate replies.
    {
        let mut f = fake.lock().unwrap();
        let conv = f.msgs[&f.sent[0]].conv.clone();
        f.inbox.push(
            json!({"from": {"emailAddress": {"address": to.to_uppercase()}},
            "subject": "RE: IAM role, Dubai", "conversationId": conv, "isDraft": false,
            "receivedDateTime": "2030-01-11T07:00:00Z"}),
        );
    }
    sender.tick(at("2030-01-14T06:05:00Z")).await.unwrap();
    assert_eq!(
        fake.lock().unwrap().sent.len(),
        2,
        "no third email after a reply"
    );
    assert_eq!(
        state_of(&pool, outreach).await,
        ("stopped".into(), Some("Replied".into()), "replied".into())
    );
    let audited: Vec<String> = sqlx::query_scalar(
        "SELECT target FROM audit WHERE org_id = $1 AND action IN ('outreach.send', 'outreach.reply') ORDER BY at",
    )
    .bind(org)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(audited.len(), 3);
    assert!(
        audited.iter().all(|t| !t.contains('@')),
        "no address in the audit"
    );

    // On Today, then opted out for good.
    let v = json_body(send(&app, get_req("/api/today", Some(&me))).await).await;
    assert_eq!(v["replies"][0]["kind"], "reply");
    assert_eq!(v["replies"][0]["candidacy_id"], candidacy.to_string());
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("/api/candidates/{candidacy}/opt-out"),
            &me,
            json!({}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let (opted, listed): (bool, bool) = sqlx::query_as(
        "SELECT p.opted_out, EXISTS (SELECT 1 FROM do_not_contact d WHERE d.org_id = $2 AND d.identifier = $3)
         FROM candidacy c JOIN person p ON p.id = c.person_id WHERE c.id = $1",
    )
    .bind(candidacy)
    .bind(org)
    .bind(&to)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(opted && listed);
    let v = json_body(send(&app, get_req("/api/today", Some(&me))).await).await;
    assert_eq!(v["replies"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn every_check_runs_again_before_sending() {
    let Some(pool) = testutil::fresh_pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (base, fake) = fake_outlook("").await;
    let (_, mail) = mail_app(pool.clone(), &base);
    let (sender_id, _, email) = connected(&pool, &mail, org).await;
    fake.lock().unwrap().me = email;
    let sender = Sender {
        pool: pool.clone(),
        mail: mail.clone(),
    };
    let monday = at(MONDAY);

    // Out of hours and paused: nothing goes, nothing stops.
    let to = format!("a-{}@mail.example", Uuid::new_v4());
    let (_, waiting) = approved(&pool, org, sender_id, &to).await;
    sender.tick(at("2030-01-05T06:00:00Z")).await.unwrap(); // Saturday
    sqlx::query("UPDATE org SET sending_paused = true WHERE id = $1")
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();
    sender.tick(monday).await.unwrap();
    assert_eq!(fake.lock().unwrap().sent.len(), 0);
    assert_eq!(state_of(&pool, waiting).await.0, "approved");
    sqlx::query("UPDATE org SET sending_paused = false WHERE id = $1")
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();

    // Put on the do-not-contact list after approval: stopped, never sent.
    sqlx::query(
        "INSERT INTO do_not_contact (org_id, identifier, reason) VALUES ($1, $2, 'opt_out')",
    )
    .bind(org)
    .bind(&to)
    .execute(&pool)
    .await
    .unwrap();
    sender.tick(monday).await.unwrap();
    assert_eq!(fake.lock().unwrap().sent.len(), 0);
    let (status, reason, state) = state_of(&pool, waiting).await;
    assert_eq!(
        (status.as_str(), state.as_str()),
        ("stopped", "shortlisted")
    );
    assert!(reason
        .unwrap()
        .starts_with("Not sent: On the do-not-contact list"));

    // The client named in an email is caught too.
    let to2 = format!("b-{}@mail.example", Uuid::new_v4());
    let (_, named) = approved(&pool, org, sender_id, &to2).await;
    sqlx::query("UPDATE outreach_step SET body = 'A role at Test Client.' WHERE outreach_id = $1 AND step = 1")
        .bind(named)
        .execute(&pool)
        .await
        .unwrap();
    sender.tick(monday).await.unwrap();
    assert_eq!(fake.lock().unwrap().sent.len(), 0);
    assert!(state_of(&pool, named)
        .await
        .1
        .unwrap()
        .contains("names the client"));

    // The daily limit: one first email a day, so the second waits for tomorrow.
    sqlx::query("UPDATE org SET first_emails_per_day = 1 WHERE id = $1")
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();
    let (_, one) = approved(
        &pool,
        org,
        sender_id,
        &format!("c-{}@mail.example", Uuid::new_v4()),
    )
    .await;
    let (_, two) = approved(
        &pool,
        org,
        sender_id,
        &format!("d-{}@mail.example", Uuid::new_v4()),
    )
    .await;
    sender.tick(monday).await.unwrap();
    sender
        .tick(monday + chrono::Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(fake.lock().unwrap().sent.len(), 1);
    assert_eq!(state_of(&pool, one).await.0, "active");
    assert_eq!(state_of(&pool, two).await.0, "approved");
    sender
        .tick(monday + chrono::Duration::days(1))
        .await
        .unwrap();
    assert_eq!(fake.lock().unwrap().sent.len(), 2);
}

#[tokio::test]
async fn a_send_that_stops_halfway_is_checked_never_doubled() {
    let Some(pool) = testutil::fresh_pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (base, fake) = fake_outlook("").await;
    let (_, mail) = mail_app(pool.clone(), &base);
    let (sender_id, _, email) = connected(&pool, &mail, org).await;
    fake.lock().unwrap().me = email;
    let sender = Sender {
        pool: pool.clone(),
        mail: mail.clone(),
    };
    let monday = at(MONDAY);

    // Sent, but the answer was lost: recorded as sent later, not sent again.
    let (_, lost) = approved(
        &pool,
        org,
        sender_id,
        &format!("e-{}@mail.example", Uuid::new_v4()),
    )
    .await;
    fake.lock().unwrap().send_mode = SendMode::LostAnswer;
    sender.tick(monday).await.unwrap();
    fake.lock().unwrap().send_mode = SendMode::Ok;
    assert_eq!(state_of(&pool, lost).await.0, "approved");
    sender
        .tick(monday + chrono::Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(
        fake.lock().unwrap().sent.len(),
        1,
        "nothing else while it is unclear"
    );
    // Five minutes on, Sourcer asks Outlook. The fake panics on a second send.
    sender
        .tick(monday + chrono::Duration::minutes(6))
        .await
        .unwrap();
    assert_eq!(state_of(&pool, lost).await.0, "active");
    assert_eq!(fake.lock().unwrap().sent.len(), 1);

    // Failed without sending: the draft is sent on the next check, once.
    let (_, failed) = approved(
        &pool,
        org,
        sender_id,
        &format!("f-{}@mail.example", Uuid::new_v4()),
    )
    .await;
    fake.lock().unwrap().send_mode = SendMode::Fail;
    sender
        .tick(monday + chrono::Duration::minutes(7))
        .await
        .unwrap();
    fake.lock().unwrap().send_mode = SendMode::Ok;
    assert_eq!(state_of(&pool, failed).await.0, "approved");
    sender
        .tick(monday + chrono::Duration::minutes(13))
        .await
        .unwrap();
    assert_eq!(state_of(&pool, failed).await.0, "active");
    assert_eq!(fake.lock().unwrap().sent.len(), 2);

    // A refused connection marks the mailbox, and nothing is sent.
    fake.lock().unwrap().revoked = true;
    mail.forget(sender_id);
    let (_, held) = approved(
        &pool,
        org,
        sender_id,
        &format!("g-{}@mail.example", Uuid::new_v4()),
    )
    .await;
    sender
        .tick(monday + chrono::Duration::days(1))
        .await
        .unwrap();
    assert_eq!(state_of(&pool, held).await.0, "approved");
    let broken: Option<String> =
        sqlx::query_scalar("SELECT broken FROM mailbox WHERE user_id = $1")
            .bind(sender_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(broken.unwrap().contains("connecting again"));
}

#[tokio::test]
async fn a_refused_email_stops_after_three_tries_and_holds_nothing_up() {
    let Some(pool) = testutil::fresh_pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (base, fake) = fake_outlook("").await;
    let (_, mail) = mail_app(pool.clone(), &base);
    let (sender_id, _, email) = connected(&pool, &mail, org).await;
    fake.lock().unwrap().me = email;
    let sender = Sender {
        pool: pool.clone(),
        mail: mail.clone(),
    };
    let t0 = at(MONDAY);
    let (_, bad) = approved(&pool, org, sender_id, "bad-address@mail.example").await;
    let (_, good) = approved(
        &pool,
        org,
        sender_id,
        &format!("h-{}@mail.example", Uuid::new_v4()),
    )
    .await;
    sqlx::query("UPDATE outreach SET approved_at = now() - interval '1 hour' WHERE id = $1")
        .bind(bad)
        .execute(&pool)
        .await
        .unwrap();

    sender.tick(t0).await.unwrap();
    sender
        .tick(t0 + chrono::Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(state_of(&pool, good).await.0, "active", "not held up");
    assert_eq!(state_of(&pool, bad).await.0, "approved");
    sender
        .tick(t0 + chrono::Duration::minutes(32))
        .await
        .unwrap();
    assert_eq!(
        state_of(&pool, bad).await.0,
        "approved",
        "tried again later"
    );
    sender
        .tick(t0 + chrono::Duration::minutes(95))
        .await
        .unwrap();
    let (status, reason, state) = state_of(&pool, bad).await;
    assert_eq!(
        (status.as_str(), state.as_str()),
        ("stopped", "shortlisted")
    );
    assert!(reason.unwrap().contains("Outlook refused email 1 3 times"));
    assert_eq!(fake.lock().unwrap().sent.len(), 1);
}

#[tokio::test]
async fn a_reply_for_another_role_or_a_sent_email_blocks_more() {
    let Some(pool) = testutil::fresh_pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (base, fake) = fake_outlook("").await;
    let (app, mail) = mail_app(pool.clone(), &base);
    let (sender_id, me, email) = connected(&pool, &mail, org).await;
    fake.lock().unwrap().me = email;
    let sender = Sender {
        pool: pool.clone(),
        mail: mail.clone(),
    };
    let to = format!("i-{}@mail.example", Uuid::new_v4());
    let (candidacy, outreach) = approved(&pool, org, sender_id, &to).await;
    // They answered on another channel or role after approval.
    sqlx::query(
        "INSERT INTO touch (org_id, person_id, channel, direction)
         SELECT org_id, person_id, 'linkedin', 'in' FROM candidacy WHERE id = $1",
    )
    .bind(candidacy)
    .execute(&pool)
    .await
    .unwrap();
    sender.tick(at(MONDAY)).await.unwrap();
    assert_eq!(fake.lock().unwrap().sent.len(), 0);
    assert!(state_of(&pool, outreach)
        .await
        .1
        .unwrap()
        .contains("replied since"));

    // Once an email has gone, the same sequence is never drafted again.
    sqlx::query("UPDATE outreach_step SET sent_at = now() WHERE outreach_id = $1 AND step = 1")
        .bind(outreach)
        .execute(&pool)
        .await
        .unwrap();
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("/api/candidates/{candidacy}/outreach?fresh=true"),
            &me,
            json!({}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let steps: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outreach_step WHERE outreach_id = $1 AND sent_at IS NOT NULL",
    )
    .bind(outreach)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(steps, 1, "the sent record is kept");
}

#[tokio::test]
async fn stop_during_a_send_waits_for_it_and_keeps_the_record_straight() {
    let Some(pool) = testutil::fresh_pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (base, fake) = fake_outlook("").await;
    let (app, mail) = mail_app(pool.clone(), &base);
    let (sender_id, me, email) = connected(&pool, &mail, org).await;
    {
        let mut f = fake.lock().unwrap();
        f.me = email;
        f.send_ms = 800;
    }
    let to = format!("j-{}@mail.example", Uuid::new_v4());
    let (candidacy, outreach) = approved(&pool, org, sender_id, &to).await;
    let sender = Sender {
        pool: pool.clone(),
        mail: mail.clone(),
    };
    let sending = tokio::spawn(async move { sender.tick(at(MONDAY)).await.unwrap() });
    // Stop while the email is on its way.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let version: i32 = sqlx::query_scalar("SELECT version FROM outreach WHERE id = $1")
        .bind(outreach)
        .fetch_one(&pool)
        .await
        .unwrap();
    let stop = send(
        &app,
        json_req(
            "POST",
            &format!("/api/candidates/{candidacy}/outreach/stop"),
            &me,
            json!({"version": version}),
        ),
    )
    .await;
    sending.await.unwrap();
    // The stop waited for the send. Its version was from before the send,
    // so it is refused and the person sees the latest; nothing is lost.
    assert_eq!(stop.status(), StatusCode::CONFLICT);
    assert_eq!(fake.lock().unwrap().sent.len(), 1);
    assert_eq!(
        state_of(&pool, outreach).await,
        ("active".into(), None, "contacted".into())
    );
}

#[tokio::test]
async fn unclear_or_refused_sends_never_go_twice_and_never_block() {
    let Some(pool) = testutil::fresh_pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (base, fake) = fake_outlook("").await;
    let (app, mail) = mail_app(pool.clone(), &base);
    let (sender_id, me, email) = connected(&pool, &mail, org).await;
    fake.lock().unwrap().me = email;
    let sender = Sender {
        pool: pool.clone(),
        mail: mail.clone(),
    };
    let t0 = at(MONDAY);
    let mins = chrono::Duration::minutes;

    // Outlook refuses the send itself: the draft goes, the step waits, and
    // after three refusals the sequence stops. Nothing else is held up.
    let (_, refused) = approved(
        &pool,
        org,
        sender_id,
        &format!("k-{}@mail.example", Uuid::new_v4()),
    )
    .await;
    fake.lock().unwrap().send_mode = SendMode::Refuse;
    sender.tick(t0).await.unwrap();
    assert_eq!(fake.lock().unwrap().msgs.len(), 0, "refused draft removed");
    sender.tick(t0 + mins(31)).await.unwrap();
    sender.tick(t0 + mins(92)).await.unwrap();
    let (status, reason, _) = state_of(&pool, refused).await;
    assert_eq!(status, "stopped");
    assert!(reason.unwrap().contains("refused"));
    fake.lock().unwrap().send_mode = SendMode::Ok;
    let (_, next) = approved(
        &pool,
        org,
        sender_id,
        &format!("l-{}@mail.example", Uuid::new_v4()),
    )
    .await;
    sender.tick(t0 + mins(93)).await.unwrap();
    assert_eq!(state_of(&pool, next).await.0, "active");

    // On its way (not in Drafts, not sent): never sent again, and after an
    // hour a person decides. The person counts as contacted for good.
    let to3 = format!("m-{}@mail.example", Uuid::new_v4());
    let (c3, transit) = approved(&pool, org, sender_id, &to3).await;
    fake.lock().unwrap().send_mode = SendMode::Fail;
    sender.tick(t0 + mins(100)).await.unwrap();
    fake.lock().unwrap().send_mode = SendMode::Ok;
    {
        let mut f = fake.lock().unwrap();
        let id = f.msgs.iter().find(|(_, m)| m.draft).unwrap().0.clone();
        f.msgs.get_mut(&id).unwrap().folder = "outbox-id";
    }
    sender.tick(t0 + mins(110)).await.unwrap();
    assert_eq!(state_of(&pool, transit).await.0, "approved", "waits");
    sender.tick(t0 + mins(170)).await.unwrap();
    let (status, reason, state) = state_of(&pool, transit).await;
    assert_eq!((status.as_str(), state.as_str()), ("stopped", "contacted"));
    assert!(reason.unwrap().contains("Could not confirm"));
    assert_eq!(fake.lock().unwrap().sent.len(), 1, "only the one that went");
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("/api/candidates/{c3}/outreach?fresh=true"),
            &me,
            json!({}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CONFLICT, "never drafted again");
    // If it did go and they answer, the reply still shows.
    fake.lock()
        .unwrap()
        .inbox
        .push(json!({"from": {"emailAddress": {"address": to3}},
        "subject": "Re: IAM role, Dubai", "conversationId": "x", "isDraft": false,
        "receivedDateTime": "2030-01-08T07:00:00Z"}));
    sender
        .check_replies(at("2030-01-08T08:00:00Z"))
        .await
        .unwrap();
    assert_eq!(state_of(&pool, transit).await.2, "replied");
}

#[tokio::test]
async fn a_reply_to_a_stopped_sequence_still_shows() {
    let Some(pool) = testutil::fresh_pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (base, fake) = fake_outlook("").await;
    let (app, mail) = mail_app(pool.clone(), &base);
    let (sender_id, me, email) = connected(&pool, &mail, org).await;
    fake.lock().unwrap().me = email;
    let sender = Sender {
        pool: pool.clone(),
        mail: mail.clone(),
    };
    let to = format!("n-{}@mail.example", Uuid::new_v4());
    let (candidacy, outreach) = approved(&pool, org, sender_id, &to).await;
    sender.tick(at(MONDAY)).await.unwrap();
    // Stopped by hand after the first email.
    let version: i32 = sqlx::query_scalar("SELECT version FROM outreach WHERE id = $1")
        .bind(outreach)
        .fetch_one(&pool)
        .await
        .unwrap();
    let res = send(
        &app,
        json_req(
            "POST",
            &format!("/api/candidates/{candidacy}/outreach/stop"),
            &me,
            json!({"version": version}),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    fake.lock()
        .unwrap()
        .inbox
        .push(json!({"from": {"emailAddress": {"address": to}},
        "subject": "Re: IAM role, Dubai", "conversationId": "other", "isDraft": false,
        "receivedDateTime": "2030-01-08T07:00:00Z"}));
    sender
        .check_replies(at("2030-01-08T08:00:00Z"))
        .await
        .unwrap();
    let (status, reason, state) = state_of(&pool, outreach).await;
    assert_eq!((status.as_str(), state.as_str()), ("stopped", "replied"));
    assert!(
        reason.unwrap().starts_with("Stopped by"),
        "the stop reason is kept"
    );
    let v = json_body(send(&app, get_req("/api/today", Some(&me))).await).await;
    assert_eq!(v["replies"][0]["candidacy_id"], candidacy.to_string());
}

#[tokio::test]
async fn reject_during_a_send_waits_for_it() {
    let Some(pool) = testutil::fresh_pool().await else {
        return;
    };
    let org = testutil::org(&pool).await;
    let (base, fake) = fake_outlook("").await;
    let (app, mail) = mail_app(pool.clone(), &base);
    let (sender_id, me, email) = connected(&pool, &mail, org).await;
    {
        let mut f = fake.lock().unwrap();
        f.me = email;
        f.send_ms = 800;
    }
    let (candidacy, outreach) = approved(
        &pool,
        org,
        sender_id,
        &format!("o-{}@mail.example", Uuid::new_v4()),
    )
    .await;
    let sender = Sender {
        pool: pool.clone(),
        mail: mail.clone(),
    };
    let sending = tokio::spawn(async move { sender.tick(at(MONDAY)).await.unwrap() });
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let row: Value = json!({"id": candidacy.to_string(), "version": sqlx::query_scalar::<_, i32>(
        "SELECT version FROM candidacy WHERE id = $1").bind(candidacy).fetch_one(&pool).await.unwrap()});
    let res = send(&app, decide_req(&me, &row, "reject", Some("FIT"))).await;
    sending.await.unwrap();
    // No deadlock: the send finished and was recorded; the reject, read before
    // the send, finds the person changed and asks for a reload.
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(fake.lock().unwrap().sent.len(), 1);
    assert_eq!(state_of(&pool, outreach).await.2, "contacted");
}
