//! What every request passes through: cross-site writes refused, hardening
//! headers on every response, and the judges' pass.

use axum::{
    routing::{get, post},
    Router,
};
use portcullis_mcp::{edge, trap, visitor};
use std::sync::Arc;

async fn serve() -> String {
    let app = Router::new()
        .route("/console/run", post(|| async { "started" }))
        .route("/mcp", post(|| async { "mcp" }))
        .route("/page", get(|| async { "page" }))
        .merge(trap::routes(trap::Trap::new(Arc::new(|| 1_794_400_000))))
        .layer(axum::middleware::from_fn(edge::harden));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

fn no_redirects() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

#[tokio::test]
async fn another_site_cannot_start_anything() {
    let base = serve().await;
    let host = base.trim_start_matches("http://").to_string();
    let c = reqwest::Client::new();
    let post = |extra: Vec<(&'static str, String)>| {
        let mut r = c.post(format!("{base}/console/run"));
        for (k, v) in extra {
            r = r.header(k, v);
        }
        async move { r.send().await.unwrap().status().as_u16() }
    };
    // A browser says where a request came from; another site is refused.
    assert_eq!(
        post(vec![("sec-fetch-site", "cross-site".into())]).await,
        403
    );
    assert_eq!(
        post(vec![("sec-fetch-site", "same-site".into())]).await,
        403
    );
    assert_eq!(
        post(vec![("origin", "https://evil.example".into())]).await,
        403
    );
    assert_eq!(post(vec![("origin", "null".into())]).await, 403);
    // The page itself, and tools that send neither header, are let through.
    assert_eq!(
        post(vec![
            ("sec-fetch-site", "same-origin".into()),
            ("origin", format!("http://{host}")),
        ])
        .await,
        200
    );
    assert_eq!(post(vec![]).await, 200);
    // MCP clients are programs and are judged by their token, not this.
    let mcp = c
        .post(format!("{base}/mcp"))
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_eq!(mcp.status(), 200);
    // Reading is never refused.
    let page = c
        .get(format!("{base}/page"))
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), 200);
    let h = page.headers();
    assert_eq!(h["x-frame-options"], "DENY");
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["content-security-policy"], "frame-ancestors 'none'");
    assert!(h.contains_key("strict-transport-security"));
}

#[tokio::test]
async fn the_judges_pass_is_a_cookie_only_the_right_key_sets() {
    visitor::set_judge_key("a-long-enough-test-key").unwrap();
    assert!(visitor::set_judge_key("short").is_err());
    let base = serve().await;
    let c = no_redirects();

    let wrong = c
        .get(format!("{base}/judge?key=guess"))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 200);
    assert!(wrong.headers().get("set-cookie").is_none());
    assert!(wrong.text().await.unwrap().starts_with("We see you."));

    let right = c
        .get(format!("{base}/judge?key=a-long-enough-test-key"))
        .send()
        .await
        .unwrap();
    assert_eq!(right.status(), 303);
    assert_eq!(right.headers()["location"], "/console");
    let cookie = right.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(cookie.contains("SameSite=Strict") && cookie.contains("HttpOnly"));
    assert!(
        !cookie.contains("a-long-enough-test-key"),
        "the key is never echoed"
    );

    // A request carrying that cookie is a judge; one carrying anything else is not.
    let pair = cookie.split(';').next().unwrap();
    let mut h = axum::http::HeaderMap::new();
    h.insert("cookie", format!("other=1; {pair}").parse().unwrap());
    assert!(visitor::is_judge(&h));
    assert_eq!(visitor::who(&h).as_deref(), Some(visitor::JUDGE));
    let mut forged = axum::http::HeaderMap::new();
    forged.insert(
        "cookie",
        format!("{}=0000", visitor::JUDGE_COOKIE).parse().unwrap(),
    );
    assert!(!visitor::is_judge(&forged));
}
