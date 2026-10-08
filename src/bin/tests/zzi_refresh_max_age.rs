use crate::common::{
    PASSWORD, USERNAME, decode_claims, get_auth_headers, get_backend_url,
    session_headers_for_client,
};
use chrono::Utc;
use pretty_assertions::assert_eq;
use rauthy_api_types::clients::{
    ClientResponse, ClientSecretResponse, NewClientRequest, UpdateClientRequest,
};
use rauthy_api_types::oidc::{GrantType, JwkKeyPairAlg, TokenRequest};
use rauthy_service::token_set::TokenSet;
use std::error::Error;
use std::time::Duration;
use tokio::time;

mod common;

const ID: &str = "refresh_max_age_test";
const MAX_AGE: i32 = 60;
const REDIRECT_URI: &str = "http://localhost/callback";

fn update_req(refresh_max_age: Option<i32>) -> UpdateClientRequest {
    UpdateClientRequest {
        name: Some("Refresh Max Age Test".to_string()),
        confidential: true,
        redirect_uris: vec![REDIRECT_URI.to_string()],
        post_logout_redirect_uris: None,
        allowed_origins: None,
        enabled: true,
        flows_enabled: vec![
            GrantType::AuthorizationCode,
            GrantType::Password,
            GrantType::RefreshToken,
        ],
        access_token_alg: JwkKeyPairAlg::EdDSA,
        id_token_alg: JwkKeyPairAlg::EdDSA,
        auth_code_lifetime: 60,
        // short, so that the refresh token's `nbf` is in the past right away
        access_token_lifetime: 10,
        scopes: vec!["openid".to_string(), "email".to_string()],
        default_scopes: vec!["openid".to_string()],
        challenges: Some(vec!["S256".to_string()]),
        force_mfa: false,
        client_uri: None,
        contacts: None,
        backchannel_logout_uri: None,
        restrict_group_prefix: None,
        claims: None,
        claims_at_root: false,
        allowed_resources: None,
        default_aud: None,
        refresh_max_age,
        scim: None,
    }
}

async fn password_login(http: &reqwest::Client, secret: &str) -> Result<TokenSet, Box<dyn Error>> {
    let res = http
        .post(format!("{}/oidc/token", get_backend_url()))
        .form(&TokenRequest {
            grant_type: GrantType::Password,
            client_id: Some(ID.to_string()),
            client_secret: Some(secret.to_string()),
            username: Some(USERNAME.to_string()),
            password: Some(PASSWORD.to_string()),
            ..Default::default()
        })
        .send()
        .await?;
    assert_eq!(res.status(), 200);
    Ok(res.json::<TokenSet>().await?)
}

async fn refresh(
    http: &reqwest::Client,
    secret: &str,
    refresh_token: &str,
) -> Result<reqwest::Response, reqwest::Error> {
    http.post(format!("{}/oidc/token", get_backend_url()))
        .form(&TokenRequest {
            grant_type: GrantType::RefreshToken,
            client_id: Some(ID.to_string()),
            client_secret: Some(secret.to_string()),
            refresh_token: Some(refresh_token.to_string()),
            ..Default::default()
        })
        .send()
        .await
}

/// A client with `refresh_max_age` caps every token at `auth_time + refresh_max_age` and
/// rejects a refresh after that with `invalid_grant`, also for refresh tokens that were
/// issued before the max age was set. Waits for the max age (60 s) to pass.
#[tokio::test]
async fn test_refresh_max_age() -> Result<(), Box<dyn Error>> {
    let auth_headers = get_auth_headers().await?;
    let backend_url = get_backend_url();
    let http = reqwest::Client::new();

    let res = http
        .post(format!("{backend_url}/clients"))
        .headers(auth_headers.clone())
        .json(&NewClientRequest {
            id: ID.to_string(),
            secret: None,
            name: Some("Refresh Max Age Test".to_string()),
            confidential: true,
            redirect_uris: vec![REDIRECT_URI.to_string()],
            post_logout_redirect_uris: None,
        })
        .send()
        .await?;
    assert_eq!(res.status(), 200);

    // out of range values are rejected
    let res = http
        .put(format!("{backend_url}/clients/{ID}"))
        .headers(auth_headers.clone())
        .json(&update_req(Some(59)))
        .send()
        .await?;
    assert_eq!(res.status(), 400);

    // start without a max age
    let res = http
        .put(format!("{backend_url}/clients/{ID}"))
        .headers(auth_headers.clone())
        .json(&update_req(None))
        .send()
        .await?;
    assert_eq!(res.status(), 200);

    let res = http
        .post(format!("{backend_url}/clients/{ID}/secret"))
        .headers(auth_headers.clone())
        .send()
        .await?;
    assert_eq!(res.status(), 200);
    let secret = res.json::<ClientSecretResponse>().await?.secret.unwrap();

    // a login from before the max age was set: its refresh token lives for days
    let ts = password_login(&http, &secret).await?;
    let rt_before = ts.refresh_token.expect("a refresh token");
    let rt_claims = decode_claims(&rt_before);
    let until_before = rt_claims["auth_time"].as_i64().unwrap() + i64::from(MAX_AGE);
    assert!(rt_claims["exp"].as_i64().unwrap() > until_before);

    let res = http
        .put(format!("{backend_url}/clients/{ID}"))
        .headers(auth_headers.clone())
        .json(&update_req(Some(MAX_AGE)))
        .send()
        .await?;
    assert_eq!(res.status(), 200);
    let client: ClientResponse = res.json().await?;
    assert_eq!(client.refresh_max_age, Some(MAX_AGE));

    // a new login: no token outlives `auth_time + refresh_max_age`
    let ts = password_login(&http, &secret).await?;
    assert_eq!(ts.expires_in, 10);
    let rt = ts.refresh_token.expect("a refresh token");
    let rt_claims = decode_claims(&rt);
    let auth_time = rt_claims["auth_time"].as_i64().unwrap();
    let until = auth_time + i64::from(MAX_AGE);
    assert_eq!(rt_claims["exp"].as_i64().unwrap(), until);

    // a refresh within the max age works, and keeps the cap
    let res = refresh(&http, &secret, &rt).await?;
    assert_eq!(res.status(), 200);
    let ts = res.json::<TokenSet>().await?;
    assert!(decode_claims(&ts.access_token)["exp"].as_i64().unwrap() <= until);
    let rt_claims = decode_claims(&ts.refresh_token.expect("a refresh token"));
    assert_eq!(rt_claims["auth_time"].as_i64().unwrap(), auth_time);
    assert!(rt_claims["exp"].as_i64().unwrap() <= until);

    // A session-bound refresh token (authorization code flow): a refresh in the last access
    // token lifetime before the max age returns no new refresh token and consumes the old one.
    let (_, ts) = session_headers_for_client(ID, Some(&secret), REDIRECT_URI).await;
    let id_claims = decode_claims(ts.id_token.as_deref().expect("an ID token"));
    assert!(id_claims["sid"].is_string(), "{id_claims}");
    let rt = ts.refresh_token.expect("a refresh token");
    let rt_claims = decode_claims(&rt);
    let until = rt_claims["auth_time"].as_i64().unwrap() + i64::from(MAX_AGE);
    let wait = until - 10 + 2 - Utc::now().timestamp();
    if wait > 0 {
        time::sleep(Duration::from_secs(wait as u64)).await;
    }
    let res = refresh(&http, &secret, &rt).await?;
    assert_eq!(res.status(), 200);
    let ts = res.json::<TokenSet>().await?;
    assert!(ts.refresh_token.is_none());
    assert!(ts.expires_in < 10, "{}", ts.expires_in);
    assert!(decode_claims(&ts.access_token)["exp"].as_i64().unwrap() <= until);
    let res = refresh(&http, &secret, &rt).await?;
    assert!(!res.status().is_success());

    // Once the max age has passed, the refresh token from before is rejected, although its
    // own `exp` is still days away.
    let wait = until_before - Utc::now().timestamp() + 1;
    if wait > 0 {
        time::sleep(Duration::from_secs(wait as u64)).await;
    }
    let res = refresh(&http, &secret, &rt_before).await?;
    assert_eq!(res.status(), 400);
    let body = res.text().await?;
    assert!(body.contains("invalid_grant"), "{body}");

    // and it has been deleted
    let res = refresh(&http, &secret, &rt_before).await?;
    assert!(!res.status().is_success());

    // cleanup
    let res = http
        .delete(format!("{backend_url}/clients/{ID}"))
        .headers(auth_headers)
        .send()
        .await?;
    assert_eq!(res.status(), 200);

    Ok(())
}
