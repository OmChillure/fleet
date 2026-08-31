//! Fail-closed HTTP integration tests for fleet-auth (in-process Axum oneshot).
//! Requires DATABASE_URL — never skips.

mod common;

use std::time::Duration;

use axum::http::{StatusCode, header};
use chrono::{DateTime, Utc};
use fleet_auth::{
    ACCESS_COOKIE, AccessClaims, REFRESH_COOKIE, decode_access_token, issue_access_token,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use common::*;

fn workspace_id(login: &HttpOut) -> String {
    login.body["default_workspace"]["id"]
        .as_str()
        .expect("default_workspace.id")
        .to_string()
}

fn user_id(login: &HttpOut) -> String {
    login.body["user"]["id"]
        .as_str()
        .expect("user.id")
        .to_string()
}

fn listed_workspace_ids(listed: &HttpOut) -> Vec<&str> {
    listed.body["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .filter_map(|w| w["id"].as_str())
        .collect()
}

async fn add_existing_user(
    app: &axum::Router,
    owner_jar: &CookieJar,
    ws: &str,
    email: &str,
    role: &str,
) {
    let added = call(
        app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(owner_jar),
        Some(json!({ "email": email, "role": role })),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    assert_eq!(
        added.body["added_existing_user"], true,
        "expected existing user added as {role}: {}",
        added.body
    );
}

async fn member_ids(app: &axum::Router, jar: &CookieJar, ws: &str) -> Vec<String> {
    let listed = call(
        app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    listed.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|m| m["user_id"].as_str().map(str::to_string))
        .collect()
}

fn member_roles(listed: &HttpOut) -> std::collections::BTreeMap<&str, &str> {
    listed.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|m| Some((m["user_id"].as_str()?, m["role"].as_str()?)))
        .collect()
}

#[tokio::test]
async fn signup_201_no_session_cookies() {
    let app = app().await;
    let email = unique_email();
    let out = signup(&app, &email, TEST_PASSWORD).await;

    assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);
    assert_eq!(
        out.body["message"].as_str(),
        Some("account created — please log in"),
        "{}",
        out.body
    );
    assert_eq!(out.body["default_workspace"]["role"], "owner");
    assert!(
        out.cookie(ACCESS_COOKIE).is_none(),
        "signup must not set {ACCESS_COOKIE}: {:?}",
        out.set_cookies()
    );
    assert!(
        out.cookie(REFRESH_COOKIE).is_none(),
        "signup must not set {REFRESH_COOKIE}: {:?}",
        out.set_cookies()
    );
    let me = call(&app, "GET", "/api/auth/me", None, None).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED, "signup is not a login");
}

#[tokio::test]
async fn signup_rejects_bad_email() {
    let app = app().await;
    let out = signup(&app, "not-an-email", TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    assert_eq!(out.error(), "valid email is required");
}

#[tokio::test]
async fn signup_rejects_short_password() {
    let app = app().await;
    let out = signup(&app, &unique_email(), "short").await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    assert!(
        out.error().contains("8"),
        "short password error should mention 8 characters, got {:?}",
        out.error()
    );

    let exactly_eight = signup(&app, &unique_email(), "12345678").await;
    assert_eq!(
        exactly_eight.status,
        StatusCode::CREATED,
        "password of length 8 must be accepted: {}",
        exactly_eight.body
    );
}

#[tokio::test]
async fn signup_rejects_long_password() {
    let app = app().await;
    let long = "x".repeat(129);
    let out = signup(&app, &unique_email(), &long).await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
}

#[tokio::test]
async fn signup_duplicate_email_409() {
    let app = app().await;
    let email = unique_email();
    let first = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);

    let second = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(second.status, StatusCode::CONFLICT, "{}", second.body);
    assert_eq!(
        second.error(),
        "an account with this email already exists",
        "{}",
        second.body
    );

    let mixed = format!("Auth.DUP.{}@Example.TEST", Uuid::new_v4());
    let first_mixed = signup(&app, &mixed, TEST_PASSWORD).await;
    assert_eq!(
        first_mixed.status,
        StatusCode::CREATED,
        "{}",
        first_mixed.body
    );
    let again_lower = signup(&app, &mixed.to_ascii_lowercase(), TEST_PASSWORD).await;
    assert_eq!(
        again_lower.status,
        StatusCode::CONFLICT,
        "email uniqueness is case-insensitive: {}",
        again_lower.body
    );
}

#[tokio::test]
async fn login_sets_httponly_cookies_and_workspace() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (out, jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_eq!(out.body["message"].as_str(), Some("logged in"));
    assert_eq!(out.body["default_workspace"]["role"], "owner");

    let access = out
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("missing {ACCESS_COOKIE} Set-Cookie"));
    let refresh = out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("missing {REFRESH_COOKIE} Set-Cookie"));
    for c in [&access, &refresh] {
        assert!(c.is_httponly(), "{} should be HttpOnly: {}", c.name, c.raw);
        assert_eq!(c.path(), Some("/"), "{} Path must be /: {}", c.name, c.raw);
        assert!(
            !c.is_secure(),
            "COOKIE_SECURE=false must not set Secure: {}",
            c.raw
        );
    }
    assert!(jar.access().is_some());
    assert!(jar.refresh().is_some());
}

/// Docs §3.4 / fleet-auth README: `fleet_access` is an HMAC JWT (HS256 via
/// `Validation::default`). `wrong_secret_fails` is a tokens.rs unit — it
/// never drives GET /me. `expired_access_jwt_me_is_401` re-encodes a
/// *validly signed* JWT with past exp — a decoder that set
/// `insecure_disable_signature_validation` or accepted `alg=none` would
/// still fail that exp lock. A `require_user` that skipped decode or
/// accepted `alg=none` would treat a flipped payload as A.
#[tokio::test]
async fn tampered_access_jwt_me_is_401() {
    let app = app().await;
    let (a_email, a_jar, _) = signup_and_login(&app).await;
    let (b_email, b_jar, _) = signup_and_login(&app).await;
    assert_ne!(a_email, b_email, "A and B must be distinct accounts");

    let a_access = a_jar
        .access()
        .expect("login must set fleet_access")
        .to_string();
    assert_eq!(
        a_access.split('.').count(),
        3,
        "fleet_access must be a JWT, got {a_access:?}"
    );
    let a_claims = decode_access_token(&test_jwt_secret(), &a_access)
        .unwrap_or_else(|e| panic!("login fleet_access must decode with fixture secret: {e}"));
    assert_eq!(
        a_claims.email, a_email,
        "JWT email must match the logged-in user"
    );
    assert!(
        !a_claims.sub.is_empty() && !a_claims.sid.is_empty(),
        "access JWT must carry sub+sid: {a_claims:?}"
    );

    let b_access = b_jar
        .access()
        .expect("B login must set fleet_access")
        .to_string();
    let b_claims = decode_access_token(&test_jwt_secret(), &b_access)
        .unwrap_or_else(|e| panic!("B fleet_access must decode with fixture secret: {e}"));
    assert_ne!(
        a_claims.sub, b_claims.sub,
        "A and B must have distinct user ids"
    );
    assert_ne!(
        a_claims.sid, b_claims.sid,
        "A and B must have distinct session ids"
    );

    let control_me = call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await;
    assert_eq!(
        control_me.status,
        StatusCode::OK,
        "original cookies must GET /me 200 before tamper cases: {}",
        control_me.body
    );
    assert_eq!(
        control_me.body["user"]["email"].as_str(),
        Some(a_email.as_str()),
        "control /me: {}",
        control_me.body
    );
    let a_ws = listed_workspace_ids(&control_me)
        .first()
        .copied()
        .expect("control /me workspace id")
        .to_string();
    assert!(
        !a_ws.is_empty(),
        "control /me must include workspaces: {}",
        control_me.body
    );

    let [_, a_payload, a_sig] = jwt_three_parts(&a_access);
    let tampered_sig = format!(
        "{}.{}",
        a_access.rsplit_once('.').expect("jwt has signature").0,
        flip_last_char(a_sig)
    );
    assert_ne!(
        tampered_sig, a_access,
        "flipped signature must not equal the live access JWT"
    );
    decode_access_token(&test_jwt_secret(), &tampered_sig)
        .expect_err("flipped signature must not decode with the fixture secret");

    let tampered_payload = {
        let [header, payload, sig] = jwt_three_parts(&a_access);
        format!("{header}.{}.{sig}", flip_middle_char(payload))
    };
    assert_ne!(
        tampered_payload, a_access,
        "flipped payload must not equal the live access JWT"
    );
    decode_access_token(&test_jwt_secret(), &tampered_payload)
        .expect_err("flipped payload must not decode with the fixture secret");

    let none_as_a_empty_sig = alg_none_jwt(a_payload);
    let none_as_a_no_sig = alg_none_jwt_no_signature(a_payload);
    let none_as_b = alg_none_jwt(jwt_three_parts(&b_access)[1]);
    assert!(
        none_as_a_empty_sig.ends_with('.'),
        "alg=none empty signature is header.payload.: {none_as_a_empty_sig}"
    );
    assert_eq!(
        none_as_a_no_sig.split('.').count(),
        2,
        "alg=none with no signature is header.payload: {none_as_a_no_sig}"
    );
    decode_access_token(&test_jwt_secret(), &none_as_a_empty_sig)
        .expect_err("alg=none empty-sig JWT must not decode");
    decode_access_token(&test_jwt_secret(), &none_as_a_no_sig)
        .expect_err("alg=none two-part JWT must not decode");
    decode_access_token(&test_jwt_secret(), &none_as_b)
        .expect_err("alg=none JWT as B must not decode");

    let other_secret = "other-secret-at-least-32-characters-long!";
    assert!(
        other_secret.len() >= 32,
        "attacker secret must be ≥32 chars (same bar as JWT_SECRET)"
    );
    assert_ne!(
        other_secret,
        test_jwt_secret(),
        "attacker secret must differ from the fixture JWT_SECRET"
    );
    let a_uid = Uuid::parse_str(&a_claims.sub).expect("A JWT sub uuid");
    let a_sid = Uuid::parse_str(&a_claims.sid).expect("A JWT sid uuid");
    let wrong_secret = issue_access_token(
        other_secret,
        a_uid,
        &a_claims.email,
        a_sid,
        Duration::from_secs(900),
    )
    .expect("encode AccessClaims with a different ≥32-char secret");
    assert_ne!(
        wrong_secret, a_access,
        "wrong-secret JWT must not equal the live access JWT"
    );
    decode_access_token(&test_jwt_secret(), &wrong_secret)
        .expect_err("wrong-secret JWT must not decode with the fixture secret");

    let leak = [
        a_email.as_str(),
        b_email.as_str(),
        a_claims.sub.as_str(),
        b_claims.sub.as_str(),
        a_ws.as_str(),
    ];

    // 1. Flipped signature / payload middle: Cookie fleet_access=<tampered>.
    assert_tampered_access_jwt_me_401(
        &call(
            &app,
            "GET",
            "/api/auth/me",
            Some(&access_only_jar(&tampered_sig)),
            None,
        )
        .await,
        "GET /me with fleet_access=<flipped last signature char>",
        &leak,
        &a_claims.sub,
        &b_claims.sub,
    );
    assert_tampered_access_jwt_me_401(
        &call(
            &app,
            "GET",
            "/api/auth/me",
            Some(&access_only_jar(&tampered_payload)),
            None,
        )
        .await,
        "GET /me with fleet_access=<flipped payload middle>",
        &leak,
        &a_claims.sub,
        &b_claims.sub,
    );

    // 2. Header alg=none + empty/no signature: same sub/email/sid, and B's sub.
    assert_tampered_access_jwt_me_401(
        &call(
            &app,
            "GET",
            "/api/auth/me",
            Some(&access_only_jar(&none_as_a_empty_sig)),
            None,
        )
        .await,
        "GET /me with fleet_access=<alg=none empty signature, A's claims>",
        &leak,
        &a_claims.sub,
        &b_claims.sub,
    );
    assert_tampered_access_jwt_me_401(
        &call(
            &app,
            "GET",
            "/api/auth/me",
            Some(&access_only_jar(&none_as_a_no_sig)),
            None,
        )
        .await,
        "GET /me with fleet_access=<alg=none no signature, A's claims>",
        &leak,
        &a_claims.sub,
        &b_claims.sub,
    );
    assert_tampered_access_jwt_me_401(
        &call(
            &app,
            "GET",
            "/api/auth/me",
            Some(&access_only_jar(&none_as_b)),
            None,
        )
        .await,
        "GET /me with fleet_access=<alg=none empty signature, B's sub>",
        &leak,
        &a_claims.sub,
        &b_claims.sub,
    );

    // 3. Same claims, different ≥32-char secret (HTTP sibling of wrong_secret_fails).
    assert_tampered_access_jwt_me_401(
        &call(
            &app,
            "GET",
            "/api/auth/me",
            Some(&access_only_jar(&wrong_secret)),
            None,
        )
        .await,
        "GET /me with fleet_access signed by a different ≥32-char secret",
        &leak,
        &a_claims.sub,
        &b_claims.sub,
    );

    // 4. Original cookies still work — this is not "every /me is 401".
    // Keep expired_token_fails / expired_access_jwt_me_is_401 (valid signature,
    // past exp is a different lock).
    let still = call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await;
    assert_eq!(
        still.status,
        StatusCode::OK,
        "original cookies must still GET /me 200: {}",
        still.body
    );
    assert_eq!(
        still.body["user"]["email"].as_str(),
        Some(a_email.as_str()),
        "original /me user: {}",
        still.body
    );
    assert_eq!(
        still.body["user"]["id"].as_str(),
        Some(a_claims.sub.as_str()),
        "original /me must still be A: {}",
        still.body
    );
    assert!(
        listed_workspace_ids(&still).contains(&a_ws.as_str()),
        "original /me must still include workspace {a_ws}: {}",
        still.body
    );
    let still_b = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_eq!(
        still_b.status,
        StatusCode::OK,
        "B's original cookies must still GET /me 200: {}",
        still_b.body
    );
    assert_eq!(
        still_b.body["user"]["id"].as_str(),
        Some(b_claims.sub.as_str()),
        "B's /me must still be B: {}",
        still_b.body
    );
}

fn jwt_three_parts(token: &str) -> [&str; 3] {
    let mut parts = token.split('.');
    let header = parts.next().expect("jwt header");
    let payload = parts.next().expect("jwt payload");
    let sig = parts.next().expect("jwt signature");
    assert!(
        parts.next().is_none() && !header.is_empty() && !payload.is_empty() && !sig.is_empty(),
        "fleet_access must be a 3-segment JWT: {token:?}"
    );
    [header, payload, sig]
}

fn flip_last_char(s: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    let last = chars.last_mut().expect("non-empty jwt segment");
    *last = if *last == 'A' { 'B' } else { 'A' };
    chars.into_iter().collect()
}

fn flip_middle_char(s: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    assert!(!chars.is_empty(), "jwt segment must be non-empty");
    let i = chars.len() / 2;
    chars[i] = if chars[i] == 'A' { 'B' } else { 'A' };
    chars.into_iter().collect()
}

/// RFC 7519 unsecured JWT (`alg=none`) with an empty signature segment.
fn alg_none_jwt(payload: &str) -> String {
    // base64url({"alg":"none","typ":"JWT"}) — in-process unsigned cookie.
    const ALG_NONE_HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
    format!("{ALG_NONE_HEADER}.{payload}.")
}

fn alg_none_jwt_no_signature(payload: &str) -> String {
    const ALG_NONE_HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
    format!("{ALG_NONE_HEADER}.{payload}")
}

fn access_only_jar(token: &str) -> CookieJar {
    let mut jar = CookieJar::default();
    jar.insert(ACCESS_COOKIE, token.to_string());
    assert!(
        jar.refresh().is_none(),
        "tampered /me must send fleet_access only"
    );
    jar
}

fn assert_tampered_access_jwt_me_401(
    out: &HttpOut,
    when: &str,
    leak: &[&str],
    a_sub: &str,
    b_sub: &str,
) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{when} must be 401, not 200: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{when} must not 200 as a decoded session: {}",
        out.body
    );
    let err = out.error().to_ascii_lowercase();
    assert!(
        err.contains("invalid") || err.contains("unauthorized"),
        "{when} must be 401 invalid or unauthorized, got {:?}: {}",
        out.error(),
        out.body
    );
    assert_ne!(
        out.error(),
        "session is no longer valid",
        "{when} must fail JWT decode (not a live-session revoke): {}",
        out.body
    );
    assert_ne!(
        out.error(),
        "not authenticated",
        "{when} presented fleet_access: {}",
        out.body
    );
    assert!(
        !err.contains("reuse") && !err.contains("family"),
        "{when} is not refresh reuse: {:?}",
        out.error()
    );
    assert!(
        out.body.get("user").is_none()
            && out.body.get("workspaces").is_none()
            && out.body.get("default_workspace").is_none(),
        "{when} must not include user/workspaces: {}",
        out.body
    );
    if let Some(id) = out.body["user"]["id"].as_str() {
        assert_ne!(id, a_sub, "{when} must not 200 as A's sub: {}", out.body);
        assert_ne!(id, b_sub, "{when} must not 200 as B's sub: {}", out.body);
    }
    let blob = format!("{} {}", out.body, out.raw);
    for needle in leak {
        assert!(
            !blob.contains(needle),
            "{when} must not leak {needle}: {}",
            out.body
        );
    }
    assert_no_auth_cookies(out, when);
    assert_no_live_auth_set_cookie(out, when);
}

fn sha256_cookie_bytes(cookie: &str) -> Vec<u8> {
    Sha256::digest(cookie.as_bytes()).to_vec()
}

fn hex_encode_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode_strict(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn assert_json_omits_refresh_secrets(out: &HttpOut, cookie: &str, when: &str) {
    assert!(
        !out.raw.contains(cookie),
        "{when} raw body must not contain the fleet_refresh cookie: {}",
        out.raw
    );
    assert!(
        !out.raw.contains("refresh_token_hash"),
        "{when} raw body must not contain refresh_token_hash: {}",
        out.raw
    );
    fn walk(value: &Value, when: &str) {
        match value {
            Value::Object(map) => {
                for key in [
                    "refresh",
                    "fleet_refresh",
                    "refresh_token",
                    "refresh_token_hash",
                ] {
                    assert!(
                        !map.contains_key(key),
                        "{when} JSON must not have a {key} key: {value}"
                    );
                }
                for child in map.values() {
                    walk(child, when);
                }
            }
            Value::Array(items) => {
                for child in items {
                    walk(child, when);
                }
            }
            _ => {}
        }
    }
    walk(&out.body, when);
}

fn assert_hash_is_sha256_of_cookie(stored: &[u8], cookie: &str, when: &str) {
    let expected = sha256_cookie_bytes(cookie);
    assert!(
        !stored.is_empty(),
        "{when} refresh_token_hash must not be empty"
    );
    assert_eq!(
        stored,
        expected.as_slice(),
        "{when} refresh_token_hash must equal SHA-256 of the cookie bytes \
         (not the cookie UTF-8, not hex::decode(cookie)); stored hex={} cookie={cookie}",
        hex_encode_lower(stored)
    );
    assert_ne!(
        stored,
        cookie.as_bytes(),
        "{when} must not store the cookie UTF-8 in refresh_token_hash"
    );
    if let Some(decoded) = hex_decode_strict(cookie) {
        assert_ne!(
            stored,
            decoded.as_slice(),
            "{when} must not store hex::decode(cookie) in refresh_token_hash"
        );
    }
    assert_ne!(
        hex_encode_lower(stored),
        cookie,
        "{when} encode(hex) of refresh_token_hash must not equal the cookie"
    );
}

async fn active_refresh_token_hashes(pool: &fleet_db::PgPool, user_id: Uuid) -> Vec<Vec<u8>> {
    sqlx::query_scalar(
        r#"
        SELECT refresh_token_hash
        FROM auth_sessions
        WHERE user_id = $1 AND revoked_at IS NULL
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .expect("SELECT refresh_token_hash FROM auth_sessions")
}

/// Docs §2 / fleet-auth README: `fleet_refresh` is opaque; only SHA-256 of
/// the cookie bytes is stored in `auth_sessions.refresh_token_hash`.
/// `login_sets_httponly_cookies_and_workspace` / `refresh_rotates_cookies`
/// only look at Set-Cookie — they never SELECT the column. Storing the
/// cookie (or hex-decoding it into the bytea) lets a DB dump replay
/// POST /refresh. Returning `fleet_refresh` in JSON is the same leak as a
/// non-HttpOnly cookie.
///
/// Distinct from `refresh_old_access_jwt_me_is_401` (stolen access JWT
/// after rotation) and `refresh_reuse_revokes_family` (reuse 401 + family
/// revoke). This lock is at-rest hashing + JSON never echoing the cookie.
#[tokio::test]
async fn refresh_token_hashed_at_rest_json_never_echoes() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (login_out, jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);
    assert_eq!(login_out.body["message"].as_str(), Some("logged in"));

    let refresh = login_out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("login must Set-Cookie {REFRESH_COOKIE}"));
    // Control: login still mints an HttpOnly fleet_refresh so this is not a
    // tautology that every session omits a token.
    assert!(
        !refresh.value.is_empty(),
        "login must mint a live {REFRESH_COOKIE}: {}",
        refresh.raw
    );
    assert!(
        refresh.is_httponly(),
        "{REFRESH_COOKIE} should be HttpOnly: {}",
        refresh.raw
    );
    assert_eq!(
        refresh.value.len(),
        64,
        "generate_refresh_token is 32 random bytes hex (64 chars): {}",
        refresh.value
    );
    assert!(
        refresh.value.bytes().all(|b| b.is_ascii_hexdigit()),
        "fleet_refresh must be hex: {}",
        refresh.value
    );
    let old_refresh = refresh.value.clone();
    assert_eq!(
        jar.refresh(),
        Some(old_refresh.as_str()),
        "jar must absorb the login fleet_refresh"
    );

    assert_json_omits_refresh_secrets(&login_out, &old_refresh, "POST /login 200");

    let uid = Uuid::parse_str(&user_id(&login_out)).expect("login user.id uuid");
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "GET /me: {}", me.body);
    assert_json_omits_refresh_secrets(&me, &old_refresh, "GET /me 200");

    let pool = connect_and_migrate().await;
    let active = active_refresh_token_hashes(&pool, uid).await;
    assert_eq!(
        active.len(),
        1,
        "login must leave exactly one live session hash: {} rows",
        active.len()
    );
    assert_hash_is_sha256_of_cookie(&active[0], &old_refresh, "login auth_sessions row");

    let rotated = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(rotated.status, StatusCode::OK, "{}", rotated.body);
    assert_eq!(
        rotated.body["message"].as_str(),
        Some("token refreshed"),
        "{}",
        rotated.body
    );
    let new_refresh = rotated
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("refresh must Set-Cookie {REFRESH_COOKIE}"));
    assert!(
        new_refresh.is_httponly(),
        "rotated {REFRESH_COOKIE} should be HttpOnly: {}",
        new_refresh.raw
    );
    assert_ne!(
        new_refresh.value, old_refresh,
        "refresh must rotate {REFRESH_COOKIE}"
    );
    assert_eq!(
        new_refresh.value.len(),
        64,
        "rotated fleet_refresh must stay 64 hex: {}",
        new_refresh.value
    );
    assert!(
        new_refresh.value.bytes().all(|b| b.is_ascii_hexdigit()),
        "rotated fleet_refresh must be hex: {}",
        new_refresh.value
    );
    assert_json_omits_refresh_secrets(&rotated, &old_refresh, "POST /refresh 200 (old cookie)");
    assert_json_omits_refresh_secrets(
        &rotated,
        &new_refresh.value,
        "POST /refresh 200 (new cookie)",
    );

    let active = active_refresh_token_hashes(&pool, uid).await;
    assert_eq!(
        active.len(),
        1,
        "rotation must leave exactly one live session hash: {} rows",
        active.len()
    );
    assert_hash_is_sha256_of_cookie(&active[0], &new_refresh.value, "rotated auth_sessions row");
    for hash in &active {
        assert_ne!(
            hash.as_slice(),
            old_refresh.as_bytes(),
            "old cookie UTF-8 must not appear in any active refresh_token_hash"
        );
        assert_ne!(
            hex_encode_lower(hash),
            old_refresh,
            "old cookie must not equal encode(hex) of any active refresh_token_hash"
        );
        assert_ne!(
            hash.as_slice(),
            sha256_cookie_bytes(&old_refresh).as_slice(),
            "old cookie SHA-256 must not remain on an active row"
        );
        if let Some(decoded) = hex_decode_strict(&old_refresh) {
            assert_ne!(
                hash.as_slice(),
                decoded.as_slice(),
                "hex::decode(old cookie) must not appear in any active refresh_token_hash"
            );
        }
    }

    let rows: Vec<(Vec<u8>, bool)> = sqlx::query_as(
        r#"
        SELECT refresh_token_hash, revoked_at IS NOT NULL
        FROM auth_sessions
        WHERE user_id = $1
        ORDER BY created_at, id
        "#,
    )
    .bind(uid)
    .fetch_all(&pool)
    .await
    .expect("list user refresh_token_hash rows");
    let old_hash = sha256_cookie_bytes(&old_refresh);
    let old_still_live = rows
        .iter()
        .any(|(hash, revoked)| hash == &old_hash && !*revoked);
    assert!(
        !old_still_live,
        "old cookie hash must be gone or revoked after rotation: {rows:?}"
    );

    let mut new_jar = CookieJar::default();
    rotated.absorb_into(&mut new_jar);
    let me_new = call(&app, "GET", "/api/auth/me", Some(&new_jar), None).await;
    assert_eq!(
        me_new.status,
        StatusCode::OK,
        "rotated cookies must still GET /me 200: {}",
        me_new.body
    );
    assert_json_omits_refresh_secrets(&me_new, &new_refresh.value, "GET /me after refresh");
    assert_json_omits_refresh_secrets(&me_new, &old_refresh, "GET /me after refresh (old cookie)");

    // Keep refresh_reuse_revokes_family: old cookie after rotation is 401 reuse.
    let mut old_only = CookieJar::default();
    old_only.insert(REFRESH_COOKIE, old_refresh);
    assert!(
        old_only.access().is_none(),
        "reuse POST must send fleet_refresh only"
    );
    let reuse = call(&app, "POST", "/api/auth/refresh", Some(&old_only), None).await;
    assert_refresh_reuse_detected(&reuse, "POST /refresh with the pre-refresh fleet_refresh");
}

const FORGED_COOKIE_HOST: &str = "evil.example.test";

fn forged_cookie_host_headers() -> [(&'static str, &'static str); 3] {
    [
        ("Host", FORGED_COOKIE_HOST),
        ("X-Forwarded-Host", FORGED_COOKIE_HOST),
        ("X-Forwarded-Proto", "https"),
    ]
}

fn cookie_domain_attr(raw: &str) -> Option<&str> {
    raw.split(';').find_map(|part| {
        let p = part.trim();
        p.split_once('=')
            .filter(|(k, _)| k.eq_ignore_ascii_case("Domain"))
            .map(|(_, v)| v.trim())
    })
}

fn raw_set_cookie_forbids_forged_domain(raw: &str, when: &str) {
    let domain = cookie_domain_attr(raw).map(|d| d.trim_matches('"').to_ascii_lowercase());
    if let Some(domain) = domain.as_deref() {
        let stripped = domain.trim_start_matches('.');
        assert!(
            stripped != "evil.example.test" && stripped != "example.test",
            "{when} raw Set-Cookie must not include Domain=evil.example.test, \
             Domain=.evil.example.test, or Domain=example.test: {raw}"
        );
    }
    let lower = raw.to_ascii_lowercase();
    for forbidden in [
        "domain=evil.example.test",
        "domain=.evil.example.test",
        "domain=example.test",
    ] {
        if let Some(idx) = lower.find(forbidden) {
            let after = &lower[idx + "domain=".len()..];
            let value = after.split(';').next().unwrap_or(after).trim();
            let value = value.trim_matches('"').trim_start_matches('.');
            assert!(
                value != "evil.example.test" && value != "example.test",
                "{when} raw Set-Cookie must not include {forbidden}: {raw}"
            );
        }
    }
}

/// Login/refresh cookies are host-only (`cookies.rs` never `set_domain`).
/// `Host` / `X-Forwarded-Host` must not mint `Domain=` — a foreign Domain
/// is dropped by the browser (login 200, `/me` 401) or scopes the session
/// onto a sibling attacker host.
fn assert_host_only_auth_set_cookies(out: &HttpOut, when: &str) {
    let cookies = out.set_cookies();
    let names: Vec<&str> = cookies.iter().map(|c| c.name.as_str()).collect();
    assert!(
        cookies
            .iter()
            .all(|c| c.name == ACCESS_COOKIE || c.name == REFRESH_COOKIE),
        "{when} must not Set-Cookie a third name: {names:?}"
    );
    assert_eq!(
        cookies.len(),
        2,
        "{when} must Set-Cookie only {ACCESS_COOKIE} + {REFRESH_COOKIE}: {names:?}"
    );

    let access = out
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("{when} missing {ACCESS_COOKIE} Set-Cookie"));
    let refresh = out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("{when} missing {REFRESH_COOKIE} Set-Cookie"));
    for c in [&access, &refresh] {
        assert!(
            !c.value.is_empty(),
            "{when} {} Set-Cookie value must not be empty: {}",
            c.name,
            c.raw
        );
        assert!(
            c.is_httponly(),
            "{when} {} should be HttpOnly: {}",
            c.name,
            c.raw
        );
        assert_eq!(
            c.path(),
            Some("/"),
            "{when} {} Path must be /: {}",
            c.name,
            c.raw
        );
        assert!(
            cookie_domain_attr(&c.raw).is_none(),
            "{when} {} must be host-only (no Domain=): {}",
            c.name,
            c.raw
        );
        raw_set_cookie_forbids_forged_domain(&c.raw, when);
    }
    for raw in raw_set_cookie_headers(out) {
        raw_set_cookie_forbids_forged_domain(&raw, when);
    }
}

/// Host / X-Forwarded-Host on login and refresh must not set Domain.
/// Control: no-Host login still 200 + HttpOnly Path=/; failed login still
/// omits Set-Cookie; COOKIE_SECURE=false still omits Secure.
#[tokio::test]
async fn login_cookie_domain_host_ignores_host_header() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let out = call_with_headers(
        &app,
        "POST",
        "/api/auth/login",
        None,
        &forged_cookie_host_headers(),
        Some(json!({ "email": email, "password": TEST_PASSWORD })),
    )
    .await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_eq!(out.body["message"].as_str(), Some("logged in"));
    assert_eq!(
        out.body["user"]["email"].as_str(),
        Some(email.as_str()),
        "login user: {}",
        out.body
    );
    assert_host_only_auth_set_cookies(&out, "login with Host + X-Forwarded-Host");

    let mut jar = CookieJar::default();
    out.absorb_into(&mut jar);
    assert!(
        jar.access().is_some() && jar.refresh().is_some(),
        "login must leave a live cookie pair"
    );
    let old_access = jar.access().expect("access").to_string();
    let old_refresh = jar.refresh().expect("refresh").to_string();

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        me.status,
        StatusCode::OK,
        "host-only cookies must authenticate /me without the forged Host: {}",
        me.body
    );
    assert_eq!(
        me.body["user"]["email"].as_str(),
        Some(email.as_str()),
        "/me user: {}",
        me.body
    );

    let rotated = call_with_headers(
        &app,
        "POST",
        "/api/auth/refresh",
        Some(&jar),
        &forged_cookie_host_headers(),
        None,
    )
    .await;
    assert_eq!(rotated.status, StatusCode::OK, "{}", rotated.body);
    assert_eq!(
        rotated.body["message"].as_str(),
        Some("token refreshed"),
        "{}",
        rotated.body
    );
    assert_host_only_auth_set_cookies(&rotated, "refresh with Host + X-Forwarded-Host");
    let new_access = rotated
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("refresh must Set-Cookie {ACCESS_COOKIE}"));
    let new_refresh = rotated
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("refresh must Set-Cookie {REFRESH_COOKIE}"));
    assert_ne!(
        new_access.value, old_access,
        "refresh must rotate {ACCESS_COOKIE}"
    );
    assert_ne!(
        new_refresh.value, old_refresh,
        "refresh must rotate {REFRESH_COOKIE}"
    );

    // Control: no Host still 200 HttpOnly Path=/ and GET /me 200.
    // COOKIE_SECURE=false still omits Secure (do not weaken cookie_secure).
    let (plain, plain_jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(plain.status, StatusCode::OK, "{}", plain.body);
    let plain_access = plain
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("control login missing {ACCESS_COOKIE} Set-Cookie"));
    let plain_refresh = plain
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("control login missing {REFRESH_COOKIE} Set-Cookie"));
    for c in [&plain_access, &plain_refresh] {
        assert!(c.is_httponly(), "{} should be HttpOnly: {}", c.name, c.raw);
        assert_eq!(c.path(), Some("/"), "{} Path must be /: {}", c.name, c.raw);
        assert!(
            !c.is_secure(),
            "COOKIE_SECURE=false must not set Secure: {}",
            c.raw
        );
        assert!(
            cookie_domain_attr(&c.raw).is_none(),
            "no-Host login must stay host-only: {}",
            c.raw
        );
    }
    let plain_me = call(&app, "GET", "/api/auth/me", Some(&plain_jar), None).await;
    assert_eq!(
        plain_me.status,
        StatusCode::OK,
        "no-Host login GET /me: {}",
        plain_me.body
    );
    assert_eq!(
        plain_me.body["user"]["email"].as_str(),
        Some(email.as_str()),
        "no-Host /me user: {}",
        plain_me.body
    );

    // Failed login still omits Set-Cookie (do not weaken failed_login_no_cookie).
    let failed = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({
            "email": email,
            "password": "wrong-password",
        })),
    )
    .await;
    assert_eq!(failed.status, StatusCode::UNAUTHORIZED, "{}", failed.body);
    assert_eq!(failed.error(), "invalid email or password");
    assert_no_live_auth_set_cookie(&failed, "failed login control");
}

/// Login must mint a live session, not Max-Age=0 (delete-cookie).
/// Default harness uses documented 900 / 604800 — never construct access_ttl=0
/// (from_env must refuse ZERO so HTTP never sees it).
#[tokio::test]
async fn jwt_ttl_login_set_cookie_max_age_is_not_zero() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (out, jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_eq!(out.body["message"].as_str(), Some("logged in"));
    assert!(
        out.body.get("user").is_some() && out.body.get("default_workspace").is_some(),
        "login JSON can 200 with user + default_workspace while cookies are deleted: {}",
        out.body
    );

    let access = out
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("missing {ACCESS_COOKIE} Set-Cookie"));
    let refresh = out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("missing {REFRESH_COOKIE} Set-Cookie"));
    for c in [&access, &refresh] {
        assert!(
            !c.value.is_empty(),
            "{} login Set-Cookie value must not be empty: {}",
            c.name,
            c.raw
        );
        let max_age = c.max_age();
        assert!(
            max_age.is_some_and(|n| n > 0),
            "{} login Set-Cookie must not be Max-Age=0 (delete-cookie): {}",
            c.name,
            c.raw
        );
        let lower = c.raw.to_ascii_lowercase();
        assert!(
            !lower.contains("max-age=0"),
            "{} must not delete the session cookie: {}",
            c.name,
            c.raw
        );
    }
    assert_eq!(
        access.max_age(),
        Some(900),
        "documented JWT_ACCESS_TTL_SECS default is 900: {}",
        access.raw
    );
    assert_eq!(
        refresh.max_age(),
        Some(604_800),
        "documented JWT_REFRESH_TTL_SECS default is 604800: {}",
        refresh.raw
    );

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        me.status,
        StatusCode::OK,
        "login cookies must be a live session, not immediately 401: {}",
        me.body
    );
}

fn assert_raw_omits_password_secrets(raw: &str, password: &str, when: &str) {
    assert!(
        !raw.contains(password),
        "{when} raw body must not contain the password string: {raw}"
    );
    assert!(
        !raw.contains("password_hash"),
        "{when} raw body must not contain password_hash: {raw}"
    );
    assert!(
        !raw.contains("$argon2"),
        "{when} raw body must not contain $argon2: {raw}"
    );
}

fn assert_user_omits_password_keys(user: &serde_json::Value, email: &str, when: &str) {
    let obj = user
        .as_object()
        .unwrap_or_else(|| panic!("{when} user must be an object: {user}"));
    let id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{when} user must have id: {user}"));
    Uuid::parse_str(id).unwrap_or_else(|e| panic!("{when} user.id must be a uuid ({e}): {user}"));
    assert_eq!(
        obj.get("email").and_then(|v| v.as_str()),
        Some(email),
        "{when} user.email: {user}"
    );
    assert_eq!(
        obj.get("display_name").and_then(|v| v.as_str()),
        Some("Ada"),
        "{when} user.display_name: {user}"
    );
    assert!(
        !obj.contains_key("password"),
        "{when} user must not have a password key: {user}"
    );
    assert!(
        !obj.contains_key("password_hash"),
        "{when} user must not have a password_hash key: {user}"
    );
}

/// Signup/login/me/refresh/members return UserPublic (id, email, display_name).
/// Cookie tests never inspect the raw body; echoing the submitted password
/// or an argon2 hash is a credential leak.
#[tokio::test]
async fn signup_login_me_members_refresh_password_hash_omitted() {
    let app = app().await;
    let email = unique_email();
    let password = format!("omit-hash-{}-s3cret", Uuid::new_v4());

    let created = signup_with(&app, &email, &password, Some("Ada")).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_raw_omits_password_secrets(&created.raw, &password, "POST /signup 201");
    assert_user_omits_password_keys(&created.body["user"], &email, "POST /signup 201");

    let (login_out, mut jar) = login(&app, &email, &password).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);
    assert_raw_omits_password_secrets(&login_out.raw, &password, "POST /login 200");
    assert_user_omits_password_keys(&login_out.body["user"], &email, "POST /login 200");
    let ws = login_out.body["default_workspace"]["id"]
        .as_str()
        .expect("login default_workspace.id")
        .to_string();
    Uuid::parse_str(&ws).expect("login default_workspace.id uuid");
    assert_eq!(
        login_out.body["default_workspace"]["role"].as_str(),
        Some("owner"),
        "control: login 200 must include default_workspace, not an empty auth body: {}",
        login_out.body
    );

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    assert_raw_omits_password_secrets(&me.raw, &password, "GET /me 200");
    assert_user_omits_password_keys(&me.body["user"], &email, "GET /me 200");

    let refreshed = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.body);
    assert_raw_omits_password_secrets(&refreshed.raw, &password, "POST /refresh 200");
    assert_user_omits_password_keys(&refreshed.body["user"], &email, "POST /refresh 200");
    refreshed.absorb_into(&mut jar);

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_raw_omits_password_secrets(&listed.raw, &password, "GET /members 200");
    let members = listed.body["members"].as_array().expect("members array");
    assert!(
        !members.is_empty(),
        "need at least one member to check password leakage: {}",
        listed.body
    );
    for member in members {
        let obj = member
            .as_object()
            .unwrap_or_else(|| panic!("member object: {member}"));
        assert!(
            !obj.contains_key("password"),
            "member must not have a password key: {member}"
        );
        assert!(
            !obj.contains_key("password_hash"),
            "member must not have a password_hash key: {member}"
        );
    }
    if let Some(you) = listed.body.get("you").and_then(|v| v.as_object()) {
        assert!(
            !you.contains_key("password") && !you.contains_key("password_hash"),
            "members.you must not have password keys: {}",
            listed.body["you"]
        );
    }
}

/// COOKIE_SECURE=true must mint Secure on login and keep it on refresh.
/// Default harness (cookie_secure=false) still must not — so this is not
/// a tautology that every Set-Cookie is Secure.
#[tokio::test]
async fn cookie_secure_true_login_and_refresh_set_secure() {
    let secure = app_with_cookie_policy(true, cookie::SameSite::Lax).await;
    let email = unique_email();
    let created = signup(&secure, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (out, jar) = login(&secure, &email, TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    let access = out
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("missing {ACCESS_COOKIE} Set-Cookie"));
    let refresh = out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("missing {REFRESH_COOKIE} Set-Cookie"));
    for c in [&access, &refresh] {
        assert!(
            c.is_secure(),
            "COOKIE_SECURE=true must set Secure: {}",
            c.raw
        );
        assert!(c.is_httponly(), "{} should be HttpOnly: {}", c.name, c.raw);
        assert_eq!(c.path(), Some("/"), "{} Path must be /: {}", c.name, c.raw);
    }
    let old_access = jar.access().expect("access").to_string();
    let old_refresh = jar.refresh().expect("refresh").to_string();

    let rotated = call(&secure, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(rotated.status, StatusCode::OK, "{}", rotated.body);
    let new_access = rotated
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("refresh must set {ACCESS_COOKIE}"));
    let new_refresh = rotated
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("refresh must set {REFRESH_COOKIE}"));
    assert_ne!(
        new_access.value, old_access,
        "refresh must rotate {ACCESS_COOKIE}"
    );
    assert_ne!(
        new_refresh.value, old_refresh,
        "refresh must rotate {REFRESH_COOKIE}"
    );
    for c in [&new_access, &new_refresh] {
        assert!(
            c.is_secure(),
            "COOKIE_SECURE=true refresh must keep Secure: {}",
            c.raw
        );
        assert!(c.is_httponly(), "{} should be HttpOnly: {}", c.name, c.raw);
        assert_eq!(c.path(), Some("/"), "{} Path must be /: {}", c.name, c.raw);
    }

    // Control: same jwt_secret, cookie_secure=false still omits Secure.
    let insecure = app().await;
    let other = unique_email();
    let created_insecure = signup(&insecure, &other, TEST_PASSWORD).await;
    assert_eq!(
        created_insecure.status,
        StatusCode::CREATED,
        "{}",
        created_insecure.body
    );
    let (insecure_out, _) = login(&insecure, &other, TEST_PASSWORD).await;
    assert_eq!(insecure_out.status, StatusCode::OK, "{}", insecure_out.body);
    let insecure_access = insecure_out
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("missing {ACCESS_COOKIE} Set-Cookie"));
    let insecure_refresh = insecure_out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("missing {REFRESH_COOKIE} Set-Cookie"));
    for c in [&insecure_access, &insecure_refresh] {
        assert!(
            !c.is_secure(),
            "COOKIE_SECURE=false must not set Secure: {}",
            c.raw
        );
        assert!(c.is_httponly(), "{} should be HttpOnly: {}", c.name, c.raw);
        assert_eq!(c.path(), Some("/"), "{} Path must be /: {}", c.name, c.raw);
    }
}

fn assert_login_access_refresh_never_none_without_secure(out: &HttpOut) {
    for name in [ACCESS_COOKIE, REFRESH_COOKIE] {
        if let Some(c) = out.cookie(name) {
            assert!(
                !c.is_samesite_none() || c.is_secure(),
                "must not mint {name} as SameSite=None without Secure: {}",
                c.raw
            );
        }
    }
}

/// In-struct None+insecure must not mint a fleet_access/fleet_refresh pair
/// browsers would drop (SameSite=None without Secure).
#[tokio::test]
async fn samesite_none_without_secure_login_does_not_mint_insecure_none() {
    let app = app_with_cookie_policy(false, cookie::SameSite::None).await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (out, _jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_login_access_refresh_never_none_without_secure(&out);
    let access = out
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("missing {ACCESS_COOKIE} Set-Cookie"));
    let refresh = out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("missing {REFRESH_COOKIE} Set-Cookie"));
    for c in [&access, &refresh] {
        assert!(
            !c.is_samesite_none() || c.is_secure(),
            "must not mint {} as SameSite=None without Secure: {}",
            c.name,
            c.raw
        );
    }
}

#[tokio::test]
async fn samesite_none_with_secure_login_sets_none_and_secure() {
    let app = app_with_cookie_policy(true, cookie::SameSite::None).await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (out, jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    let access = out
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("missing {ACCESS_COOKIE} Set-Cookie"));
    let refresh = out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("missing {REFRESH_COOKIE} Set-Cookie"));
    for c in [&access, &refresh] {
        assert!(
            c.is_samesite_none(),
            "COOKIE_SAMESITE=None + Secure must set SameSite=None: {}",
            c.raw
        );
        assert!(
            c.is_secure(),
            "COOKIE_SAMESITE=None + Secure must set Secure: {}",
            c.raw
        );
        assert!(c.is_httponly(), "{} should be HttpOnly: {}", c.name, c.raw);
    }
    assert!(jar.access().is_some());
    assert!(jar.refresh().is_some());
}

#[tokio::test]
async fn samesite_none_control_lax_insecure_login_does_not_set_secure() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (out, jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    let access = out
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("missing {ACCESS_COOKIE} Set-Cookie"));
    let refresh = out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("missing {REFRESH_COOKIE} Set-Cookie"));
    for c in [&access, &refresh] {
        assert!(
            !c.is_secure(),
            "COOKIE_SECURE=false must not set Secure: {}",
            c.raw
        );
        assert!(
            !c.is_samesite_none(),
            "default Lax must not set SameSite=None: {}",
            c.raw
        );
        assert!(c.is_httponly(), "{} should be HttpOnly: {}", c.name, c.raw);
    }
    assert_login_access_refresh_never_none_without_secure(&out);
    assert!(jar.access().is_some());
    assert!(jar.refresh().is_some());
}

/// Docs §3.3: unknown email and bad password must be the same 401
/// (`invalid email or password`). Must not leak whether the email exists.
/// Neither may Set-Cookie (do not weaken `failed_login_no_cookie`).
#[tokio::test]
async fn login_unknown_and_bad_password_same_message() {
    let app = app().await;
    let known = unique_email();
    let created = signup(&app, &known, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let known_user_id = created.body["user"]["id"]
        .as_str()
        .expect("signup user.id")
        .to_string();
    let known_ws = created.body["default_workspace"]["id"]
        .as_str()
        .expect("signup default_workspace.id")
        .to_string();
    Uuid::parse_str(&known_user_id).expect("signup user.id uuid");
    Uuid::parse_str(&known_ws).expect("signup default_workspace.id uuid");

    let unknown_email = unique_email();
    let unknown = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({
            "email": unknown_email,
            "password": TEST_PASSWORD,
        })),
    )
    .await;
    let bad = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({
            "email": known,
            "password": "wrong-password",
        })),
    )
    .await;

    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED, "{}", unknown.body);
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED, "{}", bad.body);
    assert_eq!(
        unknown.status, bad.status,
        "unknown email and bad password must share the same status"
    );
    assert_eq!(unknown.error(), "invalid email or password");
    assert_eq!(bad.error(), unknown.error());
    assert_eq!(
        unknown.body, bad.body,
        "unknown email and bad password must be indistinguishable: {} vs {}",
        unknown.body, bad.body
    );
    assert_eq!(
        unknown.raw, bad.raw,
        "unknown email and bad password raw bodies must match: {} vs {}",
        unknown.raw, bad.raw
    );

    for (when, out) in [("unknown email", &unknown), ("bad password", &bad)] {
        let leaked = out.raw.to_ascii_lowercase();
        assert!(
            !leaked.contains("unknown")
                && !leaked.contains("not found")
                && !leaked.contains("does not exist")
                && !leaked.contains("no such")
                && !leaked.contains("wrong password")
                && !leaked.contains("bad password")
                && !leaked.contains("invalid password"),
            "{when} login 401 must not leak whether the email exists: {}",
            out.raw
        );
        assert!(
            out.body.get("user").is_none()
                && out.body.get("workspaces").is_none()
                && out.body.get("default_workspace").is_none(),
            "{when} failed login must not include user/workspace: {}",
            out.body
        );
        assert!(
            !out.raw.contains(&known_user_id)
                && !out.raw.contains(&known_ws)
                && !out.raw.contains(&known)
                && !out.raw.contains(&unknown_email),
            "{when} 401 must not include user id, workspace, or email: {}",
            out.raw
        );
        // Same no-live-pair lock as failed_login_no_cookie; this card also
        // forbids any Set-Cookie (including delete-cookie).
        assert_no_live_auth_set_cookie(out, when);
        assert!(
            raw_set_cookie_headers(out).is_empty(),
            "{when} must not Set-Cookie: {:?}",
            raw_set_cookie_headers(out)
        );
    }

    // Control: the known account still logs in with cookies so this is not
    // a tautology that every POST /login is 401 or omits Set-Cookie.
    let (ok, jar) = login(&app, &known, TEST_PASSWORD).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(ok.body["message"].as_str(), Some("logged in"));
    let access = ok
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("control login must Set-Cookie {ACCESS_COOKIE}"));
    let refresh = ok
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("control login must Set-Cookie {REFRESH_COOKIE}"));
    assert!(
        !access.value.is_empty(),
        "control login must mint a non-empty {ACCESS_COOKIE}: {}",
        access.raw
    );
    assert!(
        !refresh.value.is_empty(),
        "control login must mint a non-empty {REFRESH_COOKIE}: {}",
        refresh.raw
    );
    assert!(
        jar.access().is_some() && jar.refresh().is_some(),
        "control login must leave a live cookie pair"
    );

    // Docs §3.3: inactive is a different 401 (account is not active).
    let pool = connect_and_migrate().await;
    let n = sqlx::query("UPDATE users SET status = 'disabled' WHERE email_normalized = $1")
        .bind(known.to_ascii_lowercase())
        .execute(&pool)
        .await
        .expect("deactivate user");
    assert_eq!(n.rows_affected(), 1, "expected one user row updated");

    let (inactive, inactive_jar) = login(&app, &known, TEST_PASSWORD).await;
    assert_eq!(
        inactive.status,
        StatusCode::UNAUTHORIZED,
        "{}",
        inactive.body
    );
    assert_eq!(inactive.error(), "account is not active");
    assert_ne!(
        inactive.error(),
        unknown.error(),
        "inactive must not reuse the unknown/bad-password message"
    );
    assert!(
        inactive_jar.access().is_none() && inactive_jar.refresh().is_none(),
        "inactive login must not absorb a live pair: {:?}",
        inactive.set_cookies()
    );
}

/// Raw oneshot `Set-Cookie` values (not a re-serialized parse). A handler
/// that `create_session_and_tokens` then returned 401 would still mint a
/// live `fleet_access` / `fleet_refresh` pair here.
fn raw_set_cookie_headers(out: &HttpOut) -> Vec<String> {
    out.headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap_or("").to_string())
        .collect()
}

fn set_cookie_pair_value<'a>(raw: &'a str, name: &str) -> Option<&'a str> {
    let pair = raw.split(';').next().unwrap_or(raw);
    pair.split_once('=')
        .and_then(|(k, v)| (k.trim().eq_ignore_ascii_case(name)).then_some(v.trim()))
}

fn assert_no_live_auth_set_cookie(out: &HttpOut, when: &str) {
    let raws = raw_set_cookie_headers(out);
    for raw in &raws {
        for name in [ACCESS_COOKIE, REFRESH_COOKIE] {
            if let Some(value) = set_cookie_pair_value(raw, name) {
                assert!(
                    value.is_empty(),
                    "{when} raw Set-Cookie must not include {name}= with a non-empty value: {raw}"
                );
            }
            let needle = format!("{name}=");
            if let Some(idx) = raw.find(&needle) {
                let after = &raw[idx + needle.len()..];
                let value = after.split(';').next().unwrap_or(after).trim();
                assert!(
                    value.is_empty(),
                    "{when} raw Set-Cookie must not include {name}= with a non-empty value: {raw}"
                );
            }
        }
    }
    for c in out.set_cookies() {
        if c.name == ACCESS_COOKIE || c.name == REFRESH_COOKIE {
            assert!(
                c.value.is_empty(),
                "{when} must not Set-Cookie {} with a non-empty value: {}",
                c.name,
                c.raw
            );
        }
    }
}

/// Failed login 401: same lock as the message card, plus no live pair and
/// `/me` with any Cookie those headers set is `401 not authenticated`.
async fn assert_failed_login_no_cookie(app: &axum::Router, out: &HttpOut, when: &str) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{when} must be 401: {}",
        out.body
    );
    assert!(
        out.body.get("user").is_none()
            && out.body.get("workspaces").is_none()
            && out.body.get("default_workspace").is_none(),
        "{when} must not include user/workspaces/default_workspace: {}",
        out.body
    );
    assert_no_live_auth_set_cookie(out, when);

    let mut jar = CookieJar::default();
    out.absorb_into(&mut jar);
    let me = call(app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        me.status,
        StatusCode::UNAUTHORIZED,
        "{when} GET /me with cookies from the 401 must be 401: {}",
        me.body
    );
    assert_eq!(
        me.error(),
        "not authenticated",
        "{when} GET /me must be not authenticated (no live access cookie), got {:?}: {}",
        me.error(),
        me.body
    );
}

/// Docs §3.3: cookies are set only after `verify_password`. Unknown email
/// and bad password stay the same 401; neither may mint a session pair.
/// Inactive is a different 401 and still must not Set-Cookie. Control login
/// still 200 + HttpOnly cookies so this is not “every login omits cookies”.
#[tokio::test]
async fn failed_login_no_cookie() {
    let app = app().await;
    let known = unique_email();
    let created = signup(&app, &known, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let unknown_email = unique_email();
    let unknown = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({
            "email": unknown_email,
            "password": TEST_PASSWORD,
        })),
    )
    .await;
    let bad = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({
            "email": known,
            "password": "wrong-password",
        })),
    )
    .await;

    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED, "{}", unknown.body);
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED, "{}", bad.body);
    assert_eq!(unknown.error(), "invalid email or password");
    assert_eq!(
        bad.error(),
        unknown.error(),
        "unknown email and bad password must stay the same message"
    );

    let pool = connect_and_migrate().await;
    assert!(
        user_session_rows(&pool, &known).await.is_empty(),
        "bad-password 401 must not insert auth_sessions"
    );
    assert!(
        user_session_rows(&pool, &unknown_email).await.is_empty(),
        "unknown-email 401 must not insert auth_sessions"
    );

    assert_failed_login_no_cookie(&app, &unknown, "unknown email login").await;
    assert_failed_login_no_cookie(&app, &bad, "bad password login").await;

    // Control: the original account still logs in and mints an HttpOnly pair
    // so this is not a tautology that every POST /login omits cookies.
    let (ok, jar) = login(&app, &known, TEST_PASSWORD).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(ok.body["message"].as_str(), Some("logged in"));
    let access = ok
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("control login must Set-Cookie {ACCESS_COOKIE}"));
    let refresh = ok
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("control login must Set-Cookie {REFRESH_COOKIE}"));
    assert!(
        !access.value.is_empty(),
        "control login must mint a non-empty {ACCESS_COOKIE}: {}",
        access.raw
    );
    assert!(
        !refresh.value.is_empty(),
        "control login must mint a non-empty {REFRESH_COOKIE}: {}",
        refresh.raw
    );
    for c in [&access, &refresh] {
        assert!(c.is_httponly(), "{} should be HttpOnly: {}", c.name, c.raw);
    }
    assert!(
        jar.access().is_some() && jar.refresh().is_some(),
        "control login must leave a live cookie pair"
    );
    let after_ok = user_session_rows(&pool, &known).await;
    assert_eq!(
        after_ok.len(),
        1,
        "control login must insert exactly one session: {after_ok:?}"
    );

    // Same deactivate path as inactive_user_after_login_me_and_refresh_401.
    let n = sqlx::query("UPDATE users SET status = 'disabled' WHERE email_normalized = $1")
        .bind(known.to_ascii_lowercase())
        .execute(&pool)
        .await
        .expect("deactivate user");
    assert_eq!(n.rows_affected(), 1, "expected one user row updated");

    let (inactive, inactive_jar) = login(&app, &known, TEST_PASSWORD).await;
    assert_eq!(
        inactive.status,
        StatusCode::UNAUTHORIZED,
        "inactive login: {}",
        inactive.body
    );
    assert_eq!(inactive.error(), "account is not active");
    assert_ne!(
        inactive.error(),
        unknown.error(),
        "inactive must not reuse the unknown/bad-password message"
    );
    assert!(
        inactive.body.get("user").is_none()
            && inactive.body.get("workspaces").is_none()
            && inactive.body.get("default_workspace").is_none(),
        "inactive login must not include user/workspaces/default_workspace: {}",
        inactive.body
    );
    assert_no_live_auth_set_cookie(&inactive, "inactive login");
    assert!(
        inactive_jar.access().is_none() && inactive_jar.refresh().is_none(),
        "inactive login must not absorb a live pair: {:?}",
        inactive.set_cookies()
    );
    let after_inactive = user_session_rows(&pool, &known).await;
    assert_eq!(
        after_inactive.len(),
        after_ok.len(),
        "inactive login must not insert a session: before={after_ok:?} after={after_inactive:?}"
    );

    let mut me_jar = CookieJar::default();
    inactive.absorb_into(&mut me_jar);
    let me = call(&app, "GET", "/api/auth/me", Some(&me_jar), None).await;
    assert_eq!(
        me.status,
        StatusCode::UNAUTHORIZED,
        "GET /me with cookies from inactive login must be 401: {}",
        me.body
    );
    assert_eq!(
        me.error(),
        "not authenticated",
        "inactive login must not mint a live access cookie, got {:?}: {}",
        me.error(),
        me.body
    );
}

#[tokio::test]
async fn concurrent_logins_two_sessions() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (a_out, a) = login(&app, &email, TEST_PASSWORD).await;
    let (b_out, b) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(a_out.status, StatusCode::OK, "{}", a_out.body);
    assert_eq!(b_out.status, StatusCode::OK, "{}", b_out.body);

    let ra = a.refresh().expect("session A refresh");
    let rb = b.refresh().expect("session B refresh");
    assert_ne!(
        ra, rb,
        "concurrent logins must issue distinct refresh cookies"
    );

    let me_a = call(&app, "GET", "/api/auth/me", Some(&a), None).await;
    let me_b = call(&app, "GET", "/api/auth/me", Some(&b), None).await;
    assert_eq!(me_a.status, StatusCode::OK, "{}", me_a.body);
    assert_eq!(me_b.status, StatusCode::OK, "{}", me_b.body);
}

/// Docs §3.3/§3.4: each login is a new session/family. POST /logout
/// revokes by that refresh hash and clears cookies — not revoke_family.
/// Signing out of one browser must not kick a second concurrent login.
#[tokio::test]
async fn logout_other_session_survives() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (a_out, a_jar) = login(&app, &email, TEST_PASSWORD).await;
    let (b_out, mut b_jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(a_out.status, StatusCode::OK, "{}", a_out.body);
    assert_eq!(b_out.status, StatusCode::OK, "{}", b_out.body);

    let a_access = a_jar
        .access()
        .expect("login A must set fleet_access")
        .to_string();
    let a_refresh = a_jar
        .refresh()
        .expect("login A must set fleet_refresh")
        .to_string();
    let b_access = b_jar
        .access()
        .expect("login B must set fleet_access")
        .to_string();
    let b_refresh = b_jar
        .refresh()
        .expect("login B must set fleet_refresh")
        .to_string();
    assert!(!a_access.is_empty() && !a_refresh.is_empty());
    assert!(!b_access.is_empty() && !b_refresh.is_empty());
    assert_ne!(
        a_refresh, b_refresh,
        "concurrent logins must issue distinct fleet_refresh cookies"
    );
    assert_ne!(
        a_access, b_access,
        "concurrent logins must issue distinct fleet_access cookies"
    );
    assert_access_jwt_still_decodes(&a_access);

    let me_a = call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await;
    let me_b = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_eq!(
        me_a.status,
        StatusCode::OK,
        "pre-logout A /me: {}",
        me_a.body
    );
    assert_eq!(
        me_b.status,
        StatusCode::OK,
        "pre-logout B /me: {}",
        me_b.body
    );
    let uid = user_id(&me_a);
    let ws = me_a.body["workspaces"]
        .as_array()
        .expect("A /me workspaces")
        .iter()
        .filter_map(|w| w["id"].as_str())
        .next()
        .expect("A /me must list a workspace")
        .to_string();
    assert_me_still_owner(&me_a, &email, &uid, &ws, "pre-logout A /me");
    assert_me_still_owner(&me_b, &email, &uid, &ws, "pre-logout B /me");
    let a_workspaces = listed_workspace_ids(&me_a);
    let b_workspaces = listed_workspace_ids(&me_b);
    assert_eq!(
        a_workspaces, b_workspaces,
        "A and B must see the same workspaces: {a_workspaces:?} vs {b_workspaces:?}"
    );

    let logout = call(&app, "POST", "/api/auth/logout", Some(&a_jar), None).await;
    assert_eq!(logout.status, StatusCode::OK, "logout A: {}", logout.body);
    assert_eq!(logout.body["message"].as_str(), Some("logged out"));
    let access_clear = logout
        .cookie(ACCESS_COOKIE)
        .expect("logout must send access Set-Cookie");
    let refresh_clear = logout
        .cookie(REFRESH_COOKIE)
        .expect("logout must send refresh Set-Cookie");
    assert_logout_expires_cookie(&access_clear);
    assert_logout_expires_cookie(&refresh_clear);

    // Replay A's pre-logout Cookie header. Do not absorb logout Set-Cookie —
    // that would 401 from a missing cookie even if the session stayed live.
    let mut a_replay = CookieJar::default();
    a_replay.insert(ACCESS_COOKIE, a_access.clone());
    a_replay.insert(REFRESH_COOKIE, a_refresh.clone());
    assert_eq!(
        a_replay.access(),
        Some(a_access.as_str()),
        "replay Cookie must still carry A's pre-logout access JWT"
    );
    assert_eq!(
        a_replay.refresh(),
        Some(a_refresh.as_str()),
        "replay Cookie must still carry A's pre-logout refresh token"
    );
    assert_access_jwt_still_decodes(&a_access);

    let a_me_replay = call(&app, "GET", "/api/auth/me", Some(&a_replay), None).await;
    assert_eq!(
        a_me_replay.status,
        StatusCode::UNAUTHORIZED,
        "A's replayed cookie after logout must be 401: {}",
        a_me_replay.body
    );
    assert_eq!(
        a_me_replay.error(),
        "session is no longer valid",
        "401 body should explain the session is no longer valid: {}",
        a_me_replay.body
    );
    assert_ne!(
        a_me_replay.error(),
        "not authenticated",
        "A's replay must present the cookie; 401 is session revoke, not a missing cookie: {}",
        a_me_replay.body
    );
    assert!(
        a_me_replay.body.get("user").is_none()
            && a_me_replay.body.get("workspaces").is_none()
            && a_me_replay.body.get("default_workspace").is_none(),
        "A's logout replay /me must not include user/workspaces: {}",
        a_me_replay.body
    );

    let a_refresh_after = call(&app, "POST", "/api/auth/refresh", Some(&a_replay), None).await;
    assert_eq!(
        a_refresh_after.status,
        StatusCode::UNAUTHORIZED,
        "A's revoked refresh must not rotate: {}",
        a_refresh_after.body
    );
    assert_no_live_auth_set_cookie(&a_refresh_after, "A's POST /refresh after logout");

    // B is a different family. Hash-scoped logout (and reuse of A's
    // revoked refresh) must not kick B.
    let me_b_after = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_me_still_owner(&me_b_after, &email, &uid, &ws, "B GET /me after A logout");
    assert_eq!(
        listed_workspace_ids(&me_b_after),
        a_workspaces,
        "B must keep the same workspaces after A logout: {}",
        me_b_after.body
    );
    assert_eq!(
        b_jar.refresh(),
        Some(b_refresh.as_str()),
        "B's jar must still carry the pre-logout fleet_refresh (A logout must not mutate B)"
    );

    let b_rotated = call(&app, "POST", "/api/auth/refresh", Some(&b_jar), None).await;
    assert_eq!(
        b_rotated.status,
        StatusCode::OK,
        "B POST /refresh after A logout must still rotate: {}",
        b_rotated.body
    );
    assert_eq!(
        b_rotated.body["message"].as_str(),
        Some("token refreshed"),
        "{}",
        b_rotated.body
    );
    let new_b_access = b_rotated
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("B refresh must Set-Cookie {ACCESS_COOKIE}"));
    let new_b_refresh = b_rotated
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("B refresh must Set-Cookie {REFRESH_COOKIE}"));
    assert_ne!(
        new_b_access.value, b_access,
        "B refresh must rotate {ACCESS_COOKIE}"
    );
    assert_ne!(
        new_b_refresh.value, b_refresh,
        "B refresh must rotate {REFRESH_COOKIE}"
    );
    assert_ne!(
        new_b_refresh.value, a_refresh,
        "B refresh must not reissue A's revoked fleet_refresh"
    );
    for c in [&new_b_access, &new_b_refresh] {
        assert!(
            !c.value.is_empty(),
            "B refresh {} value must not be empty: {}",
            c.name,
            c.raw
        );
        assert!(
            c.max_age().is_some_and(|n| n > 0),
            "B refresh must not expire {} (A logout must not revoke B's family): {}",
            c.name,
            c.raw
        );
        assert!(c.is_httponly(), "{} should be HttpOnly: {}", c.name, c.raw);
    }
    b_rotated.absorb_into(&mut b_jar);
    assert_eq!(
        b_jar.access(),
        Some(new_b_access.value.as_str()),
        "B jar must pick up the rotated access cookie"
    );
    assert_eq!(
        b_jar.refresh(),
        Some(new_b_refresh.value.as_str()),
        "B jar must pick up the rotated refresh cookie"
    );

    let me_b_rotated = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_me_still_owner(
        &me_b_rotated,
        &email,
        &uid,
        &ws,
        "B GET /me with rotated cookies",
    );

    // Control: a third login after A's logout still 200 so this is not a
    // tautology that every session dies.
    let (c_out, c_jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(c_out.status, StatusCode::OK, "login C: {}", c_out.body);
    assert_eq!(c_out.body["message"].as_str(), Some("logged in"));
    let c_refresh = c_out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("login C must Set-Cookie {REFRESH_COOKIE}"));
    assert!(
        !c_refresh.value.is_empty(),
        "login C must mint a live fleet_refresh: {}",
        c_refresh.raw
    );
    assert_ne!(
        c_refresh.value, a_refresh,
        "login C must not reuse A's revoked refresh"
    );
    assert_ne!(
        c_refresh.value.as_str(),
        b_jar.refresh().expect("B still has refresh"),
        "login C must be a distinct family from B"
    );
    assert!(
        c_jar.access().is_some() && c_jar.refresh().is_some(),
        "login C must leave a live cookie pair"
    );
    let me_c = call(&app, "GET", "/api/auth/me", Some(&c_jar), None).await;
    assert_me_still_owner(&me_c, &email, &uid, &ws, "login C GET /me");
}

#[tokio::test]
async fn refresh_rotates_cookies() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;
    let old_access = jar.access().expect("access").to_string();
    let old_refresh = jar.refresh().expect("refresh").to_string();

    let out = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);

    let new_access = out
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("refresh must set {ACCESS_COOKIE}"));
    let new_refresh = out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("refresh must set {REFRESH_COOKIE}"));
    assert_ne!(new_access.value, old_access);
    assert_ne!(new_refresh.value, old_refresh);
}

/// Docs §3.4 / fleet-auth README: refresh revokes the previous session row
/// and inserts a new one in the same family; the new access JWT carries
/// the new sid. `require_user` decodes `fleet_access` then
/// `is_session_active(sid, user_id)`. `refresh_rotates_cookies` only
/// asserts Set-Cookie values differ — it never GET /me with the
/// pre-refresh access JWT. A handler that skipped `is_session_active`
/// (signature/exp only) would keep a stolen pre-refresh access JWT valid
/// until `JWT_ACCESS_TTL_SECS`.
///
/// Distinct from `expired_access_jwt_me_is_401` (past exp),
/// `logout_clears_session_cookie_and_replay_is_401` (logout revoke),
/// `access_jwt_rejected_as_refresh` (swap cookie names), and
/// `refresh_rotation_never_extends_absolute_expires_at`.
#[tokio::test]
async fn refresh_old_access_jwt_me_is_401() {
    let app = app().await;
    let (email, jar, login_out) = signup_and_login(&app).await;
    let uid = user_id(&login_out);
    let ws = workspace_id(&login_out);
    let old_access = jar
        .access()
        .expect("login must set fleet_access")
        .to_string();
    let old_refresh = jar
        .refresh()
        .expect("login must set fleet_refresh")
        .to_string();
    assert_eq!(
        old_access.split('.').count(),
        3,
        "fleet_access must be a JWT, got {old_access:?}"
    );
    assert_ne!(
        old_access, old_refresh,
        "access JWT and opaque refresh must be distinct values"
    );
    let old_claims = decode_access_token(&test_jwt_secret(), &old_access)
        .unwrap_or_else(|e| panic!("login fleet_access must decode with fixture secret: {e}"));
    assert_eq!(
        old_claims.email, email,
        "JWT email must match the logged-in user"
    );
    assert_eq!(old_claims.sub, uid, "JWT sub must be the login user id");
    assert!(
        !old_claims.sid.is_empty(),
        "access JWT must carry sid: {old_claims:?}"
    );
    assert_access_jwt_still_decodes(&old_access);

    // Control: the pre-refresh access JWT still GET /me 200 if used
    // *before* the first refresh so this is not a tautology that every
    // old JWT 401s (list-expired-access-jwt-me-401 owns exp/iat).
    let mut pre_access_only = CookieJar::default();
    pre_access_only.insert(ACCESS_COOKIE, old_access.clone());
    assert!(
        pre_access_only.refresh().is_none(),
        "control /me must send fleet_access only"
    );
    let pre_me = call(&app, "GET", "/api/auth/me", Some(&pre_access_only), None).await;
    assert_eq!(
        pre_me.status,
        StatusCode::OK,
        "pre-refresh access-only GET /me must be 200: {}",
        pre_me.body
    );
    assert_me_still_owner(
        &pre_me,
        &email,
        &uid,
        &ws,
        "pre-refresh access-only GET /me",
    );
    let pre_workspaces = pre_me.body["workspaces"]
        .as_array()
        .expect("pre-refresh /me workspaces");
    assert!(
        !pre_workspaces.is_empty(),
        "pre-refresh /me must include workspaces: {}",
        pre_me.body
    );

    let rotated = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(rotated.status, StatusCode::OK, "{}", rotated.body);
    assert_eq!(
        rotated.body["message"].as_str(),
        Some("token refreshed"),
        "{}",
        rotated.body
    );
    let new_access = rotated
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("refresh must Set-Cookie {ACCESS_COOKIE}"));
    let new_refresh = rotated
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("refresh must Set-Cookie {REFRESH_COOKIE}"));
    assert_ne!(
        new_access.value, old_access,
        "refresh must rotate {ACCESS_COOKIE}"
    );
    assert_ne!(
        new_refresh.value, old_refresh,
        "refresh must rotate {REFRESH_COOKIE}"
    );
    assert!(
        !new_access.value.is_empty() && !new_refresh.value.is_empty(),
        "refresh must mint a live pair: {:?}",
        rotated.set_cookies()
    );
    let new_claims = decode_access_token(&test_jwt_secret(), &new_access.value)
        .unwrap_or_else(|e| panic!("rotated fleet_access must decode: {e}"));
    assert_eq!(
        new_claims.sub, old_claims.sub,
        "rotation stays the same user"
    );
    assert_ne!(
        new_claims.sid, old_claims.sid,
        "rotated access JWT must carry the new session id"
    );
    // Signature + expiry still hold — 401 below is session revoke, not
    // list-expired-access-jwt-me-401.
    assert_access_jwt_still_decodes(&old_access);

    // Stolen pre-refresh access JWT (no new access; refresh optional).
    let mut stolen = CookieJar::default();
    stolen.insert(ACCESS_COOKIE, old_access.clone());
    assert!(
        stolen.refresh().is_none(),
        "stolen /me must not send fleet_refresh"
    );
    assert_eq!(stolen.access(), Some(old_access.as_str()));
    let stolen_me = call(&app, "GET", "/api/auth/me", Some(&stolen), None).await;
    assert_eq!(
        stolen_me.status,
        StatusCode::UNAUTHORIZED,
        "pre-refresh fleet_access after rotation must GET /me 401, not 200: {}",
        stolen_me.body
    );
    assert_ne!(
        stolen_me.status,
        StatusCode::OK,
        "signature/exp-only require_user would keep the stolen JWT valid: {}",
        stolen_me.body
    );
    assert_eq!(
        stolen_me.error(),
        "session is no longer valid",
        "rotated-away sid must fail is_session_active, got {:?}: {}",
        stolen_me.error(),
        stolen_me.body
    );
    assert_ne!(
        stolen_me.error(),
        "invalid or expired token",
        "pre-refresh access is still a live JWT — expiry is list-expired-access-jwt-me-401: {}",
        stolen_me.body
    );
    assert_ne!(
        stolen_me.error(),
        "not authenticated",
        "stolen /me presented fleet_access: {}",
        stolen_me.body
    );
    let stolen_msg = stolen_me.error().to_ascii_lowercase();
    assert!(
        !stolen_msg.contains("reuse") && !stolen_msg.contains("family"),
        "old access on /me is session revoke, not refresh reuse: {:?}",
        stolen_me.error()
    );
    assert!(
        stolen_me.body.get("user").is_none()
            && stolen_me.body.get("workspaces").is_none()
            && stolen_me.body.get("default_workspace").is_none(),
        "stolen /me 401 must not include user/workspaces: {}",
        stolen_me.body
    );
    assert!(
        !stolen_me.raw.contains(&email)
            && !stolen_me.raw.contains(&uid)
            && !stolen_me.raw.contains(&ws),
        "stolen /me 401 must not leak email/user/workspace: {}",
        stolen_me.raw
    );
    assert_no_auth_cookies(&stolen_me, "stolen pre-refresh access GET /me 401");
    assert_no_live_auth_set_cookie(&stolen_me, "stolen pre-refresh access GET /me 401");

    // Refresh cookie is optional — presenting the revoked refresh next to
    // the old access must not authenticate /me or mint a new family.
    let mut stolen_with_old_refresh = CookieJar::default();
    stolen_with_old_refresh.insert(ACCESS_COOKIE, old_access.clone());
    stolen_with_old_refresh.insert(REFRESH_COOKIE, old_refresh.clone());
    let stolen_both = call(
        &app,
        "GET",
        "/api/auth/me",
        Some(&stolen_with_old_refresh),
        None,
    )
    .await;
    assert_eq!(
        stolen_both.status,
        StatusCode::UNAUTHORIZED,
        "old access + old refresh after rotation must still GET /me 401: {}",
        stolen_both.body
    );
    assert_eq!(
        stolen_both.error(),
        "session is no longer valid",
        "refresh cookie must not keep the rotated-away access JWT valid: {}",
        stolen_both.body
    );
    assert!(
        stolen_both.body.get("user").is_none() && stolen_both.body.get("workspaces").is_none(),
        "old-pair /me 401 must not include user/workspaces: {}",
        stolen_both.body
    );
    assert_no_auth_cookies(&stolen_both, "old access+refresh GET /me 401");
    assert_no_live_auth_set_cookie(&stolen_both, "old access+refresh GET /me 401");

    // New pair still authenticates — not a tautology that every /me after
    // refresh is 401. Family is not revoked (that is reuse).
    let mut new_jar = CookieJar::default();
    rotated.absorb_into(&mut new_jar);
    assert_eq!(
        new_jar.access(),
        Some(new_access.value.as_str()),
        "jar must pick up the rotated access cookie"
    );
    assert_eq!(
        new_jar.refresh(),
        Some(new_refresh.value.as_str()),
        "jar must pick up the rotated refresh cookie"
    );
    let me_new = call(&app, "GET", "/api/auth/me", Some(&new_jar), None).await;
    assert_eq!(
        me_new.status,
        StatusCode::OK,
        "GET /me with rotated cookies must be 200: {}",
        me_new.body
    );
    assert_me_still_owner(&me_new, &email, &uid, &ws, "rotated access+refresh GET /me");
    let new_workspaces = me_new.body["workspaces"]
        .as_array()
        .expect("rotated /me workspaces");
    assert!(
        !new_workspaces.is_empty(),
        "rotated /me must include workspaces: {}",
        me_new.body
    );

    let again = call(&app, "POST", "/api/auth/refresh", Some(&new_jar), None).await;
    assert_eq!(
        again.status,
        StatusCode::OK,
        "POST /refresh with the new refresh must still 200 (family is not revoked): {}",
        again.body
    );
    assert_eq!(
        again.body["message"].as_str(),
        Some("token refreshed"),
        "{}",
        again.body
    );
    let again_access = again
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("second refresh must Set-Cookie {ACCESS_COOKIE}"));
    let again_refresh = again
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("second refresh must Set-Cookie {REFRESH_COOKIE}"));
    assert_ne!(
        again_access.value, new_access.value,
        "second refresh must rotate {ACCESS_COOKIE} again"
    );
    assert_ne!(
        again_refresh.value, new_refresh.value,
        "second refresh must rotate {REFRESH_COOKIE} again"
    );
    assert_ne!(
        again_refresh.value, old_refresh,
        "second refresh must not reissue the pre-refresh token"
    );

    // Old refresh after rotation is reuse (keep refresh_reuse_revokes_family;
    // do not weaken).
    let mut old_refresh_jar = CookieJar::default();
    old_refresh_jar.insert(REFRESH_COOKIE, old_refresh.clone());
    assert!(
        old_refresh_jar.access().is_none(),
        "reuse POST must send fleet_refresh only"
    );
    let reuse = call(
        &app,
        "POST",
        "/api/auth/refresh",
        Some(&old_refresh_jar),
        None,
    )
    .await;
    assert_refresh_reuse_detected(&reuse, "POST /refresh with the pre-refresh fleet_refresh");
}

/// Docs §3.4: presenting an already rotated or already revoked refresh is
/// `401 refresh token reuse detected` and revokes the **entire family**.
/// Sibling rows (later rotations) die. A concurrent login is a different
/// family and must survive.
///
/// Distinct from `refresh_old_access_jwt_me_is_401`: after a *successful*
/// rotation the stolen pre-refresh access JWT is 401 but the *new* pair
/// stays live (GET /me 200, POST /refresh 200). Reuse of the old refresh
/// then 401s that new access **and** the new refresh — whole family dead —
/// and must not Set-Cookie a live new family.
#[tokio::test]
async fn refresh_reuse_revokes_family() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (a_out, mut a_jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(a_out.status, StatusCode::OK, "{}", a_out.body);
    let uid = user_id(&a_out);
    let ws = workspace_id(&a_out);
    let login_refresh = a_jar
        .refresh()
        .expect("login A must set fleet_refresh")
        .to_string();

    let pool = connect_and_migrate().await;
    let live = live_session_rows(&pool, &email).await;
    assert_eq!(
        live.len(),
        1,
        "first login must create exactly one live session: {live:?}"
    );
    let (session_a0, family_a) = live[0];

    // Concurrent login is a distinct family (not a sibling). Theft of A
    // must not revoke B.
    let (b_out, b_jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(b_out.status, StatusCode::OK, "{}", b_out.body);
    let live = live_session_rows(&pool, &email).await;
    assert_eq!(
        live.len(),
        2,
        "concurrent logins must both stay live: {live:?}"
    );
    let family_b = live
        .iter()
        .find(|(id, family)| *id != session_a0 && *family != family_a)
        .map(|(_, family)| *family)
        .unwrap_or_else(|| panic!("second login must be a different family: {live:?}"));
    assert_ne!(family_a, family_b);

    async fn rotate_ok(app: &axum::Router, jar: &mut CookieJar) -> (String, String) {
        let old_access = jar.access().map(str::to_string);
        let old_refresh = jar.refresh().map(str::to_string);
        let out = call(app, "POST", "/api/auth/refresh", Some(jar), None).await;
        assert_eq!(out.status, StatusCode::OK, "{}", out.body);
        assert_eq!(
            out.body["message"].as_str(),
            Some("token refreshed"),
            "{}",
            out.body
        );
        let new_access = out
            .cookie(ACCESS_COOKIE)
            .unwrap_or_else(|| panic!("refresh must Set-Cookie {ACCESS_COOKIE}"));
        let new_refresh = out
            .cookie(REFRESH_COOKIE)
            .unwrap_or_else(|| panic!("refresh must Set-Cookie {REFRESH_COOKIE}"));
        assert!(
            !new_access.value.is_empty() && !new_refresh.value.is_empty(),
            "refresh must mint a live pair: {:?}",
            out.set_cookies()
        );
        assert_ne!(
            Some(new_access.value.as_str()),
            old_access.as_deref(),
            "refresh must rotate the access cookie"
        );
        assert_ne!(
            Some(new_refresh.value.as_str()),
            old_refresh.as_deref(),
            "refresh must rotate the refresh cookie"
        );
        out.absorb_into(jar);
        (new_access.value, new_refresh.value)
    }

    // Two rotations → three rows in family A (login + two siblings).
    // Stronger than "refresh once": reuse must kill every sibling, not
    // only the immediately rotated pair.
    let (first_access, first_refresh) = rotate_ok(&app, &mut a_jar).await;
    let live = live_session_rows(&pool, &email).await;
    let live_a: Vec<_> = live
        .iter()
        .filter(|(_, f)| *f == family_a)
        .copied()
        .collect();
    assert_eq!(
        live_a.len(),
        1,
        "rotation must leave exactly one live row in family A: {live:?}"
    );
    assert_ne!(
        live_a[0].0, session_a0,
        "rotation must insert a new session row"
    );
    assert!(
        session_is_revoked(&pool, session_a0).await,
        "rotation must revoke the previous session row"
    );
    let session_a1 = live_a[0].0;

    let (new_access, new_refresh) = rotate_ok(&app, &mut a_jar).await;
    let family_rows = family_session_rows(&pool, family_a).await;
    assert_eq!(
        family_rows.len(),
        3,
        "login + two rotations must be three family rows: {family_rows:?}"
    );
    let live_a: Vec<_> = family_rows
        .iter()
        .filter(|(_, revoked)| !*revoked)
        .copied()
        .collect();
    assert_eq!(
        live_a.len(),
        1,
        "second rotation must leave exactly one live sibling: {family_rows:?}"
    );
    let session_a2 = live_a[0].0;
    assert_ne!(session_a2, session_a0);
    assert_ne!(session_a2, session_a1);
    assert!(
        session_is_revoked(&pool, session_a1).await,
        "second rotation must revoke the first rotated row"
    );
    assert_ne!(
        new_access, first_access,
        "second rotation must mint a new fleet_access"
    );
    assert_ne!(
        new_refresh, first_refresh,
        "second rotation must mint a new fleet_refresh"
    );
    assert_ne!(
        new_refresh, login_refresh,
        "rotated refresh must not equal the pre-refresh token"
    );
    assert_access_jwt_still_decodes(&new_access);

    // Control: after a *successful* rotation the new pair is live. That
    // is list-refresh-old-access-jwt-401 (stolen pre-refresh access 401s,
    // family still live). Reuse below must then kill this pair.
    let mut new_access_only = CookieJar::default();
    new_access_only.insert(ACCESS_COOKIE, new_access.clone());
    assert!(
        new_access_only.refresh().is_none(),
        "control /me must send fleet_access only"
    );
    let me_live = call(&app, "GET", "/api/auth/me", Some(&new_access_only), None).await;
    assert_eq!(
        me_live.status,
        StatusCode::OK,
        "new access after successful rotation must GET /me 200 (family still live): {}",
        me_live.body
    );
    assert_me_still_owner(
        &me_live,
        &email,
        &uid,
        &ws,
        "new access before reuse GET /me",
    );

    // Replay the original (already rotated) refresh — theft detection.
    let mut replay = CookieJar::default();
    replay.insert(REFRESH_COOKIE, login_refresh.clone());
    assert!(
        replay.access().is_none(),
        "reuse POST must send fleet_refresh only"
    );
    let reuse = call(&app, "POST", "/api/auth/refresh", Some(&replay), None).await;
    assert_refresh_reuse_detected(
        &reuse,
        "POST /refresh with the already-rotated (old) fleet_refresh",
    );

    let family_rows = family_session_rows(&pool, family_a).await;
    assert_eq!(
        family_rows.len(),
        3,
        "reuse must not insert another family row: {family_rows:?}"
    );
    assert!(
        family_rows.iter().all(|(_, revoked)| *revoked),
        "reuse must revoke every sibling in the family: {family_rows:?}"
    );
    assert!(
        session_is_revoked(&pool, session_a2).await,
        "the live sibling after the last rotation must die"
    );

    let live = live_session_rows(&pool, &email).await;
    assert_eq!(
        live.len(),
        1,
        "only the other-family login may stay live: {live:?}"
    );
    assert_eq!(live[0].1, family_b, "surviving session must be family B");

    // Whole family dead: the *new* access JWT (still decodes) GET /me 401,
    // and the *new* refresh POST /refresh 401. Distinct from old-access
    // after a successful rotation (family still live).
    assert_access_jwt_still_decodes(&new_access);
    let me_dead = call(&app, "GET", "/api/auth/me", Some(&new_access_only), None).await;
    assert_eq!(
        me_dead.status,
        StatusCode::UNAUTHORIZED,
        "GET /me with the new access after reuse must be 401 (whole family dead): {}",
        me_dead.body
    );
    assert_eq!(
        me_dead.error(),
        "session is no longer valid",
        "family-revoked sid must fail is_session_active, got {:?}: {}",
        me_dead.error(),
        me_dead.body
    );
    assert_ne!(
        me_dead.error(),
        "invalid or expired token",
        "new access is still a live JWT — expiry is list-expired-access-jwt-me-401: {}",
        me_dead.body
    );
    let dead_msg = me_dead.error().to_ascii_lowercase();
    assert!(
        !dead_msg.contains("reuse") && !dead_msg.contains("family"),
        "new-access /me after reuse is session revoke, not the refresh reuse string: {:?}",
        me_dead.error()
    );
    assert!(
        me_dead.body.get("user").is_none()
            && me_dead.body.get("workspaces").is_none()
            && me_dead.body.get("default_workspace").is_none(),
        "family-dead /me 401 must not include user/workspaces: {}",
        me_dead.body
    );
    assert!(
        !me_dead.raw.contains(&email) && !me_dead.raw.contains(&uid) && !me_dead.raw.contains(&ws),
        "family-dead /me 401 must not leak email/user/workspace: {}",
        me_dead.raw
    );
    assert_no_auth_cookies(&me_dead, "GET /me with new access after reuse");
    assert_no_live_auth_set_cookie(&me_dead, "GET /me with new access after reuse");

    let mut new_refresh_only = CookieJar::default();
    new_refresh_only.insert(REFRESH_COOKIE, new_refresh);
    assert!(
        new_refresh_only.access().is_none(),
        "new-refresh POST must send fleet_refresh only"
    );
    let latest = call(
        &app,
        "POST",
        "/api/auth/refresh",
        Some(&new_refresh_only),
        None,
    )
    .await;
    assert_refresh_reuse_detected(
        &latest,
        "POST /refresh with the new refresh after family revoke",
    );

    let mut mid = CookieJar::default();
    mid.insert(REFRESH_COOKIE, first_refresh);
    let mid_reuse = call(&app, "POST", "/api/auth/refresh", Some(&mid), None).await;
    assert_refresh_reuse_detected(
        &mid_reuse,
        "intermediate rotated refresh after family revoke",
    );

    let again = call(&app, "POST", "/api/auth/refresh", Some(&replay), None).await;
    assert_refresh_reuse_detected(&again, "second replay of the stolen refresh");
    assert!(
        live_session_rows(&pool, &email)
            .await
            .iter()
            .all(|(_, f)| *f == family_b),
        "second replay must not revive family A"
    );

    let me_b = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_eq!(
        me_b.status,
        StatusCode::OK,
        "other family must survive reuse of A: {}",
        me_b.body
    );
    assert_me_still_owner(
        &me_b,
        &email,
        &uid,
        &ws,
        "other family GET /me after A's reuse",
    );

    // Already-revoked (logout) refresh is the same 401 reuse signal.
    let b_refresh = b_jar
        .refresh()
        .expect("login B must set fleet_refresh")
        .to_string();
    let logout = call(&app, "POST", "/api/auth/logout", Some(&b_jar), None).await;
    assert_eq!(logout.status, StatusCode::OK, "{}", logout.body);
    let mut revoked_replay = CookieJar::default();
    revoked_replay.insert(REFRESH_COOKIE, b_refresh);
    let revoked = call(
        &app,
        "POST",
        "/api/auth/refresh",
        Some(&revoked_replay),
        None,
    )
    .await;
    assert_refresh_reuse_detected(&revoked, "replay of already-revoked (logout) refresh");
    assert!(
        live_session_rows(&pool, &email).await.is_empty(),
        "logout + reuse must leave no live session for this user"
    );
}

async fn live_session_rows(pool: &fleet_db::PgPool, email: &str) -> Vec<(Uuid, Uuid)> {
    sqlx::query_as(
        r#"
        SELECT s.id, s.family_id
        FROM auth_sessions s
        JOIN users u ON u.id = s.user_id
        WHERE u.email_normalized = $1
          AND s.revoked_at IS NULL
        "#,
    )
    .bind(email.to_ascii_lowercase())
    .fetch_all(pool)
    .await
    .expect("list live auth_sessions")
}

/// `(session id, family_id, revoked)` for every row belonging to `email`.
async fn user_session_rows(pool: &fleet_db::PgPool, email: &str) -> Vec<(Uuid, Uuid, bool)> {
    sqlx::query_as(
        r#"
        SELECT s.id, s.family_id, s.revoked_at IS NOT NULL
        FROM auth_sessions s
        JOIN users u ON u.id = s.user_id
        WHERE u.email_normalized = $1
        ORDER BY s.created_at, s.id
        "#,
    )
    .bind(email.to_ascii_lowercase())
    .fetch_all(pool)
    .await
    .expect("list user auth_sessions")
}

/// `(session id, revoked)` for every row in a rotation family.
async fn family_session_rows(pool: &fleet_db::PgPool, family_id: Uuid) -> Vec<(Uuid, bool)> {
    sqlx::query_as(
        r#"
        SELECT id, revoked_at IS NOT NULL
        FROM auth_sessions
        WHERE family_id = $1
        ORDER BY created_at, id
        "#,
    )
    .bind(family_id)
    .fetch_all(pool)
    .await
    .expect("list family auth_sessions")
}

async fn session_absolute(pool: &fleet_db::PgPool, id: Uuid) -> DateTime<Utc> {
    sqlx::query_scalar("SELECT absolute_expires_at FROM auth_sessions WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("absolute_expires_at")
}

async fn session_expires(pool: &fleet_db::PgPool, id: Uuid) -> DateTime<Utc> {
    sqlx::query_scalar("SELECT expires_at FROM auth_sessions WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("expires_at")
}

async fn session_is_revoked(pool: &fleet_db::PgPool, id: Uuid) -> bool {
    sqlx::query_scalar("SELECT revoked_at IS NOT NULL FROM auth_sessions WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("session revoked_at")
}

async fn family_rows_past_absolute(
    pool: &fleet_db::PgPool,
    family_id: Uuid,
    original: DateTime<Utc>,
) -> i64 {
    sqlx::query_scalar(
        r#"
        SELECT COUNT(*)::bigint
        FROM auth_sessions
        WHERE family_id = $1
          AND absolute_expires_at > $2
        "#,
    )
    .bind(family_id)
    .bind(original)
    .fetch_one(pool)
    .await
    .expect("count family rows past original absolute")
}

fn assert_no_auth_cookies(out: &HttpOut, when: &str) {
    assert!(
        out.cookie(ACCESS_COOKIE).is_none() && out.cookie(REFRESH_COOKIE).is_none(),
        "{when} must not issue new cookies: {:?}",
        out.set_cookies()
    );
}

fn assert_refresh_reuse_detected(out: &HttpOut, when: &str) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{when} must be 401: {}",
        out.body
    );
    assert!(
        out.error().starts_with("refresh token reuse detected"),
        "{when} must be 401 refresh token reuse detected, got {:?}",
        out.error()
    );
    let msg = out.error().to_ascii_lowercase();
    assert!(
        !msg.contains("expired")
            && !msg.contains("already rotated")
            && out.error() != "invalid refresh token"
            && out.error() != "missing refresh token",
        "{when} is reuse, not expiry/hash-miss/race: {:?}",
        out.error()
    );
    assert_no_auth_cookies(out, when);
    assert_no_live_auth_set_cookie(out, when);
    assert!(
        out.body.get("user").is_none()
            && out.body.get("workspaces").is_none()
            && out.body.get("default_workspace").is_none(),
        "{when} must not leak user/workspace: {}",
        out.body
    );
}

/// Docs §3.4: rotate same `family_id`; absolute expiry is never extended.
/// HTTP refresh after `absolute_expires_at` is `401 refresh token expired`
/// and revokes that session. A later refresh with the rotated cookie must
/// not live past the original absolute.
#[tokio::test]
async fn refresh_rotation_never_extends_absolute_expires_at() {
    let app = app().await;
    let (email, jar, _) = signup_and_login(&app).await;
    let pool = connect_and_migrate().await;

    let live = live_session_rows(&pool, &email).await;
    assert_eq!(
        live.len(),
        1,
        "login must create exactly one live session: {live:?}"
    );
    let (login_session_id, family_id) = live[0];

    // Cap absolute well below refresh_ttl (7d in this harness) so an
    // extending rotation would be visible as a later timestamp.
    let original_absolute: DateTime<Utc> = sqlx::query_scalar(
        r#"
        UPDATE auth_sessions
        SET absolute_expires_at = now() + interval '90 minutes'
        WHERE id = $1
        RETURNING absolute_expires_at
        "#,
    )
    .bind(login_session_id)
    .fetch_one(&pool)
    .await
    .expect("cap login absolute_expires_at");
    let horizon: DateTime<Utc> = sqlx::query_scalar("SELECT now() + interval '3 hours'")
        .fetch_one(&pool)
        .await
        .expect("absolute horizon");
    assert!(
        original_absolute < horizon,
        "test cap must stay under the 3h horizon (got {original_absolute} vs {horizon})"
    );
    let login_expires = session_expires(&pool, login_session_id).await;
    assert!(
        login_expires > horizon,
        "login expires_at must still be the long refresh_ttl so a copied-ttl rotation is distinguishable from the 90m cap"
    );

    async fn assert_rotation_keeps_original_absolute(
        pool: &fleet_db::PgPool,
        email: &str,
        previous_session_id: Uuid,
        family_id: Uuid,
        original_absolute: DateTime<Utc>,
        horizon: DateTime<Utc>,
    ) -> Uuid {
        let live = live_session_rows(pool, email).await;
        assert_eq!(
            live.len(),
            1,
            "rotation must leave exactly one live session: {live:?}"
        );
        let (session_id, rotated_family) = live[0];
        assert_ne!(
            session_id, previous_session_id,
            "rotation must insert a new session row"
        );
        assert_eq!(
            rotated_family, family_id,
            "rotation must keep the same family_id"
        );
        assert!(
            session_is_revoked(pool, previous_session_id).await,
            "rotation must revoke the previous session row"
        );
        let absolute = session_absolute(pool, session_id).await;
        assert_eq!(
            absolute, original_absolute,
            "rotation must copy absolute_expires_at, never extend it"
        );
        assert!(
            absolute < horizon,
            "rotated absolute_expires_at must stay under the original cap, not refresh_ttl: {absolute} vs {horizon}"
        );
        let expires = session_expires(pool, session_id).await;
        assert!(
            expires <= original_absolute,
            "rotated expires_at must not live past the original absolute: {expires} vs {original_absolute}"
        );
        assert_eq!(
            family_rows_past_absolute(pool, family_id, original_absolute).await,
            0,
            "no session in the family may have absolute_expires_at later than login"
        );
        session_id
    }

    let first = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(
        first.body["message"].as_str(),
        Some("token refreshed"),
        "{}",
        first.body
    );
    let mut rotated_jar = jar.clone();
    first.absorb_into(&mut rotated_jar);
    assert_ne!(
        rotated_jar.refresh(),
        jar.refresh(),
        "refresh must rotate the refresh cookie"
    );
    let first_session_id = assert_rotation_keeps_original_absolute(
        &pool,
        &email,
        login_session_id,
        family_id,
        original_absolute,
        horizon,
    )
    .await;

    // A later refresh with the rotated cookie must still copy the same
    // hard cap — extending on the second rotation is still a fail.
    let second = call(&app, "POST", "/api/auth/refresh", Some(&rotated_jar), None).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.body);
    assert_eq!(
        second.body["message"].as_str(),
        Some("token refreshed"),
        "{}",
        second.body
    );
    let first_refresh = rotated_jar.refresh().map(str::to_string);
    second.absorb_into(&mut rotated_jar);
    assert_ne!(
        rotated_jar.refresh(),
        first_refresh.as_deref(),
        "second refresh must rotate the refresh cookie again"
    );
    let second_session_id = assert_rotation_keeps_original_absolute(
        &pool,
        &email,
        first_session_id,
        family_id,
        original_absolute,
        horizon,
    )
    .await;

    // Age only the hard cap. Leave expires_at in the future so this is
    // absolute expiry, not sliding idle expiry.
    let aged = sqlx::query(
        r#"
        UPDATE auth_sessions
        SET absolute_expires_at = now() - interval '1 minute'
        WHERE id = $1
        "#,
    )
    .bind(second_session_id)
    .execute(&pool)
    .await
    .expect("age rotated absolute_expires_at");
    assert_eq!(aged.rows_affected(), 1, "expected the rotated session aged");
    let (abs_still_future, exp_still_future): (bool, bool) = sqlx::query_as(
        r#"
        SELECT absolute_expires_at > now(), expires_at > now()
        FROM auth_sessions
        WHERE id = $1
        "#,
    )
    .bind(second_session_id)
    .fetch_one(&pool)
    .await
    .expect("post-age expiry flags");
    assert!(
        !abs_still_future,
        "aged absolute_expires_at must be in the past"
    );
    assert!(
        exp_still_future,
        "expires_at must stay in the future so the 401 is absolute expiry, not idle expiry"
    );

    let past = call(&app, "POST", "/api/auth/refresh", Some(&rotated_jar), None).await;
    assert_eq!(past.status, StatusCode::UNAUTHORIZED, "{}", past.body);
    assert_eq!(
        past.error(),
        "refresh token expired",
        "past absolute_expires_at must be 401 refresh token expired: {}",
        past.body
    );
    assert_no_auth_cookies(&past, "past-absolute 401");
    assert!(
        session_is_revoked(&pool, second_session_id).await,
        "expired refresh must revoke that session"
    );

    let later = call(&app, "POST", "/api/auth/refresh", Some(&rotated_jar), None).await;
    assert_eq!(
        later.status,
        StatusCode::UNAUTHORIZED,
        "later refresh with the rotated cookie must not succeed past the original absolute: {}",
        later.body
    );
    assert_no_auth_cookies(&later, "later refresh after absolute expiry");
    assert!(
        live_session_rows(&pool, &email).await.is_empty(),
        "expired family must not keep a live session"
    );
    assert_eq!(
        family_rows_past_absolute(&pool, family_id, original_absolute).await,
        0,
        "a later refresh must not create a session that lives past the original absolute"
    );
}

#[tokio::test]
async fn logout_then_me_401() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let out = call(&app, "POST", "/api/auth/logout", Some(&jar), None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED, "{}", me.body);
}

#[tokio::test]
async fn me_lists_at_least_one_workspace() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    let workspaces = me.body["workspaces"].as_array().expect("workspaces array");
    assert!(
        !workspaces.is_empty(),
        "expected at least one workspace: {}",
        me.body
    );
}

#[tokio::test]
async fn create_workspace_slug_and_owner() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let out = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": "Acme Eng", "slug": "acme-eng" })),
    )
    .await;
    assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);
    assert_eq!(out.body["workspace"]["role"], "owner");
    assert_eq!(out.body["workspace"]["name"], "Acme Eng");
    let slug = out.body["workspace"]["slug"]
        .as_str()
        .expect("workspace.slug");
    assert!(
        slug.starts_with("acme-eng-"),
        "expected slug derived from request, got {slug:?}"
    );
}

#[tokio::test]
async fn create_workspace_empty_and_too_long_400() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let empty = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": "   " })),
    )
    .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST, "{}", empty.body);

    let too_long = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": "n".repeat(81) })),
    )
    .await;
    assert_eq!(
        too_long.status,
        StatusCode::BAD_REQUEST,
        "{}",
        too_long.body
    );
}

/// Docs §4 slugify: lowercase ascii alnum, other runs become one dash, max 40.
fn workspace_slugify(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for c in raw.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "workspace".into()
    } else {
        trimmed.chars().take(40).collect()
    }
}

async fn count_workspaces_named(pool: &fleet_db::PgPool, name: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*)::bigint FROM workspaces WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .expect("count workspaces by name")
}

async fn count_workspaces_slugged(pool: &fleet_db::PgPool, slug: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*)::bigint FROM workspaces WHERE slug = $1")
        .bind(slug)
        .fetch_one(pool)
        .await
        .expect("count workspaces by slug")
}

async fn count_memberships(pool: &fleet_db::PgPool, uid: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*)::bigint FROM workspace_members WHERE user_id = $1")
        .bind(uid)
        .fetch_one(pool)
        .await
        .expect("count memberships")
}

/// Docs §4: empty name or name > 80 → 400. Slug collision → 409
/// `workspace slug already exists`. Must not 500 or insert a second row.
#[tokio::test]
async fn create_workspace_rejects_empty_too_long_name_and_colliding_slug() {
    let app = app().await;
    let pool = connect_and_migrate().await;
    let (_email, jar, login) = signup_and_login(&app).await;
    let uid = login.body["user"]["id"]
        .as_str()
        .expect("user.id")
        .parse::<Uuid>()
        .expect("user id uuid");
    let start_count = count_memberships(&pool, uid).await;
    assert_eq!(start_count, 1, "signup must leave exactly one workspace");

    for empty in ["", "   ", "\t\n"] {
        let out = call(
            &app,
            "POST",
            "/api/workspaces",
            Some(&jar),
            Some(json!({ "name": empty })),
        )
        .await;
        assert_ne!(
            out.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "empty name must not 500: {}",
            out.body
        );
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "empty/whitespace name must be 400: {}",
            out.body
        );
        assert_eq!(out.error(), "workspace name is required");
    }
    assert_eq!(
        count_memberships(&pool, uid).await,
        start_count,
        "empty name must not create a workspace"
    );

    let too_long = "n".repeat(81);
    let too_long_out = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": too_long })),
    )
    .await;
    assert_ne!(
        too_long_out.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "name > 80 must not 500: {}",
        too_long_out.body
    );
    assert_eq!(
        too_long_out.status,
        StatusCode::BAD_REQUEST,
        "name > 80 must be 400: {}",
        too_long_out.body
    );
    assert_eq!(too_long_out.error(), "workspace name is too long");
    assert_eq!(
        count_workspaces_named(&pool, &too_long).await,
        0,
        "name > 80 must not insert a workspace"
    );
    assert_eq!(
        count_memberships(&pool, uid).await,
        start_count,
        "name > 80 must not create a membership"
    );

    let at_limit = "n".repeat(80);
    let ok = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": at_limit })),
    )
    .await;
    assert_eq!(
        ok.status,
        StatusCode::CREATED,
        "name of exactly 80 must be accepted: {}",
        ok.body
    );
    assert_eq!(count_memberships(&pool, uid).await, start_count + 1);

    // Docs §4: slug = slugify(name or slug) + "-" + first 8 of new UUID.
    // Uuid v7's first 8 hex chars are unix_ts_ms>>16 (~65s). Occupy this
    // bucket and the next so the handler's insert hits workspaces_slug_uidx
    // even if the request crosses the bucket boundary.
    let collide_name = format!("collide-{}", Uuid::now_v7().as_simple());
    let occupy_id = Uuid::now_v7();
    let bucket =
        u32::from_str_radix(&occupy_id.to_string()[..8], 16).expect("uuid v7 timestamp prefix");
    let occupied: Vec<String> = [bucket, bucket.wrapping_add(1)]
        .into_iter()
        .map(|n| format!("{}-{n:08x}", workspace_slugify(&collide_name)))
        .collect();
    for slug in &occupied {
        sqlx::query(
            r#"
            INSERT INTO workspaces (id, slug, name, created_at, updated_at)
            VALUES ($1, $2, $3, now(), now())
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(slug)
        .bind(format!("occupied-{slug}"))
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("occupy slug {slug}: {e}"));
        assert_eq!(
            count_workspaces_slugged(&pool, slug).await,
            1,
            "occupied {slug}"
        );
    }

    let collide = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": collide_name, "slug": collide_name })),
    )
    .await;
    assert_ne!(
        collide.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "slug collision must not 500: {}",
        collide.body
    );
    assert_eq!(
        collide.status,
        StatusCode::CONFLICT,
        "slug collision must be 409: {}",
        collide.body
    );
    assert_eq!(collide.error(), "workspace slug already exists");
    assert_eq!(
        count_workspaces_named(&pool, &collide_name).await,
        0,
        "colliding create must not insert a second workspace row"
    );
    for slug in &occupied {
        assert_eq!(
            count_workspaces_slugged(&pool, slug).await,
            1,
            "collision must not create a second row for {slug}"
        );
    }
    assert_eq!(
        count_memberships(&pool, uid).await,
        start_count + 1,
        "colliding create must not add a membership"
    );
}

#[tokio::test]
async fn other_user_forbidden_on_personas() {
    let app = app().await;
    let (_a_email, _a_jar, a_login) = signup_and_login(&app).await;
    let ws_id = a_login.body["default_workspace"]["id"]
        .as_str()
        .expect("A default_workspace.id")
        .to_string();

    let (_b_email, b_jar, _) = signup_and_login(&app).await;
    let uri = format!("/api/workspaces/{ws_id}/personas");
    let out = call(&app, "GET", &uri, Some(&b_jar), None).await;
    assert_eq!(out.status, StatusCode::FORBIDDEN, "{}", out.body);
    assert!(
        out.error().contains("not a member"),
        "forbidden should say not a member, got {:?}",
        out.error()
    );
}

/// Docs §9 catalog slugs. Seed-starters insert-if-absent these eight.
const STARTER_PERSONA_SLUGS: &[&str] = &[
    "software-engineer",
    "sales",
    "marketing",
    "customer-support",
    "analyst",
    "devops",
    "product-discovery",
    "content-docs",
];

fn listed_persona_id_slugs(listed: &HttpOut) -> std::collections::BTreeSet<(String, String)> {
    listed.body["personas"]
        .as_array()
        .expect("personas array")
        .iter()
        .map(|p| {
            (
                p["id"].as_str().expect("persona id").to_string(),
                p["slug"].as_str().expect("persona slug").to_string(),
            )
        })
        .collect()
}

async fn get_workspace_personas(app: &axum::Router, jar: &CookieJar, ws: &str) -> HttpOut {
    let listed = call(
        app,
        "GET",
        &format!("/api/workspaces/{ws}/personas"),
        Some(jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    listed
}

/// B must not create a custom persona or seed the 8 starters into A's
/// roster. Distinct from other_user_forbidden_on_personas (GET list only)
/// and persona_foreign_id_get_patch_delete_is_404_and_does_not_archive
/// (GET/PATCH/DELETE by A's persona_id).
#[tokio::test]
async fn personas_seed_create_other_ws_are_403_and_insert_nothing() {
    let app = app().await;
    let (_a_email, a_jar, a_login) = signup_and_login(&app).await;
    let ws_a = workspace_id(&a_login);
    let (_b_email, b_jar, b_login) = signup_and_login(&app).await;
    let ws_b = workspace_id(&b_login);
    assert_ne!(ws_a, ws_b, "A and B must have distinct workspaces");

    let snapshot = get_workspace_personas(&app, &a_jar, &ws_a).await;
    let snap = listed_persona_id_slugs(&snapshot);
    let snap_slugs: std::collections::BTreeSet<&str> =
        snap.iter().map(|(_, slug)| slug.as_str()).collect();
    let starters_already = STARTER_PERSONA_SLUGS
        .iter()
        .filter(|slug| snap_slugs.contains(*slug))
        .count();

    let archived_snap = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/personas?status=archived"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        archived_snap.status,
        StatusCode::OK,
        "A archived snapshot: {}",
        archived_snap.body
    );
    let archived_snap_ids = listed_persona_id_slugs(&archived_snap);

    let token = Uuid::new_v4().as_simple().to_string();
    let stolen_name = format!("Stolen-into-A-{token}");
    let stolen_slug = format!("stolen-into-a-{token}");
    let stolen_prompt = format!("SECRET-B-INTO-A-ROLE-PROMPT-{token}");

    let create_forbidden = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_a}/personas"),
        Some(&b_jar),
        Some(json!({
            "name": stolen_name,
            "slug": stolen_slug,
            "role_prompt": stolen_prompt,
        })),
    )
    .await;
    assert_ne!(
        create_forbidden.status,
        StatusCode::CREATED,
        "B POST A /personas must not 201: {}",
        create_forbidden.body
    );
    assert_eq!(
        create_forbidden.status,
        StatusCode::FORBIDDEN,
        "B POST A /personas must be 403 not-a-member: {}",
        create_forbidden.body
    );
    assert!(
        create_forbidden.error().contains("not a member"),
        "B POST A /personas expected not-a-member, got {:?}",
        create_forbidden.error()
    );
    assert!(
        !persona_body_leaks(&create_forbidden, &stolen_prompt),
        "B POST A /personas 403 must not include the secret prompt: {}",
        create_forbidden.raw
    );
    assert!(
        create_forbidden.body.get("id").is_none() || create_forbidden.body["id"].is_null(),
        "B POST A /personas must not return a created persona: {}",
        create_forbidden.body
    );

    let after_create = get_workspace_personas(&app, &a_jar, &ws_a).await;
    let after_create_ids = listed_persona_id_slugs(&after_create);
    assert_eq!(
        after_create_ids, snap,
        "B POST A /personas must not insert: {}",
        after_create.body
    );
    assert!(
        !after_create_ids
            .iter()
            .any(|(_, slug)| slug == &stolen_slug),
        "B POST A /personas must not add slug {stolen_slug}: {}",
        after_create.body
    );

    let seed_forbidden = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_a}/personas/seed-starters"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_ne!(
        seed_forbidden.status,
        StatusCode::OK,
        "B POST A /personas/seed-starters must not 200: {}",
        seed_forbidden.body
    );
    assert_ne!(
        seed_forbidden.status,
        StatusCode::CREATED,
        "B POST A /personas/seed-starters must not 201: {}",
        seed_forbidden.body
    );
    assert_eq!(
        seed_forbidden.status,
        StatusCode::FORBIDDEN,
        "B POST A /personas/seed-starters must be 403 not-a-member: {}",
        seed_forbidden.body
    );
    assert!(
        seed_forbidden.error().contains("not a member"),
        "B POST A /personas/seed-starters expected not-a-member, got {:?}",
        seed_forbidden.error()
    );
    assert_ne!(
        seed_forbidden.body["inserted"].as_u64(),
        Some(8),
        "B seed-starters 403 must not report inserted=8: {}",
        seed_forbidden.body
    );

    let after_seed = get_workspace_personas(&app, &a_jar, &ws_a).await;
    let after_seed_ids = listed_persona_id_slugs(&after_seed);
    assert_eq!(
        after_seed_ids, snap,
        "B POST A /personas/seed-starters must not insert: {}",
        after_seed.body
    );
    let after_seed_slugs: std::collections::BTreeSet<&str> = after_seed_ids
        .iter()
        .map(|(_, slug)| slug.as_str())
        .collect();
    for slug in STARTER_PERSONA_SLUGS {
        if snap_slugs.contains(*slug) {
            continue;
        }
        assert!(
            !after_seed_slugs.contains(*slug),
            "starter {slug} must not appear unless already in A's snapshot: {}",
            after_seed.body
        );
    }

    let archived_after = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/personas?status=archived"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        archived_after.status,
        StatusCode::OK,
        "A archived after B: {}",
        archived_after.body
    );
    assert_eq!(
        listed_persona_id_slugs(&archived_after),
        archived_snap_ids,
        "B must not insert archived rows on A: {}",
        archived_after.body
    );

    let seeded = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_a}/personas/seed-starters"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(
        seeded.status,
        StatusCode::OK,
        "control: A POST A seed-starters must be 200: {}",
        seeded.body
    );
    let inserted = seeded.body["inserted"].as_u64().unwrap_or(0);
    let skipped = seeded.body["skipped"].as_u64().unwrap_or(0);
    assert_eq!(
        inserted + skipped,
        8,
        "control: A seed-starters must insert-if-absent the 8 starters: {}",
        seeded.body
    );
    assert_eq!(
        inserted,
        8 - starters_already as u64,
        "control: A seed-starters must insert the starters B failed to plant \
         (counted from A's snapshot, not after B's call): {}",
        seeded.body
    );

    let after_a_seed = get_workspace_personas(&app, &a_jar, &ws_a).await;
    let after_a_ids = listed_persona_id_slugs(&after_a_seed);
    let after_a_slugs: std::collections::BTreeSet<&str> =
        after_a_ids.iter().map(|(_, slug)| slug.as_str()).collect();
    for slug in STARTER_PERSONA_SLUGS {
        assert!(
            after_a_slugs.contains(*slug),
            "control: A seed-starters must leave {slug} on A's roster: {}",
            after_a_seed.body
        );
    }

    let b_name = format!("B-own-reviewer-{token}");
    let b_slug = format!("b-own-reviewer-{token}");
    let b_prompt = format!("B-OWN-ROLE-PROMPT-{token}");
    let b_created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_b}/personas"),
        Some(&b_jar),
        Some(json!({
            "name": b_name,
            "slug": b_slug,
            "role_prompt": b_prompt,
        })),
    )
    .await;
    assert_eq!(
        b_created.status,
        StatusCode::CREATED,
        "control: B POST B /personas must be 201 so this is not a tautology \
         that every personas POST 403s: {}",
        b_created.body
    );
    assert_eq!(b_created.body["slug"].as_str(), Some(b_slug.as_str()));
    assert_eq!(
        b_created.body["role_prompt"].as_str(),
        Some(b_prompt.as_str())
    );
}

fn listed_by_slug<'a>(out: &'a HttpOut, slug: &str) -> Vec<&'a Value> {
    out.body["personas"]
        .as_array()
        .expect("personas array")
        .iter()
        .filter(|p| p["slug"].as_str() == Some(slug))
        .collect()
}

/// Docs §9: unique slug per workspace; the same slug in another
/// workspace is allowed. DELETE is a soft archive (`status=archived`),
/// not a hard row delete. Distinct from unknown-id rejection.
#[tokio::test]
async fn persona_slug_is_unique_per_workspace_and_delete_archives() {
    let app = app().await;
    let (_email, jar, login) = signup_and_login(&app).await;
    let ws_a = login.body["default_workspace"]["id"]
        .as_str()
        .expect("default_workspace.id")
        .to_string();

    let token = Uuid::new_v4().as_simple().to_string();
    let slug = format!("shared-reviewer-{token}");
    // Mixed case / spaces must slugify to the same unique slug.
    let slug_input = format!("Shared Reviewer {token}");

    let first = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_a}/personas"),
        Some(&jar),
        Some(json!({
            "name": "Reviewer A",
            "slug": slug_input,
            "description": "first"
        })),
    )
    .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert_eq!(
        first.body["slug"].as_str(),
        Some(slug.as_str()),
        "slugify must normalize mixed-case input: {}",
        first.body
    );
    assert_eq!(
        first.body["status"].as_str(),
        Some("active"),
        "{}",
        first.body
    );
    let first_id = first.body["id"].as_str().expect("created id").to_string();

    // Same slug again (already slugified) → 409, must not insert.
    let dup_explicit = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_a}/personas"),
        Some(&jar),
        Some(json!({
            "name": "Reviewer A clone",
            "slug": slug,
        })),
    )
    .await;
    assert_ne!(
        dup_explicit.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "duplicate slug must not 500: {}",
        dup_explicit.body
    );
    assert_eq!(
        dup_explicit.status,
        StatusCode::CONFLICT,
        "duplicate slug in the same workspace must be 409: {}",
        dup_explicit.body
    );
    assert_eq!(
        dup_explicit.error(),
        "persona slug already exists in this workspace"
    );

    // Same slug via name-only slugify (no slug field) is also a conflict.
    let dup_from_name = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_a}/personas"),
        Some(&jar),
        Some(json!({ "name": slug_input })),
    )
    .await;
    assert_eq!(
        dup_from_name.status,
        StatusCode::CONFLICT,
        "name that slugifies to an existing slug must be 409: {}",
        dup_from_name.body
    );
    assert_eq!(
        dup_from_name.error(),
        "persona slug already exists in this workspace"
    );

    // Same user, second workspace — same slug must be allowed.
    let ws2 = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": format!("Other {token}") })),
    )
    .await;
    assert_eq!(ws2.status, StatusCode::CREATED, "{}", ws2.body);
    let ws_b = ws2.body["workspace"]["id"]
        .as_str()
        .expect("workspace.id")
        .to_string();
    assert_ne!(ws_b, ws_a, "second workspace must be distinct");

    let other = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_b}/personas"),
        Some(&jar),
        Some(json!({
            "name": "Reviewer B",
            "slug": slug,
        })),
    )
    .await;
    assert_eq!(
        other.status,
        StatusCode::CREATED,
        "same slug in another workspace must succeed: {}",
        other.body
    );
    assert_eq!(
        other.body["slug"].as_str(),
        Some(slug.as_str()),
        "{}",
        other.body
    );
    assert_eq!(
        other.body["status"].as_str(),
        Some("active"),
        "{}",
        other.body
    );
    let other_id = other.body["id"].as_str().expect("other id").to_string();
    assert_ne!(
        other_id, first_id,
        "cross-workspace persona must be a new row"
    );

    let listed_a = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/personas"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(listed_a.status, StatusCode::OK, "{}", listed_a.body);
    let a_hits = listed_by_slug(&listed_a, &slug);
    assert_eq!(
        a_hits.len(),
        1,
        "duplicate POSTs must not insert a second row: {}",
        listed_a.body
    );
    assert_eq!(a_hits[0]["id"].as_str(), Some(first_id.as_str()));
    assert_eq!(a_hits[0]["status"].as_str(), Some("active"));

    // DELETE is a soft archive, not a hard row delete.
    let deleted = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws_a}/personas/{first_id}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        deleted.status,
        StatusCode::NO_CONTENT,
        "DELETE must archive: {}",
        deleted.body
    );

    let got = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/personas/{first_id}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "archived persona must still GET: {}",
        got.body
    );
    assert_eq!(got.body["id"].as_str(), Some(first_id.as_str()));
    assert_eq!(got.body["slug"].as_str(), Some(slug.as_str()));
    assert_eq!(
        got.body["status"].as_str(),
        Some("archived"),
        "DELETE must set status=archived, not drop the row: {}",
        got.body
    );

    let listed_active = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/personas"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        listed_active.status,
        StatusCode::OK,
        "{}",
        listed_active.body
    );
    assert!(
        listed_by_slug(&listed_active, &slug).is_empty(),
        "default list is active-only; archived {first_id} leaked: {}",
        listed_active.body
    );

    let listed_archived = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/personas?status=archived"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        listed_archived.status,
        StatusCode::OK,
        "{}",
        listed_archived.body
    );
    let archived_hits = listed_by_slug(&listed_archived, &slug);
    assert_eq!(
        archived_hits.len(),
        1,
        "archived list must still contain the same row: {}",
        listed_archived.body
    );
    assert_eq!(archived_hits[0]["id"].as_str(), Some(first_id.as_str()));
    assert_eq!(archived_hits[0]["status"].as_str(), Some("archived"));

    // Uniqueness is per workspace including archived rows — archive must
    // not free the slug the way a hard DELETE would.
    let reuse = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_a}/personas"),
        Some(&jar),
        Some(json!({
            "name": "Reviewer A revived",
            "slug": slug,
        })),
    )
    .await;
    assert_ne!(
        reuse.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "reusing an archived slug must not 500: {}",
        reuse.body
    );
    assert_eq!(
        reuse.status,
        StatusCode::CONFLICT,
        "archived slug must still be unique in this workspace: {}",
        reuse.body
    );
    assert_eq!(
        reuse.error(),
        "persona slug already exists in this workspace"
    );

    let listed_archived_after = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/personas?status=archived"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        listed_archived_after.status,
        StatusCode::OK,
        "{}",
        listed_archived_after.body
    );
    let after_hits = listed_by_slug(&listed_archived_after, &slug);
    assert_eq!(
        after_hits.len(),
        1,
        "reuse-after-archive must not insert another row: {}",
        listed_archived_after.body
    );
    assert_eq!(after_hits[0]["id"].as_str(), Some(first_id.as_str()));

    let listed_active_after = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/personas"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        listed_active_after.status,
        StatusCode::OK,
        "{}",
        listed_active_after.body
    );
    assert!(
        listed_by_slug(&listed_active_after, &slug).is_empty(),
        "reuse-after-archive must not create a second active row: {}",
        listed_active_after.body
    );

    // Other workspace is untouched: still active, same id/slug.
    let got_other = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_b}/personas/{other_id}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got_other.status, StatusCode::OK, "{}", got_other.body);
    assert_eq!(got_other.body["slug"].as_str(), Some(slug.as_str()));
    assert_eq!(got_other.body["status"].as_str(), Some("active"));
}

/// A's GET /personas/{pid} must still be the original active row.
async fn assert_persona_still_a_snapshot(
    app: &axum::Router,
    jar: &CookieJar,
    ws: &str,
    pid: &str,
    name: &str,
    role_prompt: &str,
    when: &str,
) {
    let got = call(
        app,
        "GET",
        &format!("/api/workspaces/{ws}/personas/{pid}"),
        Some(jar),
        None,
    )
    .await;
    assert_eq!(
        got.status,
        StatusCode::OK,
        "{when}: owner GET must stay 200: {}",
        got.body
    );
    assert_eq!(
        got.body["id"].as_str(),
        Some(pid),
        "{when}: id: {}",
        got.body
    );
    assert_eq!(
        got.body["name"].as_str(),
        Some(name),
        "{when}: name must not be rewritten: {}",
        got.body
    );
    assert_eq!(
        got.body["role_prompt"].as_str(),
        Some(role_prompt),
        "{when}: role_prompt must not be rewritten: {}",
        got.body
    );
    assert_eq!(
        got.body["status"].as_str(),
        Some("active"),
        "{when}: DELETE must not archive: {}",
        got.body
    );
}

fn persona_body_leaks(out: &HttpOut, secret: &str) -> bool {
    if secret.is_empty() {
        return false;
    }
    let needle = secret.to_ascii_lowercase();
    out.raw.to_ascii_lowercase().contains(&needle)
        || out.body.to_string().to_ascii_lowercase().contains(&needle)
}

/// GET/PATCH/DELETE of another workspace's persona_id: B on B's path is
/// 404 (must not leak name/role_prompt or rewrite/archive A). B on A's
/// path is 403 not-a-member. Distinct from other_user_forbidden_on_personas
/// (list only) and unknown_persona_id_is_rejected (random UUID on own path).
#[tokio::test]
async fn persona_foreign_id_get_patch_delete_is_404_and_does_not_archive() {
    let app = app().await;
    let (_a_email, a_jar, a_login) = signup_and_login(&app).await;
    let ws_a = workspace_id(&a_login);
    let (_b_email, b_jar, b_login) = signup_and_login(&app).await;
    let ws_b = workspace_id(&b_login);
    assert_ne!(ws_a, ws_b, "A and B must have distinct workspaces");

    let token = Uuid::new_v4().as_simple().to_string();
    let name = format!("A-foreign-lock-{token}");
    let role_prompt = format!("SECRET-A-ROLE-PROMPT-{token}");
    let stolen_name = format!("stolen-by-B-{token}");
    let stolen_prompt = format!("STOLEN-ROLE-PROMPT-{token}");

    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_a}/personas"),
        Some(&a_jar),
        Some(json!({
            "name": name,
            "role_prompt": role_prompt,
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let pid = created.body["id"].as_str().expect("created id").to_string();
    assert_eq!(created.body["name"].as_str(), Some(name.as_str()));
    assert_eq!(
        created.body["role_prompt"].as_str(),
        Some(role_prompt.as_str())
    );
    assert_eq!(created.body["status"].as_str(), Some("active"));

    let snap = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/personas/{pid}"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(snap.status, StatusCode::OK, "A snapshot GET: {}", snap.body);
    assert_eq!(snap.body["id"].as_str(), Some(pid.as_str()));
    assert_eq!(snap.body["name"].as_str(), Some(name.as_str()));
    assert_eq!(
        snap.body["role_prompt"].as_str(),
        Some(role_prompt.as_str())
    );
    assert_eq!(snap.body["status"].as_str(), Some("active"));

    let steal = json!({
        "name": stolen_name,
        "role_prompt": stolen_prompt,
        "status": "archived",
    });

    for method in ["GET", "PATCH", "DELETE"] {
        let uri = format!("/api/workspaces/{ws_b}/personas/{pid}");
        let body = (method == "PATCH").then(|| steal.clone());
        let out = call(&app, method, &uri, Some(&b_jar), body).await;
        assert_ne!(
            out.status,
            StatusCode::OK,
            "B {method} of A's persona_id on B's path must not return A's row: {}",
            out.body
        );
        assert_ne!(
            out.status,
            StatusCode::NO_CONTENT,
            "B {method} of A's persona_id on B's path must not archive: {}",
            out.body
        );
        assert_eq!(
            out.status,
            StatusCode::NOT_FOUND,
            "B {method} of A's persona_id on B's path must be 404: {}",
            out.body
        );
        assert!(
            out.error().contains("persona not found"),
            "B {method} on B's path should say persona not found, got {:?}",
            out.error()
        );
        assert!(
            !persona_body_leaks(&out, &name),
            "B {method} 404 must not include A's name: {}",
            out.raw
        );
        assert!(
            !persona_body_leaks(&out, &role_prompt),
            "B {method} 404 must not include A's role_prompt: {}",
            out.raw
        );
        assert_persona_still_a_snapshot(
            &app,
            &a_jar,
            &ws_a,
            &pid,
            &name,
            &role_prompt,
            &format!("after B {method} on B's path"),
        )
        .await;
        let rewritten = call(
            &app,
            "GET",
            &format!("/api/workspaces/{ws_a}/personas/{pid}"),
            Some(&a_jar),
            None,
        )
        .await;
        assert_ne!(
            rewritten.body["name"].as_str(),
            Some(stolen_name.as_str()),
            "B {method} must not rewrite A's name: {}",
            rewritten.body
        );
        assert_ne!(
            rewritten.body["role_prompt"].as_str(),
            Some(stolen_prompt.as_str()),
            "B {method} must not rewrite A's role_prompt: {}",
            rewritten.body
        );
    }

    for method in ["GET", "PATCH", "DELETE"] {
        let uri = format!("/api/workspaces/{ws_a}/personas/{pid}");
        let body = (method == "PATCH").then(|| steal.clone());
        let out = call(&app, method, &uri, Some(&b_jar), body).await;
        assert_eq!(
            out.status,
            StatusCode::FORBIDDEN,
            "B {method} on A's path must be 403 not-a-member: {}",
            out.body
        );
        assert!(
            out.error().contains("not a member"),
            "B {method} on A's path should say not a member, got {:?}",
            out.error()
        );
        assert!(
            !persona_body_leaks(&out, &role_prompt),
            "B {method} 403 must not include A's role_prompt: {}",
            out.raw
        );
        assert!(
            !persona_body_leaks(&out, &name),
            "B {method} 403 must not include A's name: {}",
            out.raw
        );
    }

    assert_persona_still_a_snapshot(
        &app,
        &a_jar,
        &ws_a,
        &pid,
        &name,
        &role_prompt,
        "after B GET/PATCH/DELETE on A's path",
    )
    .await;

    let b_name = format!("B-own-persona-{token}");
    let b_prompt = format!("B-OWN-ROLE-PROMPT-{token}");
    let b_created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_b}/personas"),
        Some(&b_jar),
        Some(json!({
            "name": b_name,
            "role_prompt": b_prompt,
        })),
    )
    .await;
    assert_eq!(
        b_created.status,
        StatusCode::CREATED,
        "control: B POST on B must 201: {}",
        b_created.body
    );
    let b_pid = b_created.body["id"]
        .as_str()
        .expect("B created id")
        .to_string();
    assert_ne!(b_pid, pid, "control persona must be a different row");

    let b_got = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_b}/personas/{b_pid}"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        b_got.status,
        StatusCode::OK,
        "control: B GET of B's persona must 200 (not a tautology that every persona GET is 404): {}",
        b_got.body
    );
    assert_eq!(b_got.body["id"].as_str(), Some(b_pid.as_str()));
    assert_eq!(b_got.body["name"].as_str(), Some(b_name.as_str()));
    assert_eq!(b_got.body["role_prompt"].as_str(), Some(b_prompt.as_str()));
    assert_eq!(b_got.body["status"].as_str(), Some("active"));

    assert_persona_still_a_snapshot(
        &app,
        &a_jar,
        &ws_a,
        &pid,
        &name,
        &role_prompt,
        "after B creates their own persona",
    )
    .await;
}

fn listed_persona_names(listed: &HttpOut) -> Vec<&str> {
    listed.body["personas"]
        .as_array()
        .expect("personas array")
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect()
}

/// POST /personas: empty or name > 80 → 400 and no insert.
/// 1-char and 80-char still 201. Other-ws cookie is 403.
#[tokio::test]
async fn persona_name_len_rejects_empty_and_over_80_and_inserts_no_row() {
    let app = app().await;
    let (_email, jar, login) = signup_and_login(&app).await;
    let ws = workspace_id(&login);
    let uri = format!("/api/workspaces/{ws}/personas");

    let snapshot = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(snapshot.status, StatusCode::OK, "{}", snapshot.body);
    let snap_names = listed_persona_names(&snapshot);
    let snap_count = snap_names.len();

    let too_long = "x".repeat(81);
    for (raw, expected_err) in [
        ("", "name is required"),
        ("   ", "name is required"),
        (too_long.as_str(), "name is too long"),
    ] {
        let out = call(&app, "POST", &uri, Some(&jar), Some(json!({ "name": raw }))).await;
        assert_ne!(
            out.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "persona name {raw:?} must not 500: {}",
            out.body
        );
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "persona name {raw:?} must be 400: {}",
            out.body
        );
        assert_eq!(out.error(), expected_err, "persona name {raw:?}");

        let after = call(&app, "GET", &uri, Some(&jar), None).await;
        assert_eq!(after.status, StatusCode::OK, "{}", after.body);
        let names = listed_persona_names(&after);
        assert_eq!(
            names.len(),
            snap_count,
            "rejected name {raw:?} must not change GET /personas count: {names:?}"
        );
        assert!(
            !names.contains(&raw),
            "rejected name {raw:?} must not appear on GET /personas: {names:?}"
        );
    }

    let (_b_email, b_jar, _) = signup_and_login(&app).await;
    let foreign_name = format!("foreign-{}", Uuid::new_v4().as_simple());
    let forbidden = call(
        &app,
        "POST",
        &uri,
        Some(&b_jar),
        Some(json!({ "name": foreign_name })),
    )
    .await;
    assert_eq!(
        forbidden.status,
        StatusCode::FORBIDDEN,
        "other-ws POST must be 403: {}",
        forbidden.body
    );
    assert!(
        forbidden.error().contains("not a member"),
        "other-ws POST should say not a member, got {:?}",
        forbidden.error()
    );
    let after_foreign = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(
        after_foreign.status,
        StatusCode::OK,
        "{}",
        after_foreign.body
    );
    let after_foreign_names = listed_persona_names(&after_foreign);
    assert_eq!(
        after_foreign_names.len(),
        snap_count,
        "other-ws POST must not insert on A: {after_foreign_names:?}"
    );
    assert!(
        !after_foreign_names.contains(&foreign_name.as_str()),
        "other-ws unique name must not appear on A: {after_foreign_names:?}"
    );

    let token = Uuid::new_v4().as_simple().to_string();
    let one: String = token.chars().take(1).collect();
    assert_eq!(one.chars().count(), 1);
    let one_out = call(&app, "POST", &uri, Some(&jar), Some(json!({ "name": one }))).await;
    assert_eq!(
        one_out.status,
        StatusCode::CREATED,
        "1-char name must be 201: {}",
        one_out.body
    );
    assert_eq!(one_out.body["name"].as_str(), Some(one.as_str()));

    let eighty = format!("{token}{}", "n".repeat(80 - token.len()));
    assert_eq!(eighty.len(), 80);
    let eighty_out = call(
        &app,
        "POST",
        &uri,
        Some(&jar),
        Some(json!({ "name": eighty })),
    )
    .await;
    assert_eq!(
        eighty_out.status,
        StatusCode::CREATED,
        "80-char name must be 201: {}",
        eighty_out.body
    );
    assert_eq!(eighty_out.body["name"].as_str(), Some(eighty.as_str()));

    let listed = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let names = listed_persona_names(&listed);
    assert!(
        names.contains(&one.as_str()),
        "1-char name must appear on GET /personas: {names:?}"
    );
    assert!(
        names.contains(&eighty.as_str()),
        "80-char name must appear on GET /personas: {names:?}"
    );
}

/// PATCH /personas/{id}: name > 80 → 400 and does not persist.
/// Empty / whitespace must not store an empty roster label.
/// Exactly 80 still 200. Distinct from POST name-len (create only).
#[tokio::test]
async fn persona_patch_name_over_80_is_400_and_does_not_persist() {
    let app = app().await;
    let (_email, jar, login) = signup_and_login(&app).await;
    let ws = workspace_id(&login);
    let token = Uuid::new_v4().as_simple().to_string();
    let created_name = format!("Patch name {token}");
    assert!(
        (1..=80).contains(&created_name.len()),
        "setup name must be 1–80: {created_name:?}"
    );

    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/personas"),
        Some(&jar),
        Some(json!({ "name": created_name })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "setup POST must be 201: {}",
        created.body
    );
    let pid = created.body["id"].as_str().expect("created id").to_string();

    let snapshot = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/personas/{pid}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(
        snapshot.status,
        StatusCode::OK,
        "snapshot GET /personas/{{id}} must be 200: {}",
        snapshot.body
    );
    let snap_name = snapshot.body["name"]
        .as_str()
        .expect("snapshot name")
        .to_string();
    let snap_slug = snapshot.body["slug"]
        .as_str()
        .expect("snapshot slug")
        .to_string();
    assert_eq!(snap_name, created_name, "snapshot name: {}", snapshot.body);

    let too_long_81 = "x".repeat(81);
    let too_long_200 = "x".repeat(200);
    for raw in [too_long_81.as_str(), too_long_200.as_str()] {
        let out = call(
            &app,
            "PATCH",
            &format!("/api/workspaces/{ws}/personas/{pid}"),
            Some(&jar),
            Some(json!({ "name": raw })),
        )
        .await;
        assert_ne!(
            out.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "PATCH name len {} must not 500: {}",
            raw.len(),
            out.body
        );
        assert_ne!(
            out.status,
            StatusCode::OK,
            "PATCH name len {} must not 200 (name is too long): {}",
            raw.len(),
            out.body
        );
        assert_eq!(
            out.status,
            StatusCode::BAD_REQUEST,
            "PATCH name len {} must be 400: {}",
            raw.len(),
            out.body
        );
        assert_eq!(
            out.error(),
            "name is too long",
            "PATCH name len {}: {}",
            raw.len(),
            out.body
        );

        let after = call(
            &app,
            "GET",
            &format!("/api/workspaces/{ws}/personas/{pid}"),
            Some(&jar),
            None,
        )
        .await;
        assert_eq!(after.status, StatusCode::OK, "{}", after.body);
        assert_eq!(
            after.body["name"].as_str(),
            Some(snap_name.as_str()),
            "rejected PATCH name len {} must not persist: {}",
            raw.len(),
            after.body
        );
        assert_eq!(
            after.body["slug"].as_str(),
            Some(snap_slug.as_str()),
            "rejected PATCH must not change slug: {}",
            after.body
        );
        let after_name = after.body["name"].as_str().unwrap_or("");
        assert_ne!(
            after_name,
            raw,
            "rejected {n}-char name must not be stored: {}",
            after.body,
            n = raw.len()
        );
        assert!(
            after_name.len() <= 80,
            "GET name after rejected PATCH must stay ≤80: {}",
            after.body
        );
    }

    for raw in ["", "   "] {
        let out = call(
            &app,
            "PATCH",
            &format!("/api/workspaces/{ws}/personas/{pid}"),
            Some(&jar),
            Some(json!({ "name": raw })),
        )
        .await;
        assert_ne!(
            out.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "PATCH name {raw:?} must not 500: {}",
            out.body
        );
        if out.status == StatusCode::BAD_REQUEST {
            assert_eq!(
                out.error(),
                "name is required",
                "PATCH name {raw:?} 400 must say name is required: {}",
                out.body
            );
        } else {
            assert_eq!(
                out.status,
                StatusCode::OK,
                "PATCH name {raw:?} must be 400 name is required, or 200 no-op: {}",
                out.body
            );
            assert_eq!(
                out.body["name"].as_str(),
                Some(snap_name.as_str()),
                "PATCH name {raw:?} 200 must not change the roster label: {}",
                out.body
            );
        }

        let after = call(
            &app,
            "GET",
            &format!("/api/workspaces/{ws}/personas/{pid}"),
            Some(&jar),
            None,
        )
        .await;
        assert_eq!(after.status, StatusCode::OK, "{}", after.body);
        let after_name = after.body["name"].as_str().unwrap_or("\0");
        assert_eq!(
            after_name,
            snap_name.as_str(),
            "PATCH name {raw:?} must not persist an empty roster label: {}",
            after.body
        );
        assert!(
            !after_name.trim().is_empty(),
            "storing an empty roster label is a fail: {}",
            after.body
        );
    }

    let eighty = "x".repeat(80);
    assert_eq!(eighty.len(), 80);
    let control = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/personas/{pid}"),
        Some(&jar),
        Some(json!({ "name": eighty })),
    )
    .await;
    assert_eq!(
        control.status,
        StatusCode::OK,
        "control PATCH 80-char name must be 200 (not a tautology that every PATCH 400s): {}",
        control.body
    );
    assert_eq!(
        control.body["name"].as_str(),
        Some(eighty.as_str()),
        "control PATCH must persist the 80-char name: {}",
        control.body
    );

    let got = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/personas/{pid}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(
        got.body["name"].as_str(),
        Some(eighty.as_str()),
        "GET after control PATCH must show the 80-char name: {}",
        got.body
    );
}

fn persona_list_has_name(listed: &HttpOut, name: &str) -> bool {
    listed_persona_names(listed).iter().any(|n| *n == name)
}

fn persona_list_contains_blob(listed: &HttpOut, blob: &str) -> bool {
    if blob.is_empty() {
        return false;
    }
    if listed.raw.contains(blob) {
        return true;
    }
    listed.body["personas"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|p| {
            p["role_prompt"].as_str() == Some(blob)
                || p["description"].as_str() == Some(blob)
                || p["name"].as_str() == Some(blob)
        })
}

fn assert_persona_field_too_long(out: &HttpOut, field: &str, raw_len: usize) {
    assert_ne!(
        out.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{field} len {raw_len} must not 500: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::CREATED,
        "{field} len {raw_len} must not 201 (stores the 32k+ string): {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{field} len {raw_len} must not 200 (stores the 32k+ string): {}",
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::BAD_REQUEST,
        "{field} len {raw_len} must be 400: {}",
        out.body
    );
    let err = out.error().to_ascii_lowercase();
    assert!(
        err.contains("too long")
            || err.contains("32k")
            || err.contains("32000")
            || err.contains("32_000"),
        "{field} len {raw_len} 400 must be role_prompt/description too long (or the 32k family), got {:?}: {}",
        out.error(),
        out.body
    );
}

/// POST/PATCH /personas: role_prompt or description over 32k → 400 and no persist.
/// Short / omitted role_prompt still 201. Distinct from name-len cards.
#[tokio::test]
async fn persona_role_prompt_or_description_over_32k_is_400_and_does_not_persist() {
    let app = app().await;
    let (_email, jar, login) = signup_and_login(&app).await;
    let ws = workspace_id(&login);
    let uri = format!("/api/workspaces/{ws}/personas");
    let token = Uuid::new_v4().as_simple().to_string();
    let huge = "x".repeat(32_001);
    assert_eq!(huge.len(), 32_001);

    let snapshot = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(snapshot.status, StatusCode::OK, "{}", snapshot.body);
    let snap_count = listed_persona_names(&snapshot).len();
    assert!(
        !persona_list_contains_blob(&snapshot, &huge),
        "pre-call GET /personas must not already contain the 32001-char string"
    );

    let over_prompt_name = format!("Over prompt {token}");
    assert!(
        (1..=80).contains(&over_prompt_name.len()),
        "over-prompt name must be 1–80: {over_prompt_name:?}"
    );
    let over_prompt = call(
        &app,
        "POST",
        &uri,
        Some(&jar),
        Some(json!({
            "name": over_prompt_name,
            "role_prompt": huge,
        })),
    )
    .await;
    assert_persona_field_too_long(&over_prompt, "POST role_prompt", huge.len());
    assert!(
        !over_prompt.raw.contains(&huge),
        "400 body must not echo the 32001-char role_prompt: {}",
        over_prompt.status
    );

    let after_prompt = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(after_prompt.status, StatusCode::OK, "{}", after_prompt.body);
    let after_prompt_names = listed_persona_names(&after_prompt);
    assert_eq!(
        after_prompt_names.len(),
        snap_count,
        "rejected 32001-char role_prompt must not change GET /personas count: {after_prompt_names:?}"
    );
    assert!(
        !persona_list_has_name(&after_prompt, &over_prompt_name),
        "rejected role_prompt name must not appear on GET /personas: {after_prompt_names:?}"
    );
    assert!(
        !persona_list_contains_blob(&after_prompt, &huge),
        "GET /personas after rejected role_prompt must not contain the 32001-char string"
    );

    let over_desc_name = format!("Over desc {token}");
    assert!(
        (1..=80).contains(&over_desc_name.len()),
        "over-desc name must be 1–80: {over_desc_name:?}"
    );
    let over_desc = call(
        &app,
        "POST",
        &uri,
        Some(&jar),
        Some(json!({
            "name": over_desc_name,
            "description": huge,
        })),
    )
    .await;
    assert_persona_field_too_long(&over_desc, "POST description", huge.len());
    assert!(
        !over_desc.raw.contains(&huge),
        "400 body must not echo the 32001-char description: {}",
        over_desc.status
    );

    let after_desc = call(&app, "GET", &uri, Some(&jar), None).await;
    assert_eq!(after_desc.status, StatusCode::OK, "{}", after_desc.body);
    let after_desc_names = listed_persona_names(&after_desc);
    assert_eq!(
        after_desc_names.len(),
        snap_count,
        "rejected 32001-char description must not change GET /personas count: {after_desc_names:?}"
    );
    assert!(
        !persona_list_has_name(&after_desc, &over_desc_name),
        "rejected description name must not appear on GET /personas: {after_desc_names:?}"
    );
    assert!(
        !persona_list_contains_blob(&after_desc, &huge),
        "GET /personas after rejected description must not contain the 32001-char string"
    );

    let valid_name = format!("Valid persona {token}");
    let short_prompt = format!("short-role-prompt-{token}");
    let short_desc = format!("short-desc-{token}");
    assert!((1..=80).contains(&valid_name.len()));
    let created = call(
        &app,
        "POST",
        &uri,
        Some(&jar),
        Some(json!({
            "name": valid_name,
            "description": short_desc,
            "role_prompt": short_prompt,
        })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "setup POST short prompt must be 201 (not a tautology that every create 400s): {}",
        created.body
    );
    assert_eq!(
        created.body["role_prompt"].as_str(),
        Some(short_prompt.as_str()),
        "setup POST must persist the short role_prompt: {}",
        created.body
    );
    assert_eq!(
        created.body["description"].as_str(),
        Some(short_desc.as_str()),
        "setup POST must persist the short description: {}",
        created.body
    );
    let pid = created.body["id"].as_str().expect("created id").to_string();

    let detail_uri = format!("/api/workspaces/{ws}/personas/{pid}");
    let snap = call(&app, "GET", &detail_uri, Some(&jar), None).await;
    assert_eq!(
        snap.status,
        StatusCode::OK,
        "snapshot GET /personas/{{id}} must be 200: {}",
        snap.body
    );
    let snap_prompt = snap.body["role_prompt"]
        .as_str()
        .expect("snapshot role_prompt")
        .to_string();
    let snap_desc = snap.body["description"]
        .as_str()
        .expect("snapshot description")
        .to_string();
    assert_eq!(
        snap_prompt, short_prompt,
        "snapshot role_prompt: {}",
        snap.body
    );
    assert_eq!(snap_desc, short_desc, "snapshot description: {}", snap.body);

    for (field, body) in [
        ("PATCH role_prompt", json!({ "role_prompt": huge })),
        ("PATCH description", json!({ "description": huge })),
    ] {
        let out = call(&app, "PATCH", &detail_uri, Some(&jar), Some(body)).await;
        assert_persona_field_too_long(&out, field, huge.len());
        assert!(
            !out.raw.contains(&huge),
            "{field} 400 body must not echo the 32001-char string: {}",
            out.status
        );

        let after = call(&app, "GET", &detail_uri, Some(&jar), None).await;
        assert_eq!(after.status, StatusCode::OK, "{}", after.body);
        assert_eq!(
            after.body["role_prompt"].as_str(),
            Some(snap_prompt.as_str()),
            "{field} must not persist role_prompt: {}",
            after.body
        );
        assert_eq!(
            after.body["description"].as_str(),
            Some(snap_desc.as_str()),
            "{field} must not persist description: {}",
            after.body
        );
        assert!(
            !after.raw.contains(&huge),
            "GET after rejected {field} must not contain the 32001-char string"
        );
    }

    let control_name = format!("Control persona {token}");
    let control_prompt = format!("control-role-prompt-{token}");
    assert!((1..=80).contains(&control_name.len()));
    let control = call(
        &app,
        "POST",
        &uri,
        Some(&jar),
        Some(json!({
            "name": control_name,
            "role_prompt": control_prompt,
        })),
    )
    .await;
    assert_eq!(
        control.status,
        StatusCode::CREATED,
        "control short unique role_prompt must be 201 (not a tautology that every create 400s): {}",
        control.body
    );
    assert_eq!(
        control.body["role_prompt"].as_str(),
        Some(control_prompt.as_str()),
        "control POST must persist the short unique role_prompt: {}",
        control.body
    );
    let control_id = control.body["id"].as_str().expect("control id").to_string();
    let control_got = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/personas/{control_id}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(control_got.status, StatusCode::OK, "{}", control_got.body);
    assert_eq!(
        control_got.body["role_prompt"].as_str(),
        Some(control_prompt.as_str()),
        "GET after control POST must show the short unique role_prompt: {}",
        control_got.body
    );

    let omitted_name = format!("Omitted prompt {token}");
    assert!((1..=80).contains(&omitted_name.len()));
    let omitted = call(
        &app,
        "POST",
        &uri,
        Some(&jar),
        Some(json!({ "name": omitted_name })),
    )
    .await;
    assert_eq!(
        omitted.status,
        StatusCode::CREATED,
        "control omitted role_prompt must be 201: {}",
        omitted.body
    );
    let omitted_prompt = omitted.body["role_prompt"].as_str().unwrap_or("\0");
    assert_ne!(
        omitted_prompt,
        huge.as_str(),
        "omitted role_prompt must not store the 32001-char string: {}",
        omitted.body
    );
    assert!(
        omitted_prompt.len() <= 32_000,
        "omitted role_prompt must not store a 32k+ string: {}",
        omitted.body
    );

    let at_cap = "y".repeat(32_000);
    assert_eq!(at_cap.len(), 32_000);
    let cap_name = format!("At cap {token}");
    assert!((1..=80).contains(&cap_name.len()));
    let cap_out = call(
        &app,
        "POST",
        &uri,
        Some(&jar),
        Some(json!({
            "name": cap_name,
            "role_prompt": at_cap,
        })),
    )
    .await;
    assert_ne!(
        cap_out.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "32_000-char role_prompt must not 500: {}",
        cap_out.body
    );
    if cap_out.status == StatusCode::CREATED {
        assert_eq!(
            cap_out.body["role_prompt"].as_str(),
            Some(at_cap.as_str()),
            "201 at the 32k boundary must persist the 32_000-char prompt: {}",
            cap_out.body
        );
    } else {
        assert_eq!(
            cap_out.status,
            StatusCode::BAD_REQUEST,
            "32_000-char role_prompt may 201, or 400 if the documented cap is lower: {}",
            cap_out.body
        );
        let after_cap = call(&app, "GET", &uri, Some(&jar), None).await;
        assert_eq!(after_cap.status, StatusCode::OK, "{}", after_cap.body);
        assert!(
            !persona_list_has_name(&after_cap, &cap_name),
            "lower-cap reject must not insert the 32_000-char row: {:?}",
            listed_persona_names(&after_cap)
        );
    }
}

#[tokio::test]
async fn unauthenticated_cookie_routes_are_401() {
    let app = app().await;
    for (method, uri) in [
        ("GET", "/api/auth/me"),
        ("GET", "/api/workspaces"),
        ("POST", "/api/workspaces"),
    ] {
        let out = call(&app, method, uri, None, Some(json!({ "name": "x" }))).await;
        assert_eq!(
            out.status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri}: {}",
            out.body
        );
    }
}

#[tokio::test]
async fn refresh_without_cookie_is_401() {
    let app = app().await;
    let out = call(&app, "POST", "/api/auth/refresh", None, None).await;
    assert_eq!(out.status, StatusCode::UNAUTHORIZED, "{}", out.body);
    assert_eq!(out.error(), "missing refresh token");
}

fn assert_me_not_authenticated(out: &HttpOut, when: &str) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{when} must be 401 not authenticated: {}",
        out.body
    );
    assert!(
        out.body.get("user").is_none(),
        "{when} must not include user: {}",
        out.body
    );
    assert!(
        out.body.get("workspaces").is_none(),
        "{when} must not include workspaces: {}",
        out.body
    );
    assert!(
        !out.error().is_empty(),
        "{when} must carry an error body: {}",
        out.body
    );
}

/// Docs §3.4 / fleet-auth README: `fleet_access` is a JWT (sub/email/sid);
/// `fleet_refresh` is opaque and hashed SHA-256 at rest. `refresh()` hashes
/// the refresh cookie — it must not decode a JWT or treat `sid` as a refresh.
/// `require_user` decodes the access JWT then `is_session_active`.
/// A hash miss must not `revoke_family` (that is reuse of a rotated refresh).
#[tokio::test]
async fn access_jwt_rejected_as_refresh() {
    let app = app().await;
    let (email, jar, _) = signup_and_login(&app).await;
    let access = jar
        .access()
        .expect("login must set fleet_access")
        .to_string();
    let refresh = jar
        .refresh()
        .expect("login must set fleet_refresh")
        .to_string();
    assert_ne!(
        access, refresh,
        "access JWT and opaque refresh must be distinct values"
    );
    assert_eq!(
        access.split('.').count(),
        3,
        "fleet_access must be a JWT, got {access:?}"
    );
    assert!(
        !refresh.contains('.'),
        "fleet_refresh must be opaque (not a JWT), got {refresh:?}"
    );
    assert_access_jwt_still_decodes(&access);

    // 1. Access JWT in the refresh cookie (no real refresh) is a hash miss.
    let mut jwt_as_refresh = CookieJar::default();
    jwt_as_refresh.insert(REFRESH_COOKIE, access.clone());
    assert!(
        jwt_as_refresh.access().is_none(),
        "this request must not send fleet_access"
    );
    assert_eq!(jwt_as_refresh.refresh(), Some(access.as_str()));

    let as_refresh = call(
        &app,
        "POST",
        "/api/auth/refresh",
        Some(&jwt_as_refresh),
        None,
    )
    .await;
    assert_eq!(
        as_refresh.status,
        StatusCode::UNAUTHORIZED,
        "access JWT as fleet_refresh must be 401: {}",
        as_refresh.body
    );
    assert_eq!(
        as_refresh.error(),
        "invalid refresh token",
        "hash miss must be invalid refresh token, not reuse/family: {}",
        as_refresh.body
    );
    let as_refresh_msg = as_refresh.error().to_ascii_lowercase();
    assert!(
        !as_refresh_msg.contains("reuse") && !as_refresh_msg.contains("family"),
        "JWT-as-refresh is not token reuse: {:?}",
        as_refresh.error()
    );
    assert_no_auth_cookies(&as_refresh, "JWT-as-refresh 401");
    assert!(
        as_refresh.body.get("user").is_none() && as_refresh.body.get("workspaces").is_none(),
        "JWT-as-refresh 401 must not leak user/workspaces: {}",
        as_refresh.body
    );

    let after_hash_miss = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        after_hash_miss.status,
        StatusCode::OK,
        "hash miss must not revoke_family: {}",
        after_hash_miss.body
    );
    assert_eq!(
        after_hash_miss.body["user"]["email"].as_str(),
        Some(email.as_str()),
        "{}",
        after_hash_miss.body
    );
    let after_hash_miss_ws = after_hash_miss.body["workspaces"]
        .as_array()
        .expect("hash-miss /me workspaces");
    assert!(
        !after_hash_miss_ws.is_empty(),
        "hash miss must leave the original session fully authenticated: {}",
        after_hash_miss.body
    );

    // 2. Opaque refresh in the access cookie cannot authenticate /me.
    let mut refresh_as_access = CookieJar::default();
    refresh_as_access.insert(ACCESS_COOKIE, refresh.clone());
    assert!(
        refresh_as_access.refresh().is_none(),
        "this request must not send fleet_refresh"
    );
    assert_eq!(refresh_as_access.access(), Some(refresh.as_str()));

    let as_access = call(&app, "GET", "/api/auth/me", Some(&refresh_as_access), None).await;
    assert_me_not_authenticated(&as_access, "GET /me with fleet_access=<refresh opaque>");
    // Present non-JWT fleet_access fails decode. Missing cookie is
    // "not authenticated"; session revoke / reuse are other cards.
    assert_eq!(
        as_access.error(),
        "invalid or expired token",
        "opaque refresh as fleet_access must fail JWT decode: {}",
        as_access.body
    );
    assert_ne!(
        as_access.error(),
        "session is no longer valid",
        "opaque refresh as fleet_access is not a session revoke (logout owns that): {}",
        as_access.body
    );
    let as_access_msg = as_access.error().to_ascii_lowercase();
    assert!(
        !as_access_msg.contains("reuse") && !as_access_msg.contains("family"),
        "opaque refresh as fleet_access is not token reuse: {:?}",
        as_access.error()
    );
    assert_no_auth_cookies(&as_access, "refresh-as-access /me 401");

    // 3. Valid access JWT is not a substitute for the refresh cookie.
    let mut access_only = CookieJar::default();
    access_only.insert(ACCESS_COOKIE, access.clone());
    assert!(
        access_only.refresh().is_none(),
        "this request must not send fleet_refresh"
    );

    let access_only_refresh =
        call(&app, "POST", "/api/auth/refresh", Some(&access_only), None).await;
    assert_eq!(
        access_only_refresh.status,
        StatusCode::UNAUTHORIZED,
        "refresh with only fleet_access must be 401: {}",
        access_only_refresh.body
    );
    assert_eq!(
        access_only_refresh.error(),
        "missing refresh token",
        "access-only refresh must match refresh_without_cookie_is_401: {}",
        access_only_refresh.body
    );
    assert_no_auth_cookies(&access_only_refresh, "access-only refresh 401");

    // 4. Control: original cookies still work — this is not "every auth call is 401".
    let control_me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        control_me.status,
        StatusCode::OK,
        "original cookies must still GET /me 200: {}",
        control_me.body
    );
    assert_eq!(
        control_me.body["user"]["email"].as_str(),
        Some(email.as_str()),
        "{}",
        control_me.body
    );
    let control_workspaces = control_me.body["workspaces"]
        .as_array()
        .expect("control /me workspaces");
    assert!(
        !control_workspaces.is_empty(),
        "control /me must include workspaces: {}",
        control_me.body
    );

    let control_refresh = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(
        control_refresh.status,
        StatusCode::OK,
        "original cookies must still POST /refresh 200: {}",
        control_refresh.body
    );
    assert_eq!(
        control_refresh.body["message"].as_str(),
        Some("token refreshed"),
        "{}",
        control_refresh.body
    );
    let new_refresh = control_refresh
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("control refresh must Set-Cookie {REFRESH_COOKIE}"));
    assert_ne!(
        new_refresh.value, refresh,
        "control refresh must rotate the real refresh cookie"
    );
}

/// Docs §3.1/§3.4 / fleet-auth README: cookie BFF is `fleet_access` +
/// `fleet_refresh` cookies. `require_user` reads only ACCESS_COOKIE then
/// `is_session_active`. A stolen access JWT in `Authorization: Bearer`
/// (no Cookie) is not a session. Dual-auth that decodes Bearer without
/// the cookie (or without `is_session_active`) would keep a logged-out
/// JWT valid and change the cookie CSRF model. Keep
/// `unauthenticated_cookie_routes_are_401` and
/// `access_jwt_rejected_as_refresh`.
async fn call_bearer(
    app: &axum::Router,
    method: &str,
    uri: &str,
    bearer: &str,
    body: Option<serde_json::Value>,
) -> HttpOut {
    let auth = format!("Bearer {bearer}");
    call_with_headers(
        app,
        method,
        uri,
        None,
        &[("Authorization", auth.as_str())],
        body,
    )
    .await
}

#[tokio::test]
async fn bearer_access_not_cookie_cannot_authenticate_me_or_workspaces() {
    let app = app().await;
    let (email, jar, login_out) = signup_and_login(&app).await;
    let access = jar
        .access()
        .expect("login must set fleet_access")
        .to_string();
    assert_eq!(
        access.split('.').count(),
        3,
        "fleet_access must be a JWT, got {access:?}"
    );
    assert_access_jwt_still_decodes(&access);
    let ws = workspace_id(&login_out);

    // 4. Control first: Cookie (no Authorization) still GET /me 200.
    let control_me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        control_me.status,
        StatusCode::OK,
        "Cookie GET /me must still 200 before logout: {}",
        control_me.body
    );
    assert_eq!(
        control_me.body["user"]["email"].as_str(),
        Some(email.as_str()),
        "Cookie GET /me must include the logged-in user: {}",
        control_me.body
    );
    let control_workspaces = control_me.body["workspaces"]
        .as_array()
        .expect("control /me workspaces");
    assert!(
        control_workspaces
            .iter()
            .any(|w| w["id"].as_str() == Some(ws.as_str())),
        "control /me must include the default workspace: {}",
        control_me.body
    );

    // 1. Bearer <access JWT>, no Cookie, on /me and /workspaces.
    let bearer_me = call_bearer(&app, "GET", "/api/auth/me", &access, None).await;
    assert_me_not_authenticated(
        &bearer_me,
        "GET /me with Authorization: Bearer <fleet_access JWT> and no Cookie",
    );
    assert_eq!(
        bearer_me.error(),
        "not authenticated",
        "Bearer without Cookie must be missing-cookie 401, not a decoded session: {}",
        bearer_me.body
    );
    assert_ne!(
        bearer_me.status,
        StatusCode::OK,
        "Bearer /me must not 200 as if the cookie were present: {}",
        bearer_me.body
    );
    assert_no_auth_cookies(&bearer_me, "Bearer /me 401");
    assert!(
        !bearer_me.raw.contains(&email) && !bearer_me.raw.contains(&ws),
        "Bearer /me must not leak user/workspace: {}",
        bearer_me.body
    );

    let bearer_ws = call_bearer(&app, "GET", "/api/workspaces", &access, None).await;
    assert_eq!(
        bearer_ws.status,
        StatusCode::UNAUTHORIZED,
        "GET /workspaces with Authorization: Bearer <fleet_access JWT> and no Cookie must be 401: {}",
        bearer_ws.body
    );
    assert_eq!(
        bearer_ws.error(),
        "not authenticated",
        "Bearer /workspaces must be not authenticated: {}",
        bearer_ws.body
    );
    assert!(
        bearer_ws.body.get("user").is_none(),
        "Bearer /workspaces must not include user: {}",
        bearer_ws.body
    );
    assert!(
        bearer_ws.body.get("workspaces").is_none(),
        "Bearer /workspaces must not include workspaces: {}",
        bearer_ws.body
    );
    assert_ne!(
        bearer_ws.status,
        StatusCode::OK,
        "Bearer /workspaces must not 200 as if the cookie were present: {}",
        bearer_ws.body
    );
    assert_no_auth_cookies(&bearer_ws, "Bearer /workspaces 401");
    assert!(
        !bearer_ws.raw.contains(&email) && !bearer_ws.raw.contains(&ws),
        "Bearer /workspaces must not leak user/workspace: {}",
        bearer_ws.body
    );

    // 3. Logout revokes the sid. The same Bearer JWT (no Cookie) is still
    // 401 — a decode-only Bearer path would 200 after logout.
    let logout = call(&app, "POST", "/api/auth/logout", Some(&jar), None).await;
    assert_eq!(logout.status, StatusCode::OK, "logout: {}", logout.body);
    assert_eq!(logout.body["message"].as_str(), Some("logged out"));
    assert_access_jwt_still_decodes(&access);

    let after_logout = call_bearer(&app, "GET", "/api/auth/me", &access, None).await;
    assert_me_not_authenticated(
        &after_logout,
        "GET /me with the same Bearer JWT after cookie logout",
    );
    assert_ne!(
        after_logout.status,
        StatusCode::OK,
        "decode-only Bearer after logout would 200; must stay 401: {}",
        after_logout.body
    );
    assert!(
        after_logout.body.get("user").is_none() && after_logout.body.get("workspaces").is_none(),
        "Bearer /me after logout must not include user/workspaces: {}",
        after_logout.body
    );
}

/// Docs §3.2/§3.4: `fleet_access` carries `sub` (user id) + `sid`
/// (`auth_sessions.id`). `require_user` decodes then
/// `is_session_active(sid, user_id)` (`AND user_id=$2`). A JWT with A's
/// sub+email and B's sid must 401 — looking up the session by sid only
/// would impersonate A while B's row stayed live.
#[tokio::test]
async fn access_jwt_sid_user_mismatch_is_401() {
    let app = app().await;
    let (a_email, a_jar, _) = signup_and_login(&app).await;
    let (b_email, b_jar, _) = signup_and_login(&app).await;
    assert_ne!(a_email, b_email, "A and B must be distinct accounts");

    let me_a = call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await;
    let me_b = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_eq!(me_a.status, StatusCode::OK, "A /me snapshot: {}", me_a.body);
    assert_eq!(me_b.status, StatusCode::OK, "B /me snapshot: {}", me_b.body);

    let a_uid = me_a.body["user"]["id"]
        .as_str()
        .expect("A /me user.id")
        .to_string();
    let b_uid = me_b.body["user"]["id"]
        .as_str()
        .expect("B /me user.id")
        .to_string();
    let a_ws = listed_workspace_ids(&me_a)
        .first()
        .copied()
        .expect("A /me workspace id")
        .to_string();
    let b_ws = listed_workspace_ids(&me_b)
        .first()
        .copied()
        .expect("B /me workspace id")
        .to_string();
    assert_ne!(a_uid, b_uid, "A and B must have distinct user ids");
    assert_ne!(a_ws, b_ws, "A and B must have distinct workspace ids");
    assert_eq!(
        me_a.body["user"]["email"].as_str(),
        Some(a_email.as_str()),
        "{}",
        me_a.body
    );
    assert_eq!(
        me_b.body["user"]["email"].as_str(),
        Some(b_email.as_str()),
        "{}",
        me_b.body
    );

    let a_access = a_jar
        .access()
        .expect("A login must set fleet_access")
        .to_string();
    let b_access = b_jar
        .access()
        .expect("B login must set fleet_access")
        .to_string();
    let a_claims = decode_access_token(&test_jwt_secret(), &a_access)
        .unwrap_or_else(|e| panic!("A fleet_access must decode with fixture secret: {e}"));
    let b_claims = decode_access_token(&test_jwt_secret(), &b_access)
        .unwrap_or_else(|e| panic!("B fleet_access must decode with fixture secret: {e}"));
    assert_eq!(a_claims.sub, a_uid, "A JWT sub must be A's user id");
    assert_eq!(b_claims.sub, b_uid, "B JWT sub must be B's user id");
    assert_eq!(a_claims.email, a_email);
    assert_eq!(b_claims.email, b_email);
    assert_ne!(
        a_claims.sid, b_claims.sid,
        "A and B must have distinct session ids"
    );

    let a_on_b = reencode_access_claims(&a_claims.sub, &a_claims.email, &b_claims.sid);
    let mut a_on_b_jar = CookieJar::default();
    a_on_b_jar.insert(ACCESS_COOKIE, a_on_b);
    assert!(
        a_on_b_jar.refresh().is_none(),
        "A-sub + B-sid request must not send fleet_refresh"
    );
    let a_on_b_me = call(&app, "GET", "/api/auth/me", Some(&a_on_b_jar), None).await;
    assert_access_jwt_sid_user_mismatch_401(
        &a_on_b_me,
        "GET /me with A's sub+email and B's sid (no refresh)",
        &a_email,
        &a_ws,
        &b_email,
        &b_ws,
    );

    let b_on_a = reencode_access_claims(&b_claims.sub, &b_claims.email, &a_claims.sid);
    let mut b_on_a_jar = CookieJar::default();
    b_on_a_jar.insert(ACCESS_COOKIE, b_on_a);
    b_on_a_jar.insert(
        REFRESH_COOKIE,
        a_jar
            .refresh()
            .expect("A login must set fleet_refresh")
            .to_string(),
    );
    let b_on_a_me = call(&app, "GET", "/api/auth/me", Some(&b_on_a_jar), None).await;
    assert_access_jwt_sid_user_mismatch_401(
        &b_on_a_me,
        "GET /me with B's sub+email and A's sid (A's refresh)",
        &a_email,
        &a_ws,
        &b_email,
        &b_ws,
    );

    let still_a = call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await;
    assert_me_still_owner(&still_a, &a_email, &a_uid, &a_ws, "A original cookies");
    let still_b = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_me_still_owner(&still_b, &b_email, &b_uid, &b_ws, "B original cookies");

    let matching = reencode_access_claims(&a_claims.sub, &a_claims.email, &a_claims.sid);
    let mut matching_jar = CookieJar::default();
    matching_jar.insert(ACCESS_COOKIE, matching);
    let control = call(&app, "GET", "/api/auth/me", Some(&matching_jar), None).await;
    assert_me_still_owner(
        &control,
        &a_email,
        &a_uid,
        &a_ws,
        "re-encoded matching sub+sid+email for A",
    );
}

/// Docs §3.4 / fleet-auth README: `fleet_access` is a short-lived JWT
/// (`JWT_ACCESS_TTL_SECS`, default 900). `require_user` decodes via
/// `decode_access_token` (`Validation::default`, 60s leeway) then
/// `is_session_active`. `expired_token_fails` is a tokens.rs unit — it
/// never drives GET /me. A handler that skipped decode, set
/// `validate_exp=false`, or treated a live session row as enough would
/// keep a stolen/old access cookie valid until the refresh family expires.
#[tokio::test]
async fn expired_access_jwt_me_is_401() {
    let app = app().await;
    let (email, jar, _) = signup_and_login(&app).await;
    let access = jar
        .access()
        .expect("login must set fleet_access")
        .to_string();
    let refresh = jar
        .refresh()
        .expect("login must set fleet_refresh")
        .to_string();
    assert_eq!(
        access.split('.').count(),
        3,
        "fleet_access must be a JWT, got {access:?}"
    );

    let claims = decode_access_token(&test_jwt_secret(), &access)
        .unwrap_or_else(|e| panic!("login fleet_access must decode with fixture secret: {e}"));
    assert_eq!(
        claims.email, email,
        "JWT email must match the logged-in user"
    );
    assert!(
        !claims.sub.is_empty() && !claims.sid.is_empty(),
        "access JWT must carry sub+sid: {claims:?}"
    );

    // Control (before refresh consumes the family): the original unexpired
    // access JWT still GET /me 200. Snapshot here so this is not a
    // tautology that every /me is 401 after we mint an expired cookie.
    let control_me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        control_me.status,
        StatusCode::OK,
        "original unexpired access must GET /me 200 before refresh: {}",
        control_me.body
    );
    assert_eq!(
        control_me.body["user"]["email"].as_str(),
        Some(email.as_str()),
        "control /me: {}",
        control_me.body
    );
    let control_workspaces = control_me.body["workspaces"]
        .as_array()
        .expect("control /me workspaces");
    assert!(
        !control_workspaces.is_empty(),
        "control /me must include workspaces: {}",
        control_me.body
    );

    // Leeway is 60s — ttl=0 is not enough. Same sub/email/sid, exp/iat
    // at least 120s in the past so Validation::default rejects expiry.
    let now = Utc::now();
    let expired_claims = AccessClaims {
        sub: claims.sub.clone(),
        email: claims.email.clone(),
        sid: claims.sid.clone(),
        exp: (now - chrono::Duration::seconds(120)).timestamp(),
        iat: (now - chrono::Duration::seconds(180)).timestamp(),
    };
    let expired = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &expired_claims,
        &jsonwebtoken::EncodingKey::from_secret(test_jwt_secret().as_bytes()),
    )
    .expect("encode expired AccessClaims");
    assert_ne!(
        expired, access,
        "expired re-encode must not equal the live access JWT"
    );
    decode_access_token(&test_jwt_secret(), &expired)
        .expect_err("re-encoded exp must be past Validation::default 60s leeway");

    // Refresh cookie may be present — a live session row must not keep
    // the expired access cookie valid.
    let mut expired_jar = CookieJar::default();
    expired_jar.insert(ACCESS_COOKIE, expired.clone());
    expired_jar.insert(REFRESH_COOKIE, refresh.clone());
    let expired_me = call(&app, "GET", "/api/auth/me", Some(&expired_jar), None).await;
    assert_eq!(
        expired_me.status,
        StatusCode::UNAUTHORIZED,
        "expired fleet_access must GET /me 401, not 200: {}",
        expired_me.body
    );
    assert_eq!(
        expired_me.error(),
        "invalid or expired token",
        "expired access must fail JWT decode (not session revoke): {}",
        expired_me.body
    );
    assert_ne!(
        expired_me.error(),
        "session is no longer valid",
        "session row is still live — expiry is not logout/revoke: {}",
        expired_me.body
    );
    assert_ne!(
        expired_me.error(),
        "not authenticated",
        "expired /me presented fleet_access: {}",
        expired_me.body
    );
    let expired_msg = expired_me.error().to_ascii_lowercase();
    assert!(
        !expired_msg.contains("reuse") && !expired_msg.contains("family"),
        "expired access is not refresh reuse (refresh_reuse_revokes_family owns that): {:?}",
        expired_me.error()
    );
    assert!(
        expired_me.body.get("user").is_none()
            && expired_me.body.get("workspaces").is_none()
            && expired_me.body.get("default_workspace").is_none(),
        "expired /me 401 must not include user/workspaces: {}",
        expired_me.body
    );
    assert_no_auth_cookies(&expired_me, "expired access GET /me 401");

    // Live refresh still rotates — this is not "every /me is 401 after expiry".
    let mut refresh_jar = CookieJar::default();
    refresh_jar.insert(REFRESH_COOKIE, refresh.clone());
    let rotated = call(&app, "POST", "/api/auth/refresh", Some(&refresh_jar), None).await;
    assert_eq!(
        rotated.status,
        StatusCode::OK,
        "original unexpired fleet_refresh must POST /refresh 200: {}",
        rotated.body
    );
    assert_eq!(
        rotated.body["message"].as_str(),
        Some("token refreshed"),
        "{}",
        rotated.body
    );
    let new_access = rotated
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("refresh must Set-Cookie {ACCESS_COOKIE}"));
    assert_ne!(
        new_access.value, access,
        "refresh must Set-Cookie a new access JWT"
    );
    assert_ne!(
        new_access.value, expired,
        "refresh must not reissue the expired access JWT"
    );
    rotated.absorb_into(&mut refresh_jar);
    assert_eq!(
        refresh_jar.access(),
        Some(new_access.value.as_str()),
        "jar must pick up the new access cookie"
    );

    let me_after = call(&app, "GET", "/api/auth/me", Some(&refresh_jar), None).await;
    assert_eq!(
        me_after.status,
        StatusCode::OK,
        "GET /me with rotated cookies must be 200: {}",
        me_after.body
    );
    assert_eq!(
        me_after.body["user"]["email"].as_str(),
        Some(email.as_str()),
        "rotated /me: {}",
        me_after.body
    );
    let after_workspaces = me_after.body["workspaces"]
        .as_array()
        .expect("rotated /me workspaces");
    assert!(
        !after_workspaces.is_empty(),
        "rotated /me must include workspaces: {}",
        me_after.body
    );
}

fn reencode_access_claims(sub: &str, email: &str, sid: &str) -> String {
    let user_id = Uuid::parse_str(sub).unwrap_or_else(|e| panic!("JWT sub must be a uuid: {e}"));
    let session_id = Uuid::parse_str(sid).unwrap_or_else(|e| panic!("JWT sid must be a uuid: {e}"));
    issue_access_token(
        &test_jwt_secret(),
        user_id,
        email,
        session_id,
        Duration::from_secs(900),
    )
    .unwrap_or_else(|e| panic!("re-encode AccessClaims: {e}"))
}

fn assert_access_jwt_sid_user_mismatch_401(
    out: &HttpOut,
    when: &str,
    a_email: &str,
    a_ws: &str,
    b_email: &str,
    b_ws: &str,
) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{when} must be 401, not 200: {}",
        out.body
    );
    assert!(
        out.error() == "session is no longer valid" || out.error() == "not authenticated",
        "{when} must be session is no longer valid / not authenticated, got {:?}: {}",
        out.error(),
        out.body
    );
    assert!(
        out.body.get("user").is_none()
            && out.body.get("workspaces").is_none()
            && out.body.get("default_workspace").is_none(),
        "{when} must not include user/workspaces: {}",
        out.body
    );
    let blob = out.body.to_string();
    for (label, needle) in [
        ("A's email", a_email),
        ("A's workspace id", a_ws),
        ("B's email", b_email),
        ("B's workspace id", b_ws),
    ] {
        assert!(
            !blob.contains(needle),
            "{when} must not include {label} ({needle}): {}",
            out.body
        );
    }
}

fn assert_me_still_owner(out: &HttpOut, email: &str, uid: &str, ws: &str, when: &str) {
    assert_eq!(
        out.status,
        StatusCode::OK,
        "{when} must still GET /me 200: {}",
        out.body
    );
    assert_eq!(
        out.body["user"]["email"].as_str(),
        Some(email),
        "{when}: {}",
        out.body
    );
    assert_eq!(
        out.body["user"]["id"].as_str(),
        Some(uid),
        "{when}: {}",
        out.body
    );
    assert!(
        listed_workspace_ids(out).contains(&ws),
        "{when} must still include workspace {ws}: {}",
        out.body
    );
}

fn assert_logout_expires_cookie(cookie: &ParsedSetCookie) {
    assert_eq!(
        cookie.max_age(),
        Some(0),
        "logout must expire cookie: {}",
        cookie.raw
    );
    assert!(
        cookie.value.is_empty(),
        "logout Set-Cookie value must be empty: {}",
        cookie.raw
    );
}

/// Signature + expiry still hold. A 401 after logout is session revoke, not
/// a dead JWT.
fn assert_access_jwt_still_decodes(token: &str) {
    #[derive(Debug, serde::Deserialize)]
    struct AccessClaimsLite {
        sub: String,
        sid: String,
        exp: i64,
    }
    let data = jsonwebtoken::decode::<AccessClaimsLite>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(test_jwt_secret().as_bytes()),
        &jsonwebtoken::Validation::default(),
    )
    .expect("replayed access JWT is still a valid token");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs() as i64;
    assert!(
        data.claims.exp > now,
        "access JWT must still be unexpired (lock is session revoke, not JWT expiry): exp={} now={}",
        data.claims.exp,
        now
    );
    assert!(
        !data.claims.sub.is_empty() && !data.claims.sid.is_empty(),
        "access JWT must still carry sub+sid: {:?}",
        data.claims
    );
}

/// Product POST /logout must expire cookies and revoke the session so a
/// replay of the pre-logout cookie on an authenticated route is 401.
#[tokio::test]
async fn logout_clears_session_cookie_and_replay_is_401() {
    let app = app().await;
    let (email, jar, _) = signup_and_login(&app).await;

    let access = jar
        .access()
        .expect("login must set access cookie")
        .to_string();
    let refresh = jar
        .refresh()
        .expect("login must set refresh cookie")
        .to_string();
    assert!(!access.is_empty(), "login access cookie must be non-empty");
    assert!(
        !refresh.is_empty(),
        "login refresh cookie must be non-empty"
    );
    assert_access_jwt_still_decodes(&access);

    let before = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        before.status,
        StatusCode::OK,
        "pre-logout me: {}",
        before.body
    );
    assert_eq!(
        before.body["user"]["email"].as_str(),
        Some(email.as_str()),
        "{}",
        before.body
    );

    let out = call(&app, "POST", "/api/auth/logout", Some(&jar), None).await;
    assert_eq!(out.status, StatusCode::OK, "logout: {}", out.body);
    assert_eq!(out.body["message"].as_str(), Some("logged out"));

    let access_clear = out
        .cookie(ACCESS_COOKIE)
        .expect("logout must send access Set-Cookie");
    let refresh_clear = out
        .cookie(REFRESH_COOKIE)
        .expect("logout must send refresh Set-Cookie");
    assert_logout_expires_cookie(&access_clear);
    assert_logout_expires_cookie(&refresh_clear);

    // Replay the pre-logout Cookie header. Do not absorb logout Set-Cookie —
    // that would 401 from a missing cookie even if the session stayed live.
    let mut replay_jar = CookieJar::default();
    replay_jar.insert(ACCESS_COOKIE, access.clone());
    replay_jar.insert(REFRESH_COOKIE, refresh.clone());
    assert_eq!(
        replay_jar.access(),
        Some(access.as_str()),
        "replay Cookie must still carry the pre-logout access JWT"
    );
    assert_eq!(
        replay_jar.refresh(),
        Some(refresh.as_str()),
        "replay Cookie must still carry the pre-logout refresh token"
    );
    assert_access_jwt_still_decodes(&access);

    let replay = call(&app, "GET", "/api/auth/me", Some(&replay_jar), None).await;
    assert_eq!(
        replay.status,
        StatusCode::UNAUTHORIZED,
        "replayed cookie after logout must be 401: {}",
        replay.body
    );
    assert_eq!(
        replay.error(),
        "session is no longer valid",
        "401 body should explain the session is no longer valid: {}",
        replay.body
    );
    // Missing cookie would be "not authenticated"; this lock is session revoke.
    assert_ne!(
        replay.error(),
        "not authenticated",
        "replay must present the cookie; 401 is session revoke, not a missing cookie: {}",
        replay.body
    );

    let refresh_after = call(&app, "POST", "/api/auth/refresh", Some(&replay_jar), None).await;
    assert_eq!(
        refresh_after.status,
        StatusCode::UNAUTHORIZED,
        "revoked refresh must not rotate: {}",
        refresh_after.body
    );
}

/// Docs §3.4: POST /logout revokes the session and clears cookies even when
/// only `fleet_access` is sent. A dropped/blocked refresh cookie must not
/// leave `auth_sessions` live — the stolen refresh must not rotate, and
/// replaying the pre-logout access JWT must be 401.
/// Revoke this sid only (not revoke_family): a second concurrent login
/// stays valid. Do not weaken logout_other_session_survives.
#[tokio::test]
async fn logout_without_refresh_revokes_sid() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (a_out, a_jar) = login(&app, &email, TEST_PASSWORD).await;
    let (b_out, b_jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(a_out.status, StatusCode::OK, "login A: {}", a_out.body);
    assert_eq!(b_out.status, StatusCode::OK, "login B: {}", b_out.body);
    assert_eq!(a_out.body["message"].as_str(), Some("logged in"));

    let a_access = a_jar
        .access()
        .expect("login A must set fleet_access")
        .to_string();
    let a_refresh = a_jar
        .refresh()
        .expect("login A must set fleet_refresh")
        .to_string();
    let b_access = b_jar
        .access()
        .expect("login B must set fleet_access")
        .to_string();
    let b_refresh = b_jar
        .refresh()
        .expect("login B must set fleet_refresh")
        .to_string();
    assert!(!a_access.is_empty() && !a_refresh.is_empty());
    assert!(!b_access.is_empty() && !b_refresh.is_empty());
    assert_ne!(
        a_access, b_access,
        "concurrent logins must issue distinct fleet_access cookies"
    );
    assert_ne!(
        a_refresh, b_refresh,
        "concurrent logins must issue distinct fleet_refresh cookies"
    );
    assert_access_jwt_still_decodes(&a_access);

    let me_a = call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await;
    let me_b = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_eq!(
        me_a.status,
        StatusCode::OK,
        "pre-logout A /me with both cookies: {}",
        me_a.body
    );
    assert_eq!(
        me_b.status,
        StatusCode::OK,
        "pre-logout B /me: {}",
        me_b.body
    );
    let uid = user_id(&me_a);
    let ws = me_a.body["workspaces"]
        .as_array()
        .expect("A /me workspaces")
        .iter()
        .filter_map(|w| w["id"].as_str())
        .next()
        .expect("A /me must list a workspace")
        .to_string();
    assert_me_still_owner(&me_a, &email, &uid, &ws, "pre-logout A /me");
    assert_me_still_owner(&me_b, &email, &uid, &ws, "pre-logout B /me");

    // Drive oneshot with fleet_access only — do not send fleet_refresh.
    let mut access_only = CookieJar::default();
    access_only.insert(ACCESS_COOKIE, a_access.clone());
    let access_only_header = access_only
        .cookie_header()
        .expect("access-only jar must serialize Cookie");
    assert!(
        access_only_header.contains(&format!("{ACCESS_COOKIE}=")),
        "logout Cookie must send {ACCESS_COOKIE}: {access_only_header}"
    );
    assert!(
        !access_only_header.contains(REFRESH_COOKIE),
        "logout Cookie must omit {REFRESH_COOKIE}: {access_only_header}"
    );
    assert!(
        access_only.refresh().is_none(),
        "access-only jar must not carry {REFRESH_COOKIE}"
    );

    let logout = call(&app, "POST", "/api/auth/logout", Some(&access_only), None).await;
    assert_eq!(
        logout.status,
        StatusCode::OK,
        "access-only logout: {}",
        logout.body
    );
    assert_eq!(logout.body["message"].as_str(), Some("logged out"));
    let access_clear = logout
        .cookie(ACCESS_COOKIE)
        .expect("access-only logout must send access Set-Cookie");
    let refresh_clear = logout
        .cookie(REFRESH_COOKIE)
        .expect("access-only logout must send refresh Set-Cookie");
    assert_logout_expires_cookie(&access_clear);
    assert_logout_expires_cookie(&refresh_clear);

    // Replay the pre-logout access JWT. Do not absorb logout Set-Cookie —
    // that would 401 from a missing cookie even if the session stayed live.
    let mut a_replay = CookieJar::default();
    a_replay.insert(ACCESS_COOKIE, a_access.clone());
    assert_eq!(
        a_replay.access(),
        Some(a_access.as_str()),
        "replay Cookie must still carry A's pre-logout access JWT"
    );
    assert!(
        a_replay.refresh().is_none(),
        "replay GET /me uses the pre-logout access JWT only"
    );
    assert_access_jwt_still_decodes(&a_access);

    let a_me_replay = call(&app, "GET", "/api/auth/me", Some(&a_replay), None).await;
    assert_eq!(
        a_me_replay.status,
        StatusCode::UNAUTHORIZED,
        "A's replayed access JWT after access-only logout must be 401 (session revoked), not 200: {}",
        a_me_replay.body
    );
    assert_eq!(
        a_me_replay.error(),
        "session is no longer valid",
        "401 body should explain the session is no longer valid: {}",
        a_me_replay.body
    );
    assert_ne!(
        a_me_replay.error(),
        "not authenticated",
        "A's replay must present the access JWT; 401 is session revoke, not a missing cookie: {}",
        a_me_replay.body
    );
    assert!(
        a_me_replay.body.get("user").is_none()
            && a_me_replay.body.get("workspaces").is_none()
            && a_me_replay.body.get("default_workspace").is_none(),
        "A's access-only logout replay /me must not include user/workspaces: {}",
        a_me_replay.body
    );

    // Original fleet_refresh was never sent on logout. It must not rotate.
    let mut stolen_refresh = CookieJar::default();
    stolen_refresh.insert(REFRESH_COOKIE, a_refresh.clone());
    assert_eq!(
        stolen_refresh.refresh(),
        Some(a_refresh.as_str()),
        "refresh request must send the original fleet_refresh"
    );
    let a_refresh_after = call(
        &app,
        "POST",
        "/api/auth/refresh",
        Some(&stolen_refresh),
        None,
    )
    .await;
    assert_eq!(
        a_refresh_after.status,
        StatusCode::UNAUTHORIZED,
        "original fleet_refresh (never sent on logout) must not rotate: {}",
        a_refresh_after.body
    );
    assert_no_live_auth_set_cookie(
        &a_refresh_after,
        "POST /refresh after access-only logout must not Set-Cookie a new family",
    );

    // B is a different family. Access-only logout must revoke A's sid only.
    let me_b_after = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_me_still_owner(
        &me_b_after,
        &email,
        &uid,
        &ws,
        "B GET /me after A's access-only logout",
    );
    assert_eq!(
        b_jar.access(),
        Some(b_access.as_str()),
        "B's jar must still carry the pre-logout fleet_access"
    );
    assert_eq!(
        b_jar.refresh(),
        Some(b_refresh.as_str()),
        "B's jar must still carry the pre-logout fleet_refresh"
    );

    // Control: logout with both cookies still 200 and replay 401
    // (do not weaken list-auth-logout-clears-cookie). Login still 200
    // so this is not a tautology that every logout 401s /me before logout.
    let (c_out, c_jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(c_out.status, StatusCode::OK, "login C: {}", c_out.body);
    assert_eq!(c_out.body["message"].as_str(), Some("logged in"));
    let c_access = c_jar
        .access()
        .expect("login C must set fleet_access")
        .to_string();
    let c_refresh = c_jar
        .refresh()
        .expect("login C must set fleet_refresh")
        .to_string();
    assert!(!c_access.is_empty() && !c_refresh.is_empty());
    let me_c = call(&app, "GET", "/api/auth/me", Some(&c_jar), None).await;
    assert_me_still_owner(&me_c, &email, &uid, &ws, "pre-logout C /me");

    let both_logout = call(&app, "POST", "/api/auth/logout", Some(&c_jar), None).await;
    assert_eq!(
        both_logout.status,
        StatusCode::OK,
        "both-cookie logout C: {}",
        both_logout.body
    );
    assert_eq!(both_logout.body["message"].as_str(), Some("logged out"));
    let both_access_clear = both_logout
        .cookie(ACCESS_COOKIE)
        .expect("both-cookie logout must send access Set-Cookie");
    let both_refresh_clear = both_logout
        .cookie(REFRESH_COOKIE)
        .expect("both-cookie logout must send refresh Set-Cookie");
    assert_logout_expires_cookie(&both_access_clear);
    assert_logout_expires_cookie(&both_refresh_clear);

    let mut c_replay = CookieJar::default();
    c_replay.insert(ACCESS_COOKIE, c_access.clone());
    c_replay.insert(REFRESH_COOKIE, c_refresh.clone());
    let c_me_replay = call(&app, "GET", "/api/auth/me", Some(&c_replay), None).await;
    assert_eq!(
        c_me_replay.status,
        StatusCode::UNAUTHORIZED,
        "C's replayed cookies after both-cookie logout must be 401: {}",
        c_me_replay.body
    );
    assert_eq!(
        c_me_replay.error(),
        "session is no longer valid",
        "both-cookie logout replay: {}",
        c_me_replay.body
    );
    assert!(
        c_me_replay.body.get("user").is_none()
            && c_me_replay.body.get("workspaces").is_none()
            && c_me_replay.body.get("default_workspace").is_none(),
        "C's both-cookie logout replay /me must not include user/workspaces: {}",
        c_me_replay.body
    );
}

/// A present-but-mismatched `fleet_refresh` (empty, stale after rotation,
/// hash miss) must not skip access-JWT `revoke_by_id`. Cookies still clear.
/// Do not `revoke_family` — B stays valid.
#[tokio::test]
async fn logout_stale_refresh_still_revokes_access_sid() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (a_out, a_jar) = login(&app, &email, TEST_PASSWORD).await;
    let (b_out, b_jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(a_out.status, StatusCode::OK, "login A: {}", a_out.body);
    assert_eq!(b_out.status, StatusCode::OK, "login B: {}", b_out.body);

    let a_access = a_jar
        .access()
        .expect("login A must set fleet_access")
        .to_string();
    let a_refresh = a_jar
        .refresh()
        .expect("login A must set fleet_refresh")
        .to_string();
    let b_access = b_jar
        .access()
        .expect("login B must set fleet_access")
        .to_string();
    let b_refresh = b_jar
        .refresh()
        .expect("login B must set fleet_refresh")
        .to_string();
    assert_access_jwt_still_decodes(&a_access);

    let uid = user_id(&call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await);
    let ws = workspace_id(&a_out);

    let mut stale = CookieJar::default();
    stale.insert(ACCESS_COOKIE, a_access.clone());
    stale.insert(REFRESH_COOKIE, "stale-or-rotated-refresh-token");
    let logout = call(&app, "POST", "/api/auth/logout", Some(&stale), None).await;
    assert_eq!(
        logout.status,
        StatusCode::OK,
        "stale-refresh logout: {}",
        logout.body
    );
    assert_eq!(logout.body["message"].as_str(), Some("logged out"));
    assert_logout_expires_cookie(
        &logout
            .cookie(ACCESS_COOKIE)
            .expect("stale-refresh logout must send access Set-Cookie"),
    );
    assert_logout_expires_cookie(
        &logout
            .cookie(REFRESH_COOKIE)
            .expect("stale-refresh logout must send refresh Set-Cookie"),
    );

    let mut a_replay = CookieJar::default();
    a_replay.insert(ACCESS_COOKIE, a_access.clone());
    assert_access_jwt_still_decodes(&a_access);
    let a_me_replay = call(&app, "GET", "/api/auth/me", Some(&a_replay), None).await;
    assert_eq!(
        a_me_replay.status,
        StatusCode::UNAUTHORIZED,
        "A's access JWT after stale-refresh logout must be 401: {}",
        a_me_replay.body
    );
    assert_eq!(
        a_me_replay.error(),
        "session is no longer valid",
        "stale-refresh logout must revoke the access sid: {}",
        a_me_replay.body
    );

    let mut stolen_refresh = CookieJar::default();
    stolen_refresh.insert(REFRESH_COOKIE, a_refresh.clone());
    let a_refresh_after = call(
        &app,
        "POST",
        "/api/auth/refresh",
        Some(&stolen_refresh),
        None,
    )
    .await;
    assert_eq!(
        a_refresh_after.status,
        StatusCode::UNAUTHORIZED,
        "A's original refresh after stale-refresh logout must not rotate: {}",
        a_refresh_after.body
    );
    assert_no_live_auth_set_cookie(
        &a_refresh_after,
        "POST /refresh after stale-refresh logout must not Set-Cookie a new family",
    );

    let me_b_after = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_me_still_owner(
        &me_b_after,
        &email,
        &uid,
        &ws,
        "B GET /me after A's stale-refresh logout",
    );
    assert_eq!(b_jar.access(), Some(b_access.as_str()));
    assert_eq!(b_jar.refresh(), Some(b_refresh.as_str()));
}

/// Expired `fleet_access` must still revoke its sid on logout. Gated
/// `decode_access_token` rejects exp; logout-only decode skips it.
/// Do not `revoke_family`.
#[tokio::test]
async fn logout_expired_access_still_revokes_sid() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (a_out, a_jar) = login(&app, &email, TEST_PASSWORD).await;
    let (b_out, b_jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(a_out.status, StatusCode::OK, "login A: {}", a_out.body);
    assert_eq!(b_out.status, StatusCode::OK, "login B: {}", b_out.body);

    let a_access = a_jar
        .access()
        .expect("login A must set fleet_access")
        .to_string();
    let a_refresh = a_jar
        .refresh()
        .expect("login A must set fleet_refresh")
        .to_string();
    let claims = decode_access_token(&test_jwt_secret(), &a_access)
        .unwrap_or_else(|e| panic!("login fleet_access must decode: {e}"));
    let now = Utc::now();
    let expired_claims = AccessClaims {
        sub: claims.sub.clone(),
        email: claims.email.clone(),
        sid: claims.sid.clone(),
        exp: (now - chrono::Duration::seconds(120)).timestamp(),
        iat: (now - chrono::Duration::seconds(180)).timestamp(),
    };
    let expired = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &expired_claims,
        &jsonwebtoken::EncodingKey::from_secret(test_jwt_secret().as_bytes()),
    )
    .expect("encode expired AccessClaims");
    decode_access_token(&test_jwt_secret(), &expired)
        .expect_err("gated decode must reject expired access");

    let uid = user_id(&call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await);
    let ws = workspace_id(&a_out);

    let mut expired_only = CookieJar::default();
    expired_only.insert(ACCESS_COOKIE, expired);
    assert!(
        expired_only.refresh().is_none(),
        "expired-access logout must omit fleet_refresh"
    );
    let logout = call(&app, "POST", "/api/auth/logout", Some(&expired_only), None).await;
    assert_eq!(
        logout.status,
        StatusCode::OK,
        "expired-access logout: {}",
        logout.body
    );
    assert_eq!(logout.body["message"].as_str(), Some("logged out"));
    assert_logout_expires_cookie(
        &logout
            .cookie(ACCESS_COOKIE)
            .expect("expired-access logout must send access Set-Cookie"),
    );
    assert_logout_expires_cookie(
        &logout
            .cookie(REFRESH_COOKIE)
            .expect("expired-access logout must send refresh Set-Cookie"),
    );

    let mut a_replay = CookieJar::default();
    a_replay.insert(ACCESS_COOKIE, a_access.clone());
    assert_access_jwt_still_decodes(&a_access);
    let a_me_replay = call(&app, "GET", "/api/auth/me", Some(&a_replay), None).await;
    assert_eq!(
        a_me_replay.status,
        StatusCode::UNAUTHORIZED,
        "A's live access JWT after expired-access logout must be 401: {}",
        a_me_replay.body
    );
    assert_eq!(
        a_me_replay.error(),
        "session is no longer valid",
        "expired-access logout must revoke the sid: {}",
        a_me_replay.body
    );

    let mut stolen_refresh = CookieJar::default();
    stolen_refresh.insert(REFRESH_COOKIE, a_refresh.clone());
    let a_refresh_after = call(
        &app,
        "POST",
        "/api/auth/refresh",
        Some(&stolen_refresh),
        None,
    )
    .await;
    assert_eq!(
        a_refresh_after.status,
        StatusCode::UNAUTHORIZED,
        "A's original refresh after expired-access logout must not rotate: {}",
        a_refresh_after.body
    );

    let me_b_after = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_me_still_owner(
        &me_b_after,
        &email,
        &uid,
        &ws,
        "B GET /me after A's expired-access logout",
    );
}

#[tokio::test]
async fn logout_clears_cookies_and_blocks_refresh() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let out = call(&app, "POST", "/api/auth/logout", Some(&jar), None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_eq!(out.body["message"].as_str(), Some("logged out"));

    let access = out
        .cookie(ACCESS_COOKIE)
        .expect("logout must send access Set-Cookie");
    let refresh = out
        .cookie(REFRESH_COOKIE)
        .expect("logout must send refresh Set-Cookie");
    assert_eq!(
        access.max_age(),
        Some(0),
        "logout must expire access cookie: {}",
        access.raw
    );
    assert_eq!(
        refresh.max_age(),
        Some(0),
        "logout must expire refresh cookie: {}",
        refresh.raw
    );

    let refresh_after = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(
        refresh_after.status,
        StatusCode::UNAUTHORIZED,
        "revoked refresh must not rotate: {}",
        refresh_after.body
    );
}

#[tokio::test]
async fn login_is_case_insensitive_on_email() {
    let app = app().await;
    let local = format!("Case.{}", Uuid::new_v4());
    let email = format!("{local}@Example.TEST");
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (out, jar) = login(&app, &email.to_ascii_lowercase(), TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert!(jar.access().is_some());
}

#[tokio::test]
async fn signup_display_name_sets_workspace_name() {
    let app = app().await;
    let email = unique_email();
    let out = signup_with(&app, &email, TEST_PASSWORD, Some("Ada Lovelace")).await;
    assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);
    assert_eq!(
        out.body["default_workspace"]["name"].as_str(),
        Some("Ada Lovelace's workspace"),
        "{}",
        out.body
    );
}

/// Docs §4: POST /workspaces name > 80 is 400. Signup must not mint a
/// default personal workspace whose name exceeds that same cap via
/// `"{display_name}'s workspace"`.
async fn assert_signup_does_not_persist_workspace_name_over_80(
    app: &axum::Router,
    display_name: &str,
    password: &str,
    when: &str,
) {
    let unclamped = format!("{display_name}'s workspace");
    assert!(
        unclamped.len() > 80,
        "{when}: fixture must exceed the 80-char workspace name cap \
         (unclamped {unclamped:?} is {} chars)",
        unclamped.len()
    );

    let email = unique_email();
    let out = signup_with(app, &email, password, Some(display_name)).await;
    assert_ne!(
        out.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{when} must not 500: {}",
        out.body
    );
    assert_raw_omits_password_secrets(&out.raw, password, when);
    if let Some(obj) = out.body.get("user").and_then(|v| v.as_object()) {
        assert!(
            !obj.contains_key("password") && !obj.contains_key("password_hash"),
            "{when} user must not include password keys: {}",
            out.body
        );
    }
    if let Some(obj) = out
        .body
        .get("default_workspace")
        .and_then(|v| v.as_object())
    {
        assert!(
            !obj.contains_key("password") && !obj.contains_key("password_hash"),
            "{when} default_workspace must not include password keys: {}",
            out.body
        );
    }

    match out.status {
        StatusCode::BAD_REQUEST => {
            let err = out.error().to_ascii_lowercase();
            assert!(
                err.contains("too long") || err.contains("workspace name"),
                "{when} 400 must be workspace name too long (or equivalent), got {:?}: {}",
                out.error(),
                out.body
            );
            let (login_out, jar) = login(app, &email, password).await;
            assert_eq!(
                login_out.status,
                StatusCode::UNAUTHORIZED,
                "{when} 400 signup must not leave an account: {}",
                login_out.body
            );
            let listed = call(app, "GET", "/api/workspaces", Some(&jar), None).await;
            assert_eq!(
                listed.status,
                StatusCode::UNAUTHORIZED,
                "{when} GET /workspaces after a 400 signup login must be 401: {}",
                listed.body
            );
            for (label, blob) in [
                ("signup", out.raw.as_str()),
                ("login", login_out.raw.as_str()),
                ("GET /workspaces", listed.raw.as_str()),
            ] {
                assert!(
                    !blob.contains(&unclamped),
                    "{when} {label} must not include the unclamped >80 name {unclamped:?}: {blob}"
                );
            }
        }
        StatusCode::CREATED => {
            let name = out.body["default_workspace"]["name"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "{when} 201 must include default_workspace.name: {}",
                        out.body
                    )
                });
            assert!(
                name.len() <= 80,
                "{when} 201 must not persist default_workspace.name longer than 80 \
                 (got {} chars {name:?}; unclamped would be {}): {}",
                name.len(),
                unclamped.len(),
                out.body
            );
            let (login_out, jar) = login(app, &email, password).await;
            assert_eq!(
                login_out.status,
                StatusCode::OK,
                "{when} 201 signup must be able to log in: {}",
                login_out.body
            );
            if let Some(login_name) = login_out.body["default_workspace"]["name"].as_str() {
                assert!(
                    login_name.len() <= 80,
                    "{when} login default_workspace.name must be <=80 \
                     (got {} chars {login_name:?}): {}",
                    login_name.len(),
                    login_out.body
                );
            }
            let listed = call(app, "GET", "/api/workspaces", Some(&jar), None).await;
            assert_eq!(
                listed.status,
                StatusCode::OK,
                "{when} GET /workspaces: {}",
                listed.body
            );
            let workspaces = listed.body["workspaces"].as_array().unwrap_or_else(|| {
                panic!(
                    "{when} GET /workspaces must return a workspaces array: {}",
                    listed.body
                )
            });
            assert!(
                !workspaces.is_empty(),
                "{when} GET /workspaces must include the default workspace: {}",
                listed.body
            );
            for ws in workspaces {
                let n = ws["name"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{when} workspace.name: {ws}"));
                assert!(
                    n.len() <= 80,
                    "{when} GET /workspaces must not list a name > 80 \
                     (got {} chars {n:?}): {}",
                    n.len(),
                    listed.body
                );
            }
        }
        other => panic!(
            "{when} must be 400 or 201 (clamp), not {other}: {}",
            out.body
        ),
    }
}

/// Signup display_name must not create a workspace name over 80 chars.
/// POST /workspaces already 400s name > 80; the default personal
/// workspace from signup must obey the same bound (400 or clamp).
#[tokio::test]
async fn signup_workspace_name_must_not_exceed_80() {
    let app = app().await;

    let password_70 = format!("ws-name-70-{}-s3cret", Uuid::new_v4());
    assert_signup_does_not_persist_workspace_name_over_80(
        &app,
        &"x".repeat(70),
        &password_70,
        "POST /signup display_name=70 x's",
    )
    .await;

    let password_200 = format!("ws-name-200-{}-s3cret", Uuid::new_v4());
    assert_signup_does_not_persist_workspace_name_over_80(
        &app,
        &"x".repeat(200),
        &password_200,
        "POST /signup display_name=200 x's",
    )
    .await;

    // Control: a short display_name still 201s with "{name}'s workspace"
    // so this is not a tautology that every signup 400s.
    let ada_email = unique_email();
    let ada = signup_with(&app, &ada_email, TEST_PASSWORD, Some("Ada Lovelace")).await;
    assert_eq!(ada.status, StatusCode::CREATED, "{}", ada.body);
    assert_eq!(
        ada.body["default_workspace"]["name"].as_str(),
        Some("Ada Lovelace's workspace"),
        "{}",
        ada.body
    );
    assert_raw_omits_password_secrets(&ada.raw, TEST_PASSWORD, "control POST /signup Ada Lovelace");

    let (ada_login, ada_jar) = login(&app, &ada_email, TEST_PASSWORD).await;
    assert_eq!(ada_login.status, StatusCode::OK, "{}", ada_login.body);
    let listed = call(&app, "GET", "/api/workspaces", Some(&ada_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let names: Vec<&str> = listed.body["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .filter_map(|w| w["name"].as_str())
        .collect();
    assert!(
        names.contains(&"Ada Lovelace's workspace"),
        "GET /workspaces must show the short default name: {names:?} {}",
        listed.body
    );

    // Keep create_workspace_empty_and_too_long_400: POST 81 still 400.
    let too_long = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&ada_jar),
        Some(json!({ "name": "x".repeat(81) })),
    )
    .await;
    assert_ne!(
        too_long.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "POST /workspaces name of 81 must not 500: {}",
        too_long.body
    );
    assert_eq!(
        too_long.status,
        StatusCode::BAD_REQUEST,
        "POST /workspaces name of 81 must stay 400: {}",
        too_long.body
    );
    assert_eq!(too_long.error(), "workspace name is too long");
}

#[tokio::test]
async fn inactive_user_cannot_login() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let pool = connect_and_migrate().await;
    let n = sqlx::query("UPDATE users SET status = 'disabled' WHERE email_normalized = $1")
        .bind(email.to_ascii_lowercase())
        .execute(&pool)
        .await
        .expect("deactivate user");
    assert_eq!(n.rows_affected(), 1, "expected one user row updated");

    let (out, _) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::UNAUTHORIZED, "{}", out.body);
    assert_eq!(out.error(), "account is not active");
}

/// Docs §3.4: user deactivated later → subsequent `me` / refresh fail
/// unauthorized. Login-only coverage (`inactive_user_cannot_login`) is not
/// enough. Existing access JWT + refresh cookie must 401 after
/// `is_active=false`; cookies must not mint a new family.
#[tokio::test]
async fn inactive_user_after_login_me_and_refresh_401() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (first_login, jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(first_login.status, StatusCode::OK, "{}", first_login.body);
    // Second concurrent login is a distinct family. Refresh after
    // deactivate must not mint a third.
    let (second_login, second_jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(second_login.status, StatusCode::OK, "{}", second_login.body);

    let access = jar
        .access()
        .expect("login must set fleet_access")
        .to_string();
    let refresh = jar
        .refresh()
        .expect("login must set fleet_refresh")
        .to_string();
    assert!(!access.is_empty(), "login access cookie must be non-empty");
    assert!(
        !refresh.is_empty(),
        "login refresh cookie must be non-empty"
    );
    assert_access_jwt_still_decodes(&access);

    let before = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(
        before.status,
        StatusCode::OK,
        "pre-deactivate /me must work: {}",
        before.body
    );
    assert_eq!(
        before.body["user"]["email"].as_str(),
        Some(email.as_str()),
        "{}",
        before.body
    );
    let workspaces = before.body["workspaces"]
        .as_array()
        .expect("pre-deactivate /me workspaces");
    assert!(
        !workspaces.is_empty(),
        "pre-deactivate /me must include workspaces: {}",
        before.body
    );

    // Control account stays active so a later 401 is not "every /me is 401".
    let (other_email, other_jar, _) = signup_and_login(&app).await;

    let pool = connect_and_migrate().await;
    let before_sessions = user_session_rows(&pool, &email).await;
    assert_eq!(
        before_sessions.len(),
        2,
        "two concurrent logins must be two session rows: {before_sessions:?}"
    );
    let before_families: Vec<Uuid> = {
        let mut families: Vec<Uuid> = before_sessions
            .iter()
            .map(|(_, family, _)| *family)
            .collect();
        families.sort();
        families.dedup();
        families
    };
    assert_eq!(
        before_families.len(),
        2,
        "two concurrent logins must be two families: {before_sessions:?}"
    );

    let n = sqlx::query("UPDATE users SET status = 'disabled' WHERE email_normalized = $1")
        .bind(email.to_ascii_lowercase())
        .execute(&pool)
        .await
        .expect("deactivate user");
    assert_eq!(n.rows_affected(), 1, "expected one user row updated");

    // Lock is user status, not JWT expiry / missing cookie.
    assert_access_jwt_still_decodes(&access);

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_deactivated_me_401(&me, "GET /me with existing access JWT after deactivate");

    let mut access_only = CookieJar::default();
    access_only.insert(ACCESS_COOKIE, access.clone());
    let access_only_me = call(&app, "GET", "/api/auth/me", Some(&access_only), None).await;
    assert_deactivated_me_401(
        &access_only_me,
        "GET /me with access-only jar after deactivate",
    );

    let refreshed = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_deactivated_refresh_401(
        &refreshed,
        "POST /refresh with existing refresh cookie after deactivate",
    );

    let second_refreshed = call(&app, "POST", "/api/auth/refresh", Some(&second_jar), None).await;
    assert_deactivated_refresh_401(
        &second_refreshed,
        "POST /refresh of the other live family after deactivate",
    );

    let after_sessions = user_session_rows(&pool, &email).await;
    let after_ids: Vec<Uuid> = after_sessions.iter().map(|(id, _, _)| *id).collect();
    let after_families: Vec<Uuid> = {
        let mut families: Vec<Uuid> = after_sessions
            .iter()
            .map(|(_, family, _)| *family)
            .collect();
        families.sort();
        families.dedup();
        families
    };
    for (id, family, _) in &after_sessions {
        assert!(
            before_sessions
                .iter()
                .any(|(before_id, _, _)| before_id == id),
            "deactivated refresh must not insert a session row ({id}): before={before_sessions:?} after={after_sessions:?}"
        );
        assert!(
            before_families.contains(family),
            "deactivated refresh must not mint a new family ({family}): before={before_sessions:?} after={after_sessions:?}"
        );
    }
    assert_eq!(
        after_families, before_families,
        "session families must be unchanged: before={before_sessions:?} after={after_sessions:?}"
    );
    assert_eq!(
        after_ids.len(),
        before_sessions.len(),
        "deactivated refresh must not grow the session table: before={before_sessions:?} after={after_sessions:?}"
    );

    // Login-only is a sibling test; keep it here so this is not me/refresh
    // 401 while login still mints a family.
    let (login_again, login_jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(
        login_again.status,
        StatusCode::UNAUTHORIZED,
        "inactive login after deactivate: {}",
        login_again.body
    );
    assert_eq!(login_again.error(), "account is not active");
    assert_no_auth_cookies(&login_again, "inactive login after deactivate");
    assert!(
        login_jar.access().is_none() && login_jar.refresh().is_none(),
        "inactive login must not set cookies: {:?}",
        login_again.set_cookies()
    );
    let after_login_sessions = user_session_rows(&pool, &email).await;
    assert_eq!(
        after_login_sessions.len(),
        after_sessions.len(),
        "inactive login must not insert a session: before={after_sessions:?} after={after_login_sessions:?}"
    );

    let other_me = call(&app, "GET", "/api/auth/me", Some(&other_jar), None).await;
    assert_eq!(
        other_me.status,
        StatusCode::OK,
        "active control user /me must still work: {}",
        other_me.body
    );
    assert_eq!(
        other_me.body["user"]["email"].as_str(),
        Some(other_email.as_str()),
        "{}",
        other_me.body
    );
    let other_refresh = call(&app, "POST", "/api/auth/refresh", Some(&other_jar), None).await;
    assert_eq!(
        other_refresh.status,
        StatusCode::OK,
        "active control user refresh must still work: {}",
        other_refresh.body
    );
    assert_eq!(
        other_refresh.body["message"].as_str(),
        Some("token refreshed"),
        "{}",
        other_refresh.body
    );
    assert!(
        other_refresh.cookie(ACCESS_COOKIE).is_some()
            && other_refresh.cookie(REFRESH_COOKIE).is_some(),
        "control refresh must rotate cookies: {:?}",
        other_refresh.set_cookies()
    );
}

fn assert_deactivated_me_401(out: &HttpOut, when: &str) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{when} must be 401: {}",
        out.body
    );
    assert!(
        !out.error().is_empty(),
        "{when} must carry an error body: {}",
        out.body
    );
    // Cookie was presented and the JWT still decodes — this is user
    // status, not a missing cookie / dead JWT / logout revoke.
    assert_ne!(
        out.error(),
        "not authenticated",
        "{when} presented the access cookie: {}",
        out.body
    );
    assert_ne!(
        out.error(),
        "invalid or expired token",
        "{when} access JWT is still valid: {}",
        out.body
    );
    assert_ne!(
        out.error(),
        "session is no longer valid",
        "{when} is user deactivation, not session revoke: {}",
        out.body
    );
    assert!(
        out.body.get("user").is_none() && out.body.get("workspaces").is_none(),
        "{when} must not leak user/workspaces: {}",
        out.body
    );
    assert_no_auth_cookies(out, when);
}

fn assert_deactivated_refresh_401(out: &HttpOut, when: &str) {
    assert_eq!(
        out.status,
        StatusCode::UNAUTHORIZED,
        "{when} must be 401: {}",
        out.body
    );
    assert_eq!(
        out.error(),
        "account is not active",
        "{when} must match login's inactive 401, got {:?}",
        out.error()
    );
    assert_ne!(
        out.error(),
        "refresh token reuse detected — session family revoked",
        "{when} is deactivation, not reuse: {}",
        out.body
    );
    assert!(
        out.body.get("user").is_none() && out.body.get("default_workspace").is_none(),
        "{when} must not leak user/workspace: {}",
        out.body
    );
    assert_no_auth_cookies(out, when);
}

#[tokio::test]
async fn create_workspace_then_list_includes_both() {
    let app = app().await;
    let (_email, jar, login_out) = signup_and_login(&app).await;
    let first_id = login_out.body["default_workspace"]["id"]
        .as_str()
        .expect("default workspace")
        .to_string();

    let created = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": "Second" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let second_id = created.body["workspace"]["id"].as_str().unwrap();

    let listed = call(&app, "GET", "/api/workspaces", Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids: Vec<&str> = listed.body["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["id"].as_str())
        .collect();
    assert!(ids.contains(&first_id.as_str()), "{ids:?}");
    assert!(ids.contains(&second_id), "{ids:?}");
}

fn assert_listed_workspaces_include(
    listed: &HttpOut,
    id: &str,
    name: &str,
    slug: &str,
    when: &str,
) {
    assert_eq!(
        listed.status,
        StatusCode::OK,
        "{when} must be 200: {}",
        listed.body
    );
    let workspaces = listed.body["workspaces"]
        .as_array()
        .unwrap_or_else(|| panic!("{when} workspaces array: {}", listed.body));
    assert!(
        !workspaces.is_empty(),
        "{when} must list at least one workspace: {}",
        listed.body
    );
    let ids = listed_workspace_ids(listed);
    assert!(
        ids.contains(&id),
        "{when} must include workspace id {id}: {ids:?} {}",
        listed.body
    );
    let names = listed_workspace_names(listed);
    assert!(
        names.contains(&name),
        "{when} must include workspace name {name:?}: {names:?}"
    );
    let slugs = listed_workspace_slugs(listed);
    assert!(
        slugs.contains(&slug),
        "{when} must include workspace slug {slug:?}: {slugs:?}"
    );
}

fn assert_listed_workspaces_omit(listed: &HttpOut, id: &str, name: &str, slug: &str, when: &str) {
    if listed
        .body
        .get("workspaces")
        .and_then(Value::as_array)
        .is_some()
    {
        let ids = listed_workspace_ids(listed);
        assert!(
            !ids.contains(&id),
            "{when} must not include workspace id {id}: {ids:?}"
        );
        let names = listed_workspace_names(listed);
        assert!(
            !names.contains(&name),
            "{when} must not include workspace name {name:?}: {names:?}"
        );
        let slugs = listed_workspace_slugs(listed);
        assert!(
            !slugs.contains(&slug),
            "{when} must not include workspace slug {slug:?}: {slugs:?}"
        );
    }
    let blob = format!("{} {}", listed.body, listed.raw);
    for (label, needle) in [("id", id), ("name", name), ("slug", slug)] {
        assert!(
            !blob.contains(needle),
            "{when} must not contain other owner's {label} {needle:?}: {}",
            listed.body
        );
    }
}

/// Docs §3/§4: GET /api/workspaces and GET /api/auth/me return the caller's
/// memberships only (`list_for_user`). `me_lists_at_least_one_workspace` only
/// asserts the array is non-empty. `create_workspace_slug_and_owner` never GETs
/// as a second user. `create_workspace_then_list_includes_both` is the same
/// user creating a second workspace. A handler that `SELECT * FROM workspaces`
/// (or returns every default) leaks names and UUIDs used on every `/{ws}/…`
/// route.
#[tokio::test]
async fn workspaces_list_membership_only() {
    let app = app().await;
    let (a_email, a_jar, a_login) = signup_and_login(&app).await;
    let (b_email, b_jar, b_login) = signup_and_login(&app).await;
    assert_ne!(a_email, b_email, "A and B must be distinct accounts");

    let a_id = workspace_id(&a_login);
    let a_name = workspace_name(&a_login).to_string();
    let a_slug = workspace_slug(&a_login);
    let b_id = workspace_id(&b_login);
    let b_name = workspace_name(&b_login).to_string();
    let b_slug = workspace_slug(&b_login);
    assert_ne!(a_id, b_id, "A and B must own distinct workspace ids");
    assert_ne!(
        a_name, b_name,
        "A and B default names must differ so omit-name is not vacuous"
    );
    assert_ne!(
        a_slug, b_slug,
        "A and B default slugs must differ so omit-slug is not vacuous"
    );

    let a_listed = call(&app, "GET", "/api/workspaces", Some(&a_jar), None).await;
    let a_me = call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await;
    assert_listed_workspaces_include(&a_listed, &a_id, &a_name, &a_slug, "A GET /workspaces");
    assert_listed_workspaces_omit(&a_listed, &b_id, &b_name, &b_slug, "A GET /workspaces");
    assert_listed_workspaces_include(&a_me, &a_id, &a_name, &a_slug, "A GET /me");
    assert_listed_workspaces_omit(&a_me, &b_id, &b_name, &b_slug, "A GET /me");

    let b_listed = call(&app, "GET", "/api/workspaces", Some(&b_jar), None).await;
    let b_me = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_listed_workspaces_include(&b_listed, &b_id, &b_name, &b_slug, "B GET /workspaces");
    assert_listed_workspaces_omit(&b_listed, &a_id, &a_name, &a_slug, "B GET /workspaces");
    assert_listed_workspaces_include(&b_me, &b_id, &b_name, &b_slug, "B GET /me");
    assert_listed_workspaces_omit(&b_me, &a_id, &a_name, &a_slug, "B GET /me");

    let extra_name = format!("A-extra-{}", Uuid::new_v4());
    assert_ne!(extra_name, a_name);
    assert_ne!(extra_name, b_name);
    let created = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&a_jar),
        Some(json!({ "name": extra_name })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(
        created.body["workspace"]["name"].as_str(),
        Some(extra_name.as_str()),
        "{}",
        created.body
    );
    let extra_id = created.body["workspace"]["id"]
        .as_str()
        .expect("workspace.id")
        .to_string();
    let extra_slug = created.body["workspace"]["slug"]
        .as_str()
        .expect("workspace.slug")
        .to_string();
    assert_ne!(
        extra_id, a_id,
        "new workspace must not reuse A's default id"
    );
    assert_ne!(extra_id, b_id, "new workspace must not be B's id");

    let a_after = call(&app, "GET", "/api/workspaces", Some(&a_jar), None).await;
    assert_eq!(
        a_after.status,
        StatusCode::OK,
        "A's cookie on A GET /workspaces must stay 200: {}",
        a_after.body
    );
    let a_ids = listed_workspace_ids(&a_after);
    assert!(
        a_ids.contains(&a_id.as_str()),
        "A GET /workspaces must keep default {a_id}: {a_ids:?}"
    );
    assert!(
        a_ids.contains(&extra_id.as_str()),
        "A GET /workspaces must include new {extra_id}: {a_ids:?}"
    );
    assert_listed_workspaces_omit(
        &a_after,
        &b_id,
        &b_name,
        &b_slug,
        "A GET /workspaces after create",
    );

    let b_after = call(&app, "GET", "/api/workspaces", Some(&b_jar), None).await;
    let b_me_after = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    for (out, when) in [
        (&b_after, "B GET /workspaces after A create"),
        (&b_me_after, "B GET /me after A create"),
    ] {
        assert_listed_workspaces_include(out, &b_id, &b_name, &b_slug, when);
        assert_listed_workspaces_omit(out, &a_id, &a_name, &a_slug, when);
        assert_listed_workspaces_omit(out, &extra_id, &extra_name, &extra_slug, when);
    }

    // Keep unauthenticated_cookie_routes_are_401: GET /workspaces without a
    // cookie stays 401. A's cookie still 200 so this is not "every list is empty".
    let unauth = call(&app, "GET", "/api/workspaces", None, None).await;
    assert_eq!(
        unauth.status,
        StatusCode::UNAUTHORIZED,
        "unauthenticated GET /workspaces must stay 401: {}",
        unauth.body
    );
    assert!(
        unauth.body.get("workspaces").is_none(),
        "unauthenticated GET /workspaces must not list workspaces: {}",
        unauth.body
    );
    let unauth_blob = format!("{} {}", unauth.body, unauth.raw);
    for needle in [
        a_id.as_str(),
        a_name.as_str(),
        a_slug.as_str(),
        b_id.as_str(),
        b_name.as_str(),
        b_slug.as_str(),
        extra_id.as_str(),
        extra_name.as_str(),
        extra_slug.as_str(),
    ] {
        assert!(
            !unauth_blob.contains(needle),
            "unauthenticated GET /workspaces must not leak {needle:?}: {}",
            unauth.body
        );
    }

    let a_control = call(&app, "GET", "/api/workspaces", Some(&a_jar), None).await;
    assert_eq!(
        a_control.status,
        StatusCode::OK,
        "control: A's cookie on A GET /workspaces must stay 200: {}",
        a_control.body
    );
    let control_ids = listed_workspace_ids(&a_control);
    assert!(
        control_ids.contains(&a_id.as_str()) && control_ids.contains(&extra_id.as_str()),
        "control: A GET /workspaces 200 must still list both of A's ids: {control_ids:?}"
    );
}

#[tokio::test]
async fn invite_adds_existing_user_and_member_cannot_invite() {
    let app = app().await;
    let (owner_email, owner_jar, owner_me) = signup_and_login(&app).await;
    let ws = owner_me.body["default_workspace"]["id"]
        .as_str()
        .or_else(|| {
            owner_me.body["workspaces"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|w| w["id"].as_str())
        })
        .expect("workspace")
        .to_string();
    let _ = owner_email;

    let (member_email, member_jar, _) = signup_and_login(&app).await;

    let invited = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(invited.status, StatusCode::CREATED, "{}", invited.body);
    assert_eq!(invited.body["added_existing_user"], true);

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let roles: Vec<&str> = members.body["members"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["role"].as_str())
        .collect();
    assert!(roles.contains(&"owner"), "{roles:?}");
    assert!(roles.contains(&"member"), "{roles:?}");

    let forbidden = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&member_jar),
        Some(json!({ "email": unique_email(), "role": "member" })),
    )
    .await;
    assert_eq!(
        forbidden.status,
        StatusCode::FORBIDDEN,
        "{}",
        forbidden.body
    );
}

async fn workspace_invite_row_count(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
    email: &str,
) -> i64 {
    sqlx::query_scalar(
        r#"
        SELECT COUNT(*)::bigint
        FROM workspace_invites
        WHERE workspace_id = $1
          AND (
            email_normalized = $2
            OR email_normalized = lower(btrim($2))
          )
        "#,
    )
    .bind(workspace_id)
    .bind(email)
    .fetch_one(pool)
    .await
    .expect("count workspace_invites")
}

fn listed_invite_emails(listed: &HttpOut) -> Vec<String> {
    listed.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter_map(|i| i["email"].as_str().map(str::to_string))
        .collect()
}

/// `create_invite` trims + lowercases then 400s when empty or the address
/// has no '@'. Must not 201/409/500 or insert a `workspace_invites` row.
/// Member POST of a unique valid email stays 403. Control owner POST of a
/// unique `@example.test` address is still 201 so this is not “every invite 400s”.
#[tokio::test]
async fn invite_invalid_email_is_400_and_inserts_no_row() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let ws_uuid = Uuid::parse_str(&ws).expect("workspace uuid");
    let (member_email, member_jar, _) = signup_and_login(&app).await;
    add_existing_user(&app, &owner_jar, &ws, &member_email, "member").await;

    let pool = connect_and_migrate().await;
    let before = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.body);
    let before_count = before.body["invites"].as_array().expect("invites").len();

    for submitted in ["", "   ", "not-an-email", "nodomain"] {
        let rejected = call(
            &app,
            "POST",
            &format!("/api/workspaces/{ws}/invites"),
            Some(&owner_jar),
            Some(json!({ "email": submitted })),
        )
        .await;
        assert_ne!(
            rejected.status,
            StatusCode::CREATED,
            "invalid {submitted:?} must not 201: {}",
            rejected.body
        );
        assert_ne!(
            rejected.status,
            StatusCode::CONFLICT,
            "invalid {submitted:?} must not 409: {}",
            rejected.body
        );
        assert_ne!(
            rejected.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid {submitted:?} must not 500: {}",
            rejected.body
        );
        assert_eq!(
            rejected.status,
            StatusCode::BAD_REQUEST,
            "invalid {submitted:?} must be 400: {}",
            rejected.body
        );
        assert_eq!(
            rejected.error(),
            "valid email is required",
            "invalid {submitted:?}: {}",
            rejected.body
        );
        assert_ne!(
            rejected.body["added_existing_user"], true,
            "invalid {submitted:?} must not add a synthetic member: {}",
            rejected.body
        );

        let after = call(
            &app,
            "GET",
            &format!("/api/workspaces/{ws}/invites"),
            Some(&owner_jar),
            None,
        )
        .await;
        assert_eq!(after.status, StatusCode::OK, "{}", after.body);
        let emails = listed_invite_emails(&after);
        assert_eq!(
            emails.len(),
            before_count,
            "GET /invites count must stay {before_count} after rejecting {submitted:?}: {emails:?}"
        );
        assert!(
            !emails.iter().any(|e| e == submitted),
            "GET /invites must not include submitted {submitted:?}: {emails:?}"
        );
        let normalized = submitted.trim().to_lowercase();
        assert!(
            !emails.iter().any(|e| e == &normalized),
            "GET /invites must not include normalized {normalized:?} from {submitted:?}: {emails:?}"
        );
        if !submitted.trim().is_empty() {
            assert!(
                !after.raw.contains(submitted),
                "GET /invites must not include {submitted:?}: {}",
                after.raw
            );
        }

        let rows = workspace_invite_row_count(&pool, ws_uuid, submitted).await;
        assert_eq!(
            rows, 0,
            "no workspace_invites row for {submitted:?} (normalized {normalized:?})"
        );
    }

    let member_guest = unique_email();
    let forbidden = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&member_jar),
        Some(json!({ "email": member_guest })),
    )
    .await;
    assert_eq!(
        forbidden.status,
        StatusCode::FORBIDDEN,
        "member POST unique-valid must stay 403: {}",
        forbidden.body
    );
    assert_eq!(
        forbidden.error(),
        "not allowed to invite",
        "member POST unique-valid: {}",
        forbidden.body
    );
    let after_member = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(after_member.status, StatusCode::OK, "{}", after_member.body);
    let after_member_emails = listed_invite_emails(&after_member);
    assert_eq!(
        after_member_emails.len(),
        before_count,
        "member 403 must not insert: {after_member_emails:?}"
    );
    assert!(
        !after_member_emails.iter().any(|e| e == &member_guest),
        "member 403 must not list {member_guest}: {after_member_emails:?}"
    );
    assert_eq!(
        workspace_invite_row_count(&pool, ws_uuid, &member_guest).await,
        0,
        "member 403 must not insert workspace_invites for {member_guest}"
    );

    let control = unique_email();
    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": control })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(
        created.body["added_existing_user"], false,
        "control owner POST unique@example.test must be a pending invite: {}",
        created.body
    );
    assert_eq!(
        created.body["invite"]["email"].as_str(),
        Some(control.as_str()),
        "{}",
        created.body
    );

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let listed_emails = listed_invite_emails(&listed);
    assert!(
        listed_emails.iter().any(|e| e == &control),
        "control invite must be listed: {listed_emails:?}"
    );
    assert_eq!(
        listed_emails.len(),
        before_count + 1,
        "control must add exactly one open invite: {listed_emails:?}"
    );
    assert_eq!(
        workspace_invite_row_count(&pool, ws_uuid, &control).await,
        1,
        "control must insert one workspace_invites row for {control}"
    );
}

#[tokio::test]
async fn pending_invite_joins_on_signup() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_me) = signup_and_login(&app).await;
    let ws = owner_me.body["default_workspace"]["id"]
        .as_str()
        .or_else(|| {
            owner_me.body["workspaces"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|w| w["id"].as_str())
        })
        .expect("workspace")
        .to_string();

    let guest = unique_email();
    let inv = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": guest, "role": "admin" })),
    )
    .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.body);
    assert_eq!(inv.body["added_existing_user"], false);

    let _ = signup(&app, &guest, TEST_PASSWORD).await;
    let (login_out, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);

    let listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids: Vec<&str> = listed.body["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["id"].as_str())
        .collect();
    assert!(
        ids.contains(&ws.as_str()),
        "guest should join invited workspace: {ids:?}"
    );
}

#[tokio::test]
async fn login_accepts_pending_invites() {
    let app = app().await;
    let (owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let guest = unique_email();

    let inv = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": guest, "role": "admin" })),
    )
    .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.body);
    assert_eq!(inv.body["added_existing_user"], false);

    // Existing user for that email without signup (signup would consume the invite).
    let pool = connect_and_migrate().await;
    let guest_id = Uuid::now_v7();
    let n = sqlx::query(
        r#"
        INSERT INTO users (
            id, email, email_normalized, password_hash, status, created_at, updated_at
        )
        SELECT $1, $2, $3, password_hash, 'active', now(), now()
        FROM users
        WHERE email_normalized = $4
        "#,
    )
    .bind(guest_id)
    .bind(&guest)
    .bind(guest.to_ascii_lowercase())
    .bind(owner_email.to_ascii_lowercase())
    .execute(&pool)
    .await
    .expect("insert guest user");
    assert_eq!(n.rows_affected(), 1, "expected guest user inserted");

    let before = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.body);
    let before_emails: Vec<&str> = before.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|m| m["email"].as_str())
        .collect();
    assert!(
        !before_emails.iter().any(|e| e.eq_ignore_ascii_case(&guest)),
        "pending invite must not add the user before login: {before_emails:?}"
    );

    let (login_out, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);

    let listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = listed_workspace_ids(&listed);
    assert!(
        ids.contains(&ws.as_str()),
        "login must accept pending invite and join workspace: {ids:?}"
    );

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&guest_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    assert_eq!(
        members.body["you"]["role"].as_str(),
        Some("admin"),
        "guest should join as invited role: {}",
        members.body
    );
}

/// Login/signup as a different email must not consume another user's
/// pending invite. `accept_pending_invites_for_user` walks only
/// `list_pending_invites_for_email` for the caller's address.
#[tokio::test]
async fn invite_wrong_email_login_does_not_join() {
    let app = app().await;
    let (_owner_a_email, owner_a_jar, owner_a_login) = signup_and_login(&app).await;
    let ws_a = workspace_id(&owner_a_login);
    let (owner_b_email, owner_b_jar, owner_b_login) = signup_and_login(&app).await;
    let ws_b = workspace_id(&owner_b_login);
    let (_control_email, control_jar, _) = signup_and_login(&app).await;
    let alice_email = format!("alice.{}@example.test", Uuid::new_v4());

    let inv = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_a}/invites"),
        Some(&owner_a_jar),
        Some(json!({ "email": alice_email, "role": "admin" })),
    )
    .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.body);
    assert_eq!(inv.body["added_existing_user"], false);
    let invite_id = inv.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();

    let b_listed_before = call(&app, "GET", "/api/workspaces", Some(&owner_b_jar), None).await;
    assert_eq!(
        b_listed_before.status,
        StatusCode::OK,
        "{}",
        b_listed_before.body
    );
    let b_ids_before = listed_workspace_ids(&b_listed_before);
    assert!(
        !b_ids_before.contains(&ws_a.as_str()),
        "B must not already be in A's workspace: {b_ids_before:?}"
    );
    assert!(
        b_ids_before.contains(&ws_b.as_str()),
        "control: B still has B's workspace: {b_ids_before:?}"
    );

    let (b_login, b_jar) = login(&app, &owner_b_email, TEST_PASSWORD).await;
    assert_eq!(b_login.status, StatusCode::OK, "{}", b_login.body);

    let b_listed = call(&app, "GET", "/api/workspaces", Some(&b_jar), None).await;
    assert_eq!(b_listed.status, StatusCode::OK, "{}", b_listed.body);
    let b_ids = listed_workspace_ids(&b_listed);
    assert!(
        !b_ids.contains(&ws_a.as_str()),
        "B's login must not join A's workspace: {b_ids:?}"
    );
    assert!(
        b_ids.contains(&ws_b.as_str()),
        "B must keep B's workspace after login: {b_ids:?}"
    );

    let b_me = call(&app, "GET", "/api/auth/me", Some(&b_jar), None).await;
    assert_eq!(b_me.status, StatusCode::OK, "{}", b_me.body);
    let b_me_ids = listed_workspace_ids(&b_me);
    assert!(
        !b_me_ids.contains(&ws_a.as_str()),
        "GET /me must omit A's workspace for B: {b_me_ids:?}"
    );

    // GET /fleet is fleet-server; members + personas are the
    // membership-gated GETs this crate serves (403 not-a-member).
    for uri in [
        format!("/api/workspaces/{ws_a}/members"),
        format!("/api/workspaces/{ws_a}/personas"),
    ] {
        let forbidden = call(&app, "GET", &uri, Some(&b_jar), None).await;
        assert_eq!(
            forbidden.status,
            StatusCode::FORBIDDEN,
            "{uri}: {}",
            forbidden.body
        );
        assert!(
            forbidden.error().contains("not a member"),
            "{uri}: expected not-a-member, got {:?}",
            forbidden.error()
        );
    }

    let still_open = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/invites"),
        Some(&owner_a_jar),
        None,
    )
    .await;
    assert_eq!(still_open.status, StatusCode::OK, "{}", still_open.body);
    let open: Vec<&serde_json::Value> = still_open.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter(|i| i["id"].as_str() == Some(invite_id.as_str()))
        .collect();
    assert_eq!(
        open.len(),
        1,
        "B's login must not consume Alice's invite {invite_id}: {}",
        still_open.body
    );
    assert!(
        open[0].get("accepted_at").is_none() || open[0]["accepted_at"].is_null(),
        "invite accepted_at must stay unset: {}",
        open[0]
    );
    assert!(
        open[0].get("revoked_at").is_none() || open[0]["revoked_at"].is_null(),
        "invite revoked_at must stay unset: {}",
        open[0]
    );
    assert_eq!(
        open[0]["role"].as_str(),
        Some("admin"),
        "open invite role must stay admin: {}",
        open[0]
    );

    let created = signup(&app, &alice_email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let (alice_login, alice_jar) = login(&app, &alice_email, TEST_PASSWORD).await;
    assert_eq!(alice_login.status, StatusCode::OK, "{}", alice_login.body);

    let alice_listed = call(&app, "GET", "/api/workspaces", Some(&alice_jar), None).await;
    assert_eq!(alice_listed.status, StatusCode::OK, "{}", alice_listed.body);
    let alice_ids = listed_workspace_ids(&alice_listed);
    assert!(
        alice_ids.contains(&ws_a.as_str()),
        "Alice's signup+login must still join A: {alice_ids:?}"
    );

    let alice_members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/members"),
        Some(&alice_jar),
        None,
    )
    .await;
    assert_eq!(
        alice_members.status,
        StatusCode::OK,
        "{}",
        alice_members.body
    );
    assert_eq!(
        alice_members.body["you"]["role"].as_str(),
        Some("admin"),
        "Alice should join as invited admin: {}",
        alice_members.body
    );

    let b_listed_after = call(&app, "GET", "/api/workspaces", Some(&b_jar), None).await;
    assert_eq!(
        b_listed_after.status,
        StatusCode::OK,
        "{}",
        b_listed_after.body
    );
    let b_ids_after = listed_workspace_ids(&b_listed_after);
    assert!(
        !b_ids_after.contains(&ws_a.as_str()),
        "B must still omit A after Alice joins: {b_ids_after:?}"
    );

    let control_listed = call(&app, "GET", "/api/workspaces", Some(&control_jar), None).await;
    assert_eq!(
        control_listed.status,
        StatusCode::OK,
        "{}",
        control_listed.body
    );
    let control_ids = listed_workspace_ids(&control_listed);
    assert!(
        !control_ids.contains(&ws_a.as_str()),
        "control email with no invite must not join A: {control_ids:?}"
    );
}

#[tokio::test]
async fn expired_invite_does_not_join_on_signup() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let guest = unique_email();

    let inv = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": guest, "role": "member" })),
    )
    .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.body);
    assert_eq!(inv.body["added_existing_user"], false);
    let invite_id = inv.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();

    let pool = connect_and_migrate().await;
    let n = sqlx::query(
        "UPDATE workspace_invites SET expires_at = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(Uuid::parse_str(&invite_id).expect("invite uuid"))
    .execute(&pool)
    .await
    .expect("age expires_at");
    assert_eq!(n.rows_affected(), 1, "expected one invite aged");

    let created = signup(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let (login_out, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);

    let listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = listed_workspace_ids(&listed);
    assert!(
        !ids.contains(&ws.as_str()),
        "expired invite must not grant the workspace on signup: {ids:?} contains {ws}"
    );
}

#[tokio::test]
async fn last_owner_cannot_patch_self_to_member() {
    let app = app().await;
    let (_email, jar, login_out) = signup_and_login(&app).await;
    let ws = workspace_id(&login_out);
    let owner_id = user_id(&login_out);

    let out = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{owner_id}"),
        Some(&jar),
        Some(json!({ "role": "member" })),
    )
    .await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    let msg = out.error().to_ascii_lowercase();
    assert!(
        msg.contains("last owner"),
        "expected last-owner message, got {:?}",
        out.error()
    );

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    assert_eq!(
        members.body["you"]["role"].as_str(),
        Some("owner"),
        "last owner must still be owner after rejected demotion: {}",
        members.body
    );
}

/// Two owners: original PATCH self to member succeeds; the remaining
/// last owner cannot. Last-owner is keyed off owner count, not roster
/// size — after the original becomes a member they are still in the
/// workspace, so a last-member guard would wrongly allow the leftover
/// owner to demote.
#[tokio::test]
async fn owner_can_promote_another_owner_then_demote_self() {
    let app = app().await;
    let (_a_email, a_jar, a_login) = signup_and_login(&app).await;
    let ws = workspace_id(&a_login);
    let a_id = user_id(&a_login);
    let (b_email, b_jar, b_login) = signup_and_login(&app).await;
    let b_id = user_id(&b_login);
    add_existing_user(&app, &a_jar, &ws, &b_email, "member").await;

    let sole = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{a_id}"),
        Some(&a_jar),
        Some(json!({ "role": "member" })),
    )
    .await;
    assert_eq!(
        sole.status,
        StatusCode::BAD_REQUEST,
        "sole owner must not demote self: {}",
        sole.body
    );
    assert!(
        sole.error().to_ascii_lowercase().contains("last owner"),
        "expected last-owner before promote, got {:?}",
        sole.error()
    );

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{b_id}"),
        Some(&a_jar),
        Some(json!({ "role": "owner" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);
    assert_eq!(
        promoted.body["role"].as_str(),
        Some("owner"),
        "{}",
        promoted.body
    );
    assert_eq!(
        promoted.body["user_id"].as_str(),
        Some(b_id.as_str()),
        "{}",
        promoted.body
    );

    let demoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{a_id}"),
        Some(&a_jar),
        Some(json!({ "role": "member" })),
    )
    .await;
    assert_eq!(
        demoted.status,
        StatusCode::OK,
        "original owner PATCH self to member must succeed once another owner exists: {}",
        demoted.body
    );
    assert_eq!(
        demoted.body["role"].as_str(),
        Some("member"),
        "{}",
        demoted.body
    );
    assert_eq!(
        demoted.body["user_id"].as_str(),
        Some(a_id.as_str()),
        "{}",
        demoted.body
    );

    let listed_a = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(listed_a.status, StatusCode::OK, "{}", listed_a.body);
    assert_eq!(
        listed_a.body["you"]["role"].as_str(),
        Some("member"),
        "original must still be a member after self-demote: {}",
        listed_a.body
    );
    assert_eq!(
        listed_a.body["you"]["user_id"].as_str(),
        Some(a_id.as_str()),
        "{}",
        listed_a.body
    );
    let roles = member_roles(&listed_a);
    assert_eq!(
        roles.get(a_id.as_str()).copied(),
        Some("member"),
        "original must be member, not removed: {roles:?}"
    );
    assert_eq!(
        roles.get(b_id.as_str()).copied(),
        Some("owner"),
        "promoted owner must remain owner: {roles:?}"
    );
    assert_eq!(
        roles.len(),
        2,
        "roster must still be two members: {roles:?}"
    );

    let last = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{b_id}"),
        Some(&b_jar),
        Some(json!({ "role": "member" })),
    )
    .await;
    assert_eq!(
        last.status,
        StatusCode::BAD_REQUEST,
        "remaining last owner must not demote self: {}",
        last.body
    );
    assert!(
        last.error().to_ascii_lowercase().contains("last owner"),
        "expected last-owner after original demoted, got {:?}",
        last.error()
    );

    let listed_b = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(listed_b.status, StatusCode::OK, "{}", listed_b.body);
    assert_eq!(
        listed_b.body["you"]["role"].as_str(),
        Some("owner"),
        "last owner must still be owner after rejected demotion: {}",
        listed_b.body
    );
    let roles_after = member_roles(&listed_b);
    assert_eq!(
        roles_after.get(a_id.as_str()).copied(),
        Some("member"),
        "original must remain a member: {roles_after:?}"
    );
    assert_eq!(
        roles_after.get(b_id.as_str()).copied(),
        Some("owner"),
        "last owner must remain owner: {roles_after:?}"
    );
    assert_eq!(
        roles_after.len(),
        2,
        "rejected last-owner demote must not change roster: {roles_after:?}"
    );
}

#[tokio::test]
async fn last_owner_cannot_delete_self() {
    let app = app().await;
    let (_email, jar, login_out) = signup_and_login(&app).await;
    let ws = workspace_id(&login_out);
    let owner_id = user_id(&login_out);

    let out = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/members/{owner_id}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    let msg = out.error().to_ascii_lowercase();
    assert!(
        msg.contains("last owner"),
        "expected last-owner message, got {:?}",
        out.error()
    );

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    assert_eq!(
        members.body["you"]["role"].as_str(),
        Some("owner"),
        "last owner must still be a member after rejected leave: {}",
        members.body
    );
}

/// `member` is not owner/admin (`can_manage` is false) but DELETE self
/// must still succeed. A last-owner guard keyed off workspace owner
/// count (instead of the target's role) would reject this too.
#[tokio::test]
async fn member_without_manage_can_leave_self() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let owner_id = user_id(&owner_login);
    let (member_email, member_jar, member_login) = signup_and_login(&app).await;
    let member_id = user_id(&member_login);
    add_existing_user(&app, &owner_jar, &ws, &member_email, "member").await;

    let left = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/members/{member_id}"),
        Some(&member_jar),
        None,
    )
    .await;
    assert_eq!(left.status, StatusCode::OK, "{}", left.body);
    assert_eq!(left.body["removed"], true, "{}", left.body);
    assert_eq!(
        left.body["user_id"].as_str(),
        Some(member_id.as_str()),
        "{}",
        left.body
    );

    let after = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&member_jar),
        None,
    )
    .await;
    assert_eq!(
        after.status,
        StatusCode::FORBIDDEN,
        "leaver is no longer a member: {}",
        after.body
    );
    assert!(
        after.error().contains("not a member"),
        "expected not-a-member, got {:?}",
        after.error()
    );

    let again = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/members/{member_id}"),
        Some(&member_jar),
        None,
    )
    .await;
    assert_eq!(
        again.status,
        StatusCode::FORBIDDEN,
        "second leave must not succeed: {}",
        again.body
    );
    assert!(
        again.error().contains("not a member"),
        "second DELETE expected not-a-member, got {:?}",
        again.error()
    );

    let remaining = member_ids(&app, &owner_jar, &ws).await;
    assert!(
        remaining.contains(&owner_id),
        "owner must remain: {remaining:?}"
    );
    assert!(
        !remaining.contains(&member_id),
        "leaver must be gone: {remaining:?}"
    );
}

/// Leaving bypasses `can_manage` only for the caller. A member must
/// still be forbidden from DELETE of anyone else (including a missing id).
#[tokio::test]
async fn leave_self_does_not_let_member_remove_others() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let owner_id = user_id(&owner_login);
    let (member_email, member_jar, member_login) = signup_and_login(&app).await;
    let member_id = user_id(&member_login);
    add_existing_user(&app, &owner_jar, &ws, &member_email, "member").await;

    let (other_email, _other_jar, other_login) = signup_and_login(&app).await;
    let other_id = user_id(&other_login);
    add_existing_user(&app, &owner_jar, &ws, &other_email, "member").await;
    let unknown_id = Uuid::now_v7().to_string();

    for (label, target) in [
        ("owner", owner_id.as_str()),
        ("peer", other_id.as_str()),
        ("unknown", unknown_id.as_str()),
    ] {
        let out = call(
            &app,
            "DELETE",
            &format!("/api/workspaces/{ws}/members/{target}"),
            Some(&member_jar),
            None,
        )
        .await;
        assert_eq!(out.status, StatusCode::FORBIDDEN, "{label}: {}", out.body);
        assert!(
            out.error().contains("not allowed to remove members"),
            "{label}: expected remove-members forbid, got {:?}",
            out.error()
        );
    }

    let remaining = member_ids(&app, &owner_jar, &ws).await;
    for id in [&owner_id, &member_id, &other_id] {
        assert!(
            remaining.contains(id),
            "nobody should have been removed ({id}): {remaining:?}"
        );
    }
}

/// Last-owner only applies when this leave would drop the last owner.
/// A second owner makes self-leave legal.
#[tokio::test]
async fn owner_can_leave_when_another_owner_exists() {
    let app = app().await;
    let (_a_email, a_jar, a_login) = signup_and_login(&app).await;
    let ws = workspace_id(&a_login);
    let a_id = user_id(&a_login);
    let (b_email, b_jar, b_login) = signup_and_login(&app).await;
    let b_id = user_id(&b_login);
    add_existing_user(&app, &a_jar, &ws, &b_email, "member").await;

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{b_id}"),
        Some(&a_jar),
        Some(json!({ "role": "owner" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);
    assert_eq!(promoted.body["role"].as_str(), Some("owner"));

    let left = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/members/{a_id}"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(left.status, StatusCode::OK, "{}", left.body);
    assert_eq!(left.body["removed"], true);

    let gone = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(gone.status, StatusCode::FORBIDDEN, "{}", gone.body);

    let remaining = member_ids(&app, &b_jar, &ws).await;
    assert!(remaining.contains(&b_id), "remaining owner: {remaining:?}");
    assert!(!remaining.contains(&a_id), "leaver gone: {remaining:?}");

    let last = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/members/{b_id}"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        last.status,
        StatusCode::BAD_REQUEST,
        "now-last owner must not leave: {}",
        last.body
    );
    assert!(
        last.error().to_ascii_lowercase().contains("last owner"),
        "got {:?}",
        last.error()
    );
}

fn workspace_name(login: &HttpOut) -> &str {
    login.body["default_workspace"]["name"]
        .as_str()
        .expect("default_workspace.name")
}

fn workspace_slug(login: &HttpOut) -> String {
    login.body["default_workspace"]["slug"]
        .as_str()
        .expect("default_workspace.slug")
        .to_string()
}

fn listed_workspace_names(listed: &HttpOut) -> Vec<&str> {
    listed.body["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .filter_map(|w| w["name"].as_str())
        .collect()
}

fn listed_workspace_slugs(listed: &HttpOut) -> Vec<&str> {
    listed.body["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .filter_map(|w| w["slug"].as_str())
        .collect()
}

fn assert_auth_body_omits_password_secrets(out: &HttpOut, when: &str) {
    assert_raw_omits_password_secrets(&out.raw, TEST_PASSWORD, when);
    if let Some(user) = out.body.get("user").and_then(|v| v.as_object()) {
        assert!(
            !user.contains_key("password") && !user.contains_key("password_hash"),
            "{when} user must not include password keys: {user:?}"
        );
    }
    if let Some(workspaces) = out.body.get("workspaces").and_then(|v| v.as_array()) {
        for ws in workspaces {
            if let Some(obj) = ws.as_object() {
                assert!(
                    !obj.contains_key("password") && !obj.contains_key("password_hash"),
                    "{when} workspace must not include password keys: {ws}"
                );
            }
        }
    }
    if let Some(ws) = out
        .body
        .get("default_workspace")
        .and_then(|v| v.as_object())
    {
        assert!(
            !ws.contains_key("password") && !ws.contains_key("password_hash"),
            "{when} default_workspace must not include password keys: {}",
            out.body["default_workspace"]
        );
    }
}

/// GET /me and GET /workspaces after zero memberships must mint a new
/// personal default (not the workspace just left, and not the other
/// owner's private workspace).
fn assert_new_default_personal_workspace(
    listed: &HttpOut,
    left_id: &str,
    expected_name: &str,
    other_only_id: &str,
    other_only_name: &str,
    when: &str,
) -> String {
    assert_eq!(listed.status, StatusCode::OK, "{when}: {}", listed.body);
    assert_auth_body_omits_password_secrets(listed, when);
    let workspaces = listed.body["workspaces"]
        .as_array()
        .unwrap_or_else(|| panic!("{when} workspaces array: {}", listed.body));
    assert!(
        !workspaces.is_empty(),
        "{when}: expected a default personal workspace, got empty: {}",
        listed.body
    );
    let ids = listed_workspace_ids(listed);
    assert!(
        !ids.contains(&left_id),
        "{when}: must not re-join the left workspace {left_id}: {ids:?}"
    );
    assert!(
        !ids.contains(&other_only_id),
        "{when}: must not include the other owner's workspace {other_only_id}: {ids:?}"
    );
    let names = listed_workspace_names(listed);
    assert!(
        !names.contains(&other_only_name),
        "{when}: must not include the other owner's workspace name {other_only_name:?}: {names:?}"
    );

    let default = workspaces
        .iter()
        .find(|w| w["role"].as_str() == Some("owner") && w["name"].as_str() == Some(expected_name));
    let default = default.unwrap_or_else(|| {
        panic!("{when}: missing default personal workspace named {expected_name:?}: {workspaces:?}")
    });
    let new_id = default["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{when} default id: {default}"))
        .to_string();
    assert_ne!(
        new_id, left_id,
        "{when}: new default must not be the left id"
    );
    new_id
}

async fn assert_not_a_member_on_left(
    app: &axum::Router,
    jar: &CookieJar,
    left_id: &str,
    when: &str,
) {
    // GET /fleet is fleet-server; members + personas are the
    // membership-gated GETs this crate serves (403 not-a-member).
    for uri in [
        format!("/api/workspaces/{left_id}/members"),
        format!("/api/workspaces/{left_id}/personas"),
    ] {
        let forbidden = call(app, "GET", &uri, Some(jar), None).await;
        assert_eq!(
            forbidden.status,
            StatusCode::FORBIDDEN,
            "{when} {uri}: {}",
            forbidden.body
        );
        assert!(
            forbidden.error().contains("not a member"),
            "{when} {uri}: expected not-a-member, got {:?}",
            forbidden.error()
        );
    }
}

async fn promote_to_owner(app: &axum::Router, owner_jar: &CookieJar, ws: &str, user_id: &str) {
    let promoted = call(
        app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{user_id}"),
        Some(owner_jar),
        Some(json!({ "role": "owner" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);
    assert_eq!(
        promoted.body["role"].as_str(),
        Some("owner"),
        "{}",
        promoted.body
    );
}

/// Leaving (or being kicked from) the last workspace must not leave
/// `workspaces:[]`. GET /me and GET /workspaces create a new default
/// personal workspace; they must not re-insert the left membership.
#[tokio::test]
async fn leave_zero_workspaces_me_creates_default() {
    let app = app().await;

    // 1. Owner A leaves after promoting B. A's next /me and /workspaces
    // mint a new personal default — not the left id and not B's workspace.
    let (a_email, a_jar, a_login) = signup_and_login(&app).await;
    let left = workspace_id(&a_login);
    let a_default_name = workspace_name(&a_login).to_string();
    let a_id = user_id(&a_login);
    let (b_email, b_jar, b_login) = signup_and_login(&app).await;
    let b_only = workspace_id(&b_login);
    let b_only_name = workspace_name(&b_login).to_string();
    let b_id = user_id(&b_login);
    add_existing_user(&app, &a_jar, &left, &b_email, "member").await;
    promote_to_owner(&app, &a_jar, &left, &b_id).await;

    let left_self = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{left}/members/{a_id}"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_eq!(left_self.status, StatusCode::OK, "{}", left_self.body);
    assert_eq!(left_self.body["removed"], true, "{}", left_self.body);
    assert_eq!(
        left_self.body["user_id"].as_str(),
        Some(a_id.as_str()),
        "{}",
        left_self.body
    );

    let a_me = call(&app, "GET", "/api/auth/me", Some(&a_jar), None).await;
    let a_new = assert_new_default_personal_workspace(
        &a_me,
        &left,
        &a_default_name,
        &b_only,
        &b_only_name,
        "A GET /me after leave",
    );
    assert_eq!(
        a_me.body["user"]["email"].as_str(),
        Some(a_email.as_str()),
        "A GET /me user: {}",
        a_me.body
    );

    let a_listed = call(&app, "GET", "/api/workspaces", Some(&a_jar), None).await;
    let a_listed_new = assert_new_default_personal_workspace(
        &a_listed,
        &left,
        &a_default_name,
        &b_only,
        &b_only_name,
        "A GET /workspaces after leave",
    );
    let mut me_ids = listed_workspace_ids(&a_me);
    let mut list_ids = listed_workspace_ids(&a_listed);
    me_ids.sort_unstable();
    list_ids.sort_unstable();
    assert_eq!(
        me_ids, list_ids,
        "A GET /me and GET /workspaces must return the same workspace set"
    );
    assert_eq!(
        a_new, a_listed_new,
        "A default id must match on /me and /workspaces"
    );

    // 2. Creating the new default must not re-join the left workspace.
    assert_not_a_member_on_left(&app, &a_jar, &left, "A after /me").await;

    let b_members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{left}/members"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(b_members.status, StatusCode::OK, "{}", b_members.body);
    assert_eq!(
        b_members.body["you"]["role"].as_str(),
        Some("owner"),
        "B must remain owner of the left workspace: {}",
        b_members.body
    );
    let b_roster: Vec<(&str, &str)> = b_members.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|m| Some((m["user_id"].as_str()?, m["role"].as_str()?)))
        .collect();
    assert!(
        b_roster
            .iter()
            .any(|(id, role)| *id == b_id && *role == "owner"),
        "B must still be listed as owner: {b_roster:?}"
    );
    assert!(
        b_roster.iter().all(|(id, _)| *id != a_id),
        "A must not be listed on the left workspace: {b_roster:?}"
    );

    let a_refresh = call(&app, "POST", "/api/auth/refresh", Some(&a_jar), None).await;
    assert_eq!(a_refresh.status, StatusCode::OK, "{}", a_refresh.body);
    assert_auth_body_omits_password_secrets(&a_refresh, "A POST /refresh after leave");
    assert_eq!(
        a_refresh.body["default_workspace"]["id"].as_str(),
        Some(a_new.as_str()),
        "refresh default_workspace must be the new personal default: {}",
        a_refresh.body
    );
    assert_ne!(
        a_refresh.body["default_workspace"]["id"].as_str(),
        Some(left.as_str()),
        "refresh must not land on the left workspace: {}",
        a_refresh.body
    );

    // 3. Kick path: D removes C after being promoted. C also gets a new
    // default that is not the workspace they were kicked from.
    let (_c_email, c_jar, c_login) = signup_and_login(&app).await;
    let c_left = workspace_id(&c_login);
    let c_default_name = workspace_name(&c_login).to_string();
    let c_id = user_id(&c_login);
    let (d_email, d_jar, d_login) = signup_and_login(&app).await;
    let d_only = workspace_id(&d_login);
    let d_only_name = workspace_name(&d_login).to_string();
    let d_id = user_id(&d_login);
    add_existing_user(&app, &c_jar, &c_left, &d_email, "member").await;
    promote_to_owner(&app, &c_jar, &c_left, &d_id).await;

    let kicked = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{c_left}/members/{c_id}"),
        Some(&d_jar),
        None,
    )
    .await;
    assert_eq!(kicked.status, StatusCode::OK, "{}", kicked.body);
    assert_eq!(kicked.body["removed"], true, "{}", kicked.body);

    let c_me = call(&app, "GET", "/api/auth/me", Some(&c_jar), None).await;
    assert_new_default_personal_workspace(
        &c_me,
        &c_left,
        &c_default_name,
        &d_only,
        &d_only_name,
        "C GET /me after kick",
    );
    let c_listed = call(&app, "GET", "/api/workspaces", Some(&c_jar), None).await;
    assert_new_default_personal_workspace(
        &c_listed,
        &c_left,
        &c_default_name,
        &d_only,
        &d_only_name,
        "C GET /workspaces after kick",
    );
    assert_not_a_member_on_left(&app, &c_jar, &c_left, "C after kick /me").await;

    let d_roster = member_ids(&app, &d_jar, &c_left).await;
    assert!(d_roster.contains(&d_id), "D remains: {d_roster:?}");
    assert!(!d_roster.contains(&c_id), "C must not remain: {d_roster:?}");

    // 4. Control: last owner still cannot leave. Fresh signup GET /me
    // still has exactly the signup workspace (not a second invented one).
    let b_last = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{left}/members/{b_id}"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        b_last.status,
        StatusCode::BAD_REQUEST,
        "now-last owner B must not leave: {}",
        b_last.body
    );
    assert!(
        b_last.error().to_ascii_lowercase().contains("last owner"),
        "expected last-owner, got {:?}",
        b_last.error()
    );

    let (fresh_email, fresh_jar, fresh_login) = signup_and_login(&app).await;
    let fresh_ws = workspace_id(&fresh_login);
    let fresh_id = user_id(&fresh_login);
    let fresh_me = call(&app, "GET", "/api/auth/me", Some(&fresh_jar), None).await;
    assert_eq!(fresh_me.status, StatusCode::OK, "{}", fresh_me.body);
    assert_auth_body_omits_password_secrets(&fresh_me, "fresh signup GET /me");
    let fresh_ids = listed_workspace_ids(&fresh_me);
    assert_eq!(
        fresh_ids,
        vec![fresh_ws.as_str()],
        "fresh GET /me must keep exactly the signup workspace, not invent another: {}",
        fresh_me.body
    );
    assert_eq!(
        fresh_me.body["user"]["email"].as_str(),
        Some(fresh_email.as_str()),
        "{}",
        fresh_me.body
    );

    let (member_email, _member_jar, member_login) = signup_and_login(&app).await;
    let member_id = user_id(&member_login);
    add_existing_user(&app, &fresh_jar, &fresh_ws, &member_email, "member").await;
    let last_with_members = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{fresh_ws}/members/{fresh_id}"),
        Some(&fresh_jar),
        None,
    )
    .await;
    assert_eq!(
        last_with_members.status,
        StatusCode::BAD_REQUEST,
        "last owner must not leave while members remain: {}",
        last_with_members.body
    );
    assert!(
        last_with_members
            .error()
            .to_ascii_lowercase()
            .contains("last owner"),
        "expected last-owner, got {:?}",
        last_with_members.error()
    );
    let remaining = member_ids(&app, &fresh_jar, &fresh_ws).await;
    assert!(
        remaining.contains(&fresh_id),
        "last owner must remain: {remaining:?}"
    );
    assert!(
        remaining.contains(&member_id),
        "non-owner member must remain: {remaining:?}"
    );

    let solo_leave = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{b_only}/members/{b_id}"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        solo_leave.status,
        StatusCode::BAD_REQUEST,
        "last owner of their signup workspace must not leave: {}",
        solo_leave.body
    );
    assert!(
        solo_leave
            .error()
            .to_ascii_lowercase()
            .contains("last owner"),
        "expected last-owner on B's signup workspace, got {:?}",
        solo_leave.error()
    );
}

/// Admin has `can_manage`, but leave is still a self-DELETE (and must
/// not be blocked by last-owner, because the target is not an owner).
#[tokio::test]
async fn admin_can_leave_self() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let owner_id = user_id(&owner_login);
    let (admin_email, admin_jar, admin_login) = signup_and_login(&app).await;
    let admin_id = user_id(&admin_login);
    add_existing_user(&app, &owner_jar, &ws, &admin_email, "admin").await;

    let left = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/members/{admin_id}"),
        Some(&admin_jar),
        None,
    )
    .await;
    assert_eq!(left.status, StatusCode::OK, "{}", left.body);
    assert_eq!(left.body["removed"], true);
    assert!(
        !left.error().to_ascii_lowercase().contains("last owner"),
        "admin self-DELETE must not be last-owner blocked: {}",
        left.body
    );

    let after = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&admin_jar),
        None,
    )
    .await;
    assert_eq!(after.status, StatusCode::FORBIDDEN, "{}", after.body);

    let remaining = member_ids(&app, &owner_jar, &ws).await;
    assert!(remaining.contains(&owner_id), "{remaining:?}");
    assert!(!remaining.contains(&admin_id), "{remaining:?}");
}

/// Last-owner is keyed off remaining owners, not roster size. A sole
/// owner with non-owner members still cannot DELETE self (a last-member
/// guard would allow this).
#[tokio::test]
async fn last_owner_cannot_leave_self_when_members_remain() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let owner_id = user_id(&owner_login);
    let (member_email, _member_jar, member_login) = signup_and_login(&app).await;
    let member_id = user_id(&member_login);
    add_existing_user(&app, &owner_jar, &ws, &member_email, "member").await;

    let out = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/members/{owner_id}"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    let msg = out.error().to_ascii_lowercase();
    assert!(
        msg.contains("last owner"),
        "expected last-owner message, got {:?}",
        out.error()
    );

    let remaining = member_ids(&app, &owner_jar, &ws).await;
    assert!(
        remaining.contains(&owner_id),
        "last owner must still be a member: {remaining:?}"
    );
    assert!(
        remaining.contains(&member_id),
        "non-owner member must remain: {remaining:?}"
    );
    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(
        listed.body["you"]["role"].as_str(),
        Some("owner"),
        "last owner must still be owner after rejected leave: {}",
        listed.body
    );
}

#[tokio::test]
async fn member_cannot_list_or_revoke_invites() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);

    let pending = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": unique_email(), "role": "member" })),
    )
    .await;
    assert_eq!(pending.status, StatusCode::CREATED, "{}", pending.body);
    let invite_id = pending.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();

    let (member_email, member_jar, _) = signup_and_login(&app).await;
    let added = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    assert_eq!(added.body["added_existing_user"], true);

    let list = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&member_jar),
        None,
    )
    .await;
    assert_eq!(list.status, StatusCode::FORBIDDEN, "{}", list.body);

    let revoke = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/invites/{invite_id}"),
        Some(&member_jar),
        None,
    )
    .await;
    assert_eq!(revoke.status, StatusCode::FORBIDDEN, "{}", revoke.body);

    let still = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(still.status, StatusCode::OK, "{}", still.body);
    let ids: Vec<&str> = still.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert!(
        ids.contains(&invite_id.as_str()),
        "member revoke must not remove the invite: {ids:?}"
    );
}

/// Foreign owner on another workspace's GET /members and GET/POST/DELETE
/// /invites is 403 not-a-member (membership, not can_manage). Distinct
/// from PATCH/DELETE /members/{id} and from a same-workspace member role.
fn assert_other_ws_not_a_member(out: &HttpOut, when: &str, secrets: &[&str]) {
    assert_ne!(
        out.status,
        StatusCode::OK,
        "{when} must not 200: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::CREATED,
        "{when} must not 201: {}",
        out.body
    );
    assert_ne!(
        out.status,
        StatusCode::NOT_FOUND,
        "{when} must not 404: {}",
        out.body
    );
    assert_eq!(
        out.status,
        StatusCode::FORBIDDEN,
        "{when} must be 403: {}",
        out.body
    );
    assert!(
        out.error().contains("not a member"),
        "{when} expected not-a-member, got {:?}",
        out.error()
    );
    let msg = out.error().to_ascii_lowercase();
    assert!(
        !msg.contains("not allowed to"),
        "{when} must be membership 403, not can_manage 403: {:?}",
        out.error()
    );
    let raw = out.raw.to_ascii_lowercase();
    for secret in secrets {
        if secret.is_empty() {
            continue;
        }
        assert!(
            !raw.contains(&secret.to_ascii_lowercase()),
            "{when} body must not contain {secret:?}: {}",
            out.raw
        );
    }
}

/// Two independent owners: A's cookie must not list B's roster or
/// list/create/revoke B's invites. Product membership is already
/// checked on every scoped route — this locks GET /members and
/// GET/POST/DELETE /invites, which PATCH/DELETE /members/{id} does not.
#[tokio::test]
async fn members_invites_other_ws_are_403_not_a_member() {
    let app = app().await;
    let (_a_email, a_jar, _a_login) = signup_and_login(&app).await;

    let b_email = unique_email();
    let b_display = format!("Bravo {}", Uuid::new_v4());
    let b_created = signup_with(&app, &b_email, TEST_PASSWORD, Some(&b_display)).await;
    assert_eq!(b_created.status, StatusCode::CREATED, "{}", b_created.body);
    let (b_login, b_jar) = login(&app, &b_email, TEST_PASSWORD).await;
    assert_eq!(b_login.status, StatusCode::OK, "{}", b_login.body);
    let b_ws = workspace_id(&b_login);
    let b_id = user_id(&b_login);
    let b_ws_uuid = Uuid::parse_str(&b_ws).expect("B workspace uuid");

    let b_guest = unique_email();
    let invited = call(
        &app,
        "POST",
        &format!("/api/workspaces/{b_ws}/invites"),
        Some(&b_jar),
        Some(json!({ "email": b_guest, "role": "member" })),
    )
    .await;
    assert_eq!(invited.status, StatusCode::CREATED, "{}", invited.body);
    assert_eq!(invited.body["added_existing_user"], false);
    let b_invite_id = invited.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();

    let members_forbidden = call(
        &app,
        "GET",
        &format!("/api/workspaces/{b_ws}/members"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_other_ws_not_a_member(
        &members_forbidden,
        "A GET B /members",
        &[&b_email, &b_id, &b_display],
    );

    let invites_forbidden = call(
        &app,
        "GET",
        &format!("/api/workspaces/{b_ws}/invites"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_other_ws_not_a_member(
        &invites_forbidden,
        "A GET B /invites",
        &[&b_email, &b_id, &b_display, &b_guest, &b_invite_id],
    );

    let a_guest = unique_email();
    let post_forbidden = call(
        &app,
        "POST",
        &format!("/api/workspaces/{b_ws}/invites"),
        Some(&a_jar),
        Some(json!({ "email": a_guest, "role": "member" })),
    )
    .await;
    assert_other_ws_not_a_member(
        &post_forbidden,
        "A POST B /invites",
        &[&b_email, &b_id, &b_display],
    );
    assert_ne!(
        post_forbidden.body["added_existing_user"], true,
        "A POST B /invites must not add a member: {}",
        post_forbidden.body
    );
    assert!(
        post_forbidden.body.get("invite").is_none(),
        "A POST B /invites must not return an invite: {}",
        post_forbidden.body
    );

    let delete_forbidden = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{b_ws}/invites/{b_invite_id}"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_other_ws_not_a_member(
        &delete_forbidden,
        "A DELETE B /invites",
        &[&b_email, &b_id, &b_display, &b_guest],
    );
    assert_ne!(
        delete_forbidden.body["revoked"], true,
        "A DELETE B /invites must not revoke: {}",
        delete_forbidden.body
    );

    let still_invites = call(
        &app,
        "GET",
        &format!("/api/workspaces/{b_ws}/invites"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        still_invites.status,
        StatusCode::OK,
        "control: B GET B /invites must stay 200: {}",
        still_invites.body
    );
    let invite_ids: Vec<&str> = still_invites.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert_eq!(
        invite_ids.as_slice(),
        [b_invite_id.as_str()],
        "B must still have exactly the original invite id: {invite_ids:?}"
    );

    let still_members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{b_ws}/members"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        still_members.status,
        StatusCode::OK,
        "control: B GET B /members must stay 200: {}",
        still_members.body
    );
    let members = still_members.body["members"].as_array().expect("members");
    assert_eq!(
        members.len(),
        1,
        "B GET /members must still be only B: {}",
        still_members.body
    );
    assert_eq!(members[0]["user_id"].as_str(), Some(b_id.as_str()));
    assert_eq!(members[0]["email"].as_str(), Some(b_email.as_str()));
    assert_eq!(
        members[0]["display_name"].as_str(),
        Some(b_display.as_str()),
        "control: B's display_name must be on B's roster so the 403 leak check is not vacuous: {}",
        still_members.body
    );
    assert_eq!(still_members.body["you"]["role"].as_str(), Some("owner"));
    assert_eq!(
        still_members.body["you"]["user_id"].as_str(),
        Some(b_id.as_str())
    );

    let pool = connect_and_migrate().await;
    assert_eq!(
        workspace_invite_row_count(&pool, b_ws_uuid, &a_guest).await,
        0,
        "no workspace_invites row for A's guest email on B ({a_guest})"
    );
}

#[tokio::test]
async fn revoke_invite_then_signup_does_not_join() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let guest = unique_email();

    let inv = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": guest, "role": "member" })),
    )
    .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.body);
    assert_eq!(inv.body["added_existing_user"], false);
    let invite_id = inv.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();

    let revoked = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/invites/{invite_id}"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
    assert_eq!(revoked.body["revoked"], true);

    let created = signup(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let (login_out, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);

    let listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = listed_workspace_ids(&listed);
    assert!(
        !ids.contains(&ws.as_str()),
        "revoked invite must not grant the workspace: {ids:?} contains {ws}"
    );
}

/// `revoke_invite` UPDATEs `workspace_invites` WHERE workspace_id=$1 AND id=$2.
/// B's cookie on B's path with A's invite_id is 404 (not 200 revoked). B on
/// A's path is 403 not-a-member (list-members-invites-other-ws-403).
fn assert_invite_still_open(listed: &HttpOut, invite_id: &str, when: &str) -> serde_json::Value {
    assert_eq!(listed.status, StatusCode::OK, "{when}: {}", listed.body);
    let open: Vec<&serde_json::Value> = listed.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter(|i| i["id"].as_str() == Some(invite_id))
        .collect();
    assert_eq!(
        open.len(),
        1,
        "{when}: invite {invite_id} must stay open: {}",
        listed.body
    );
    assert!(
        open[0].get("accepted_at").is_none() || open[0]["accepted_at"].is_null(),
        "{when}: accepted_at must stay unset: {}",
        open[0]
    );
    assert!(
        open[0].get("revoked_at").is_none() || open[0]["revoked_at"].is_null(),
        "{when}: revoked_at must stay unset: {}",
        open[0]
    );
    open[0].clone()
}

#[tokio::test]
async fn invite_foreign_id_delete_is_404_and_does_not_revoke() {
    let app = app().await;
    let (_a_email, a_jar, a_login) = signup_and_login(&app).await;
    let ws_a = workspace_id(&a_login);
    let (_b_email, b_jar, b_login) = signup_and_login(&app).await;
    let ws_b = workspace_id(&b_login);
    let guest = format!("guest.{uuid}@example.test", uuid = Uuid::new_v4());

    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_a}/invites"),
        Some(&a_jar),
        Some(json!({ "email": guest, "role": "member" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["added_existing_user"], false);
    let invite_id = created.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();

    let foreign = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws_b}/invites/{invite_id}"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_ne!(
        foreign.status,
        StatusCode::OK,
        "B DELETE of A's invite_id on B's path must not revoke: {}",
        foreign.body
    );
    assert_ne!(
        foreign.body["revoked"], true,
        "B DELETE of A's invite_id must not return revoked: {}",
        foreign.body
    );
    assert_eq!(
        foreign.status,
        StatusCode::NOT_FOUND,
        "B DELETE of A's invite_id on B's path must be 404: {}",
        foreign.body
    );
    assert!(
        foreign.error().contains("invite not found"),
        "expected invite not found, got {:?}",
        foreign.error()
    );
    let raw = foreign.raw.to_ascii_lowercase();
    assert!(
        !raw.contains(&ws_a.to_ascii_lowercase()),
        "404 body must not include A's workspace id: {}",
        foreign.raw
    );
    assert!(
        !raw.contains(&guest.to_ascii_lowercase()),
        "404 body must not include the guest email: {}",
        foreign.raw
    );
    assert!(
        first_hex48(&foreign.raw).is_none(),
        "404 body must not include the invite token: {}",
        foreign.raw
    );
    if let Some(obj) = foreign.body.as_object() {
        for key in ["token", "invite_token", "inviteToken", "workspace_id"] {
            assert!(
                !obj.contains_key(key),
                "404 body must not include {key}: {}",
                foreign.body
            );
        }
    }

    let after_foreign = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/invites"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_invite_still_open(&after_foreign, &invite_id, "after B DELETE on B's path");

    let on_a = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws_a}/invites/{invite_id}"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        on_a.status,
        StatusCode::FORBIDDEN,
        "B DELETE on A's path must be 403 not-a-member: {}",
        on_a.body
    );
    assert!(
        on_a.error().contains("not a member"),
        "B on A's DELETE must say not-a-member, got {:?}",
        on_a.error()
    );
    assert!(
        !on_a
            .error()
            .to_ascii_lowercase()
            .contains("not allowed to revoke"),
        "must be membership 403, not can_manage 403: {:?}",
        on_a.error()
    );

    let after_403 = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/invites"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_invite_still_open(&after_403, &invite_id, "after B DELETE on A's path");

    let b_guest = format!("b.guest.{uuid}@example.test", uuid = Uuid::new_v4());
    let b_created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_b}/invites"),
        Some(&b_jar),
        Some(json!({ "email": b_guest, "role": "member" })),
    )
    .await;
    assert_eq!(
        b_created.status,
        StatusCode::CREATED,
        "control: B POST on B must 201: {}",
        b_created.body
    );
    let b_invite_id = b_created.body["invite"]["id"]
        .as_str()
        .expect("B invite.id")
        .to_string();
    assert_ne!(
        b_invite_id, invite_id,
        "control invite must be a different row"
    );

    let b_revoked = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws_b}/invites/{b_invite_id}"),
        Some(&b_jar),
        None,
    )
    .await;
    assert_eq!(
        b_revoked.status,
        StatusCode::OK,
        "control: B DELETE of B's invite must 200: {}",
        b_revoked.body
    );
    assert_eq!(
        b_revoked.body["revoked"], true,
        "control: B DELETE must revoke B's invite: {}",
        b_revoked.body
    );

    let still_a = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/invites"),
        Some(&a_jar),
        None,
    )
    .await;
    assert_invite_still_open(&still_a, &invite_id, "after B revokes B's own invite");

    let signed = signup(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(signed.status, StatusCode::CREATED, "{}", signed.body);
    let (guest_login, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(guest_login.status, StatusCode::OK, "{}", guest_login.body);

    let guest_listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(guest_listed.status, StatusCode::OK, "{}", guest_listed.body);
    let guest_ids = listed_workspace_ids(&guest_listed);
    assert!(
        guest_ids.contains(&ws_a.as_str()),
        "guest signup must still join A after foreign DELETE: {guest_ids:?}"
    );
    assert!(
        !guest_ids.contains(&ws_b.as_str()),
        "guest must not join B: {guest_ids:?}"
    );

    let guest_members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws_a}/members"),
        Some(&guest_jar),
        None,
    )
    .await;
    assert_eq!(
        guest_members.status,
        StatusCode::OK,
        "{}",
        guest_members.body
    );
    assert_eq!(
        guest_members.body["you"]["role"].as_str(),
        Some("member"),
        "guest should join as invited member: {}",
        guest_members.body
    );
}

#[tokio::test]
async fn invite_existing_member_is_already_a_member() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let (member_email, _member_jar, _) = signup_and_login(&app).await;

    let first = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert_eq!(first.body["added_existing_user"], true);

    let again = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);
    let msg = again.error().to_ascii_lowercase();
    assert!(
        msg.contains("already a member"),
        "expected already-a-member conflict, got {:?}",
        again.error()
    );
}

/// Partial unique index `workspace_invites_open_email_uidx` on
/// `(workspace_id, email_normalized)` WHERE accepted/revoked are null.
/// Re-POST of the same address (any case/whitespace, even another role)
/// while that row is still open must 409; list stays one invite.
#[tokio::test]
async fn second_open_invite_for_same_email_is_409() {
    let app = app().await;
    let (_email, jar, login) = signup_and_login(&app).await;
    let ws = workspace_id(&login);
    let guest = format!("Invite.{uuid}@Example.TEST", uuid = Uuid::new_v4());
    let normalized = guest.to_lowercase();

    let first = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        Some(json!({ "email": guest, "role": "member" })),
    )
    .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert_eq!(first.body["added_existing_user"], false);
    let first_id = first.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();
    assert_eq!(
        first.body["invite"]["email"].as_str(),
        Some(normalized.as_str()),
        "{}",
        first.body
    );
    assert_eq!(first.body["invite"]["role"].as_str(), Some("member"));

    let again = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        Some(json!({
            "email": format!("  {}  ", guest.to_ascii_uppercase()),
            "role": "admin"
        })),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);
    let msg = again.error().to_ascii_lowercase();
    assert!(
        msg.contains("open invite") && msg.contains("already exists"),
        "expected open-invite conflict, got {:?}",
        again.error()
    );

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let same: Vec<&serde_json::Value> = listed.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter(|i| i["email"].as_str() == Some(normalized.as_str()))
        .collect();
    assert_eq!(
        same.len(),
        1,
        "must remain a single open invite: {}",
        listed.body
    );
    assert_eq!(
        same[0]["id"].as_str(),
        Some(first_id.as_str()),
        "{}",
        listed.body
    );
    assert_eq!(
        same[0]["role"].as_str(),
        Some("member"),
        "duplicate must not replace the open invite: {}",
        listed.body
    );
}

/// The unique index is partial: after revoke, the same email can be
/// invited again. The new open row is listed; the revoked one is not.
#[tokio::test]
async fn revoked_invite_does_not_block_reinvite_same_email() {
    let app = app().await;
    let (_email, jar, login) = signup_and_login(&app).await;
    let ws = workspace_id(&login);
    let guest = format!("invite.re.{uuid}@example.test", uuid = Uuid::new_v4());

    let first = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        Some(json!({ "email": guest, "role": "member" })),
    )
    .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert_eq!(first.body["added_existing_user"], false);
    let first_id = first.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();

    let revoked = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/invites/{first_id}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
    assert_eq!(revoked.body["revoked"], true);

    let again = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        Some(json!({ "email": guest, "role": "admin" })),
    )
    .await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.body);
    assert_eq!(again.body["added_existing_user"], false);
    let second_id = again.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();
    assert_ne!(
        second_id, first_id,
        "re-invite must be a new row: {}",
        again.body
    );
    assert_eq!(again.body["invite"]["role"].as_str(), Some("admin"));

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids: Vec<&str> = listed.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert!(
        ids.contains(&second_id.as_str()),
        "new open invite must be listed: {ids:?}"
    );
    assert!(
        !ids.contains(&first_id.as_str()),
        "revoked invite must not stay open: {ids:?}"
    );
}

#[tokio::test]
async fn invite_role_owner_is_rejected() {
    let app = app().await;
    let (_email, jar, login_out) = signup_and_login(&app).await;
    let ws = workspace_id(&login_out);

    let out = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        Some(json!({ "email": unique_email(), "role": "owner" })),
    )
    .await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    let msg = out.error().to_ascii_lowercase();
    assert!(
        msg.contains("admin") && msg.contains("member"),
        "expected invite role must be admin or member, got {:?}",
        out.error()
    );
}

#[tokio::test]
async fn owner_get_invites_omits_token() {
    let app = app().await;
    let (_email, jar, login_out) = signup_and_login(&app).await;
    let ws = workspace_id(&login_out);

    let inv = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        Some(json!({ "email": unique_email(), "role": "member" })),
    )
    .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.body);

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let invites = listed.body["invites"].as_array().expect("invites array");
    assert!(
        !invites.is_empty(),
        "need at least one invite to check token leakage: {}",
        listed.body
    );
    for invite in invites {
        let obj = invite
            .as_object()
            .unwrap_or_else(|| panic!("invite object: {invite}"));
        assert!(
            !obj.contains_key("token"),
            "invite must not include token: {invite}"
        );
        assert!(
            !obj.contains_key("invite_token"),
            "invite must not include invite_token: {invite}"
        );
    }
}

fn invite_omits_token_keys(invite: &serde_json::Value, when: &str) {
    let obj = invite
        .as_object()
        .unwrap_or_else(|| panic!("{when} invite must be an object: {invite}"));
    for key in ["token", "invite_token", "inviteToken"] {
        assert!(
            !obj.contains_key(key),
            "{when} invite must not include {key}: {invite}"
        );
    }
}

/// Consecutive ASCII hex run of length ≥ 48 — the stored invite token is
/// `hex::encode([u8; 24])`. Drive this against `HttpOut.raw`, not a
/// re-serialized `Value`.
fn first_hex48(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let is_hex = |b: u8| b.is_ascii_hexdigit();
    let mut i = 0;
    while i < bytes.len() {
        if is_hex(bytes[i]) {
            let start = i;
            while i < bytes.len() && is_hex(bytes[i]) {
                i += 1;
            }
            if i - start >= 48 {
                return Some(raw[start..start + 48].to_string());
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Invites are accepted by matching email, not a magic-link token.
/// `create_invite` still mints a 24-byte hex token; the 201 body must not
/// return it. `owner_get_invites_omits_token` only asserts GET /invites.
#[tokio::test]
async fn invite_create_omits_token() {
    let app = app().await;
    let (_owner_email, jar, login_out) = signup_and_login(&app).await;
    let ws = workspace_id(&login_out);
    let email = unique_email();

    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        Some(json!({ "email": email, "role": "member" })),
    )
    .await;
    assert_ne!(
        created.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "POST /invites must not 500: {}",
        created.body
    );
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(
        created.body["added_existing_user"], false,
        "{}",
        created.body
    );

    let invite = &created.body["invite"];
    invite_omits_token_keys(invite, "POST /invites 201");
    let create_secret = first_hex48(&created.raw);
    assert!(
        create_secret.is_none(),
        "POST /invites 201 raw body must not contain a 48-char hex string (stored invite token): {}",
        created.raw
    );

    let invite_id = invite["id"]
        .as_str()
        .unwrap_or_else(|| panic!("control: 201 must include invite.id: {}", created.body));
    Uuid::parse_str(invite_id)
        .unwrap_or_else(|e| panic!("control: invite.id must be a uuid ({e}): {}", created.body));
    assert_eq!(
        invite["email"].as_str(),
        Some(email.as_str()),
        "control: 201 must include invite.email, not an empty create body: {}",
        created.body
    );
    assert_eq!(
        invite["role"].as_str(),
        Some("member"),
        "control: 201 must include invite.role: {}",
        created.body
    );
    let expires_at = invite["expires_at"].as_str().unwrap_or_else(|| {
        panic!(
            "control: 201 must include invite.expires_at: {}",
            created.body
        )
    });
    DateTime::parse_from_rfc3339(expires_at).unwrap_or_else(|e| {
        panic!(
            "control: invite.expires_at must be rfc3339 ({e}): {}",
            created.body
        )
    });

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let invites = listed.body["invites"]
        .as_array()
        .unwrap_or_else(|| panic!("invites array: {}", listed.body));
    let mut listed_ids = Vec::new();
    for row in invites {
        invite_omits_token_keys(row, "GET /invites");
        if let Some(id) = row["id"].as_str() {
            listed_ids.push(id.to_string());
        }
    }
    assert!(
        listed_ids.iter().any(|id| id == invite_id),
        "control: GET /invites must still list the created id {invite_id}: {}",
        listed.body
    );
    if let Some(secret) = create_secret.as_deref() {
        assert!(
            !listed.raw.contains(secret),
            "GET /invites must not contain the create-response token: {}",
            listed.raw
        );
    }
}

#[tokio::test]
async fn admin_cannot_invite_admin_can_invite_member() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let (member_email, admin_jar, member_login) = signup_and_login(&app).await;
    let member_id = user_id(&member_login);

    let added = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{member_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);
    assert_eq!(
        promoted.body["role"].as_str(),
        Some("admin"),
        "{}",
        promoted.body
    );

    let invite_admin = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&admin_jar),
        Some(json!({ "email": unique_email(), "role": "admin" })),
    )
    .await;
    assert_eq!(
        invite_admin.status,
        StatusCode::FORBIDDEN,
        "{}",
        invite_admin.body
    );
    let msg = invite_admin.error().to_ascii_lowercase();
    assert!(
        msg.contains("admins can only invite members"),
        "expected admins-can-only-invite-members, got {:?}",
        invite_admin.error()
    );

    let invite_member = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&admin_jar),
        Some(json!({ "email": unique_email(), "role": "member" })),
    )
    .await;
    assert_eq!(
        invite_member.status,
        StatusCode::CREATED,
        "{}",
        invite_member.body
    );
    assert_eq!(invite_member.body["added_existing_user"], false);
}

#[tokio::test]
async fn admin_cannot_delete_owner_or_patch_roles() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let owner_id = user_id(&owner_login);
    let (member_email, admin_jar, member_login) = signup_and_login(&app).await;
    let admin_id = user_id(&member_login);

    let added = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{admin_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);

    let (other_email, _other_jar, other_login) = signup_and_login(&app).await;
    let other_id = user_id(&other_login);
    let other_added = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": other_email, "role": "member" })),
    )
    .await;
    assert_eq!(
        other_added.status,
        StatusCode::CREATED,
        "{}",
        other_added.body
    );

    let delete_owner = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/members/{owner_id}"),
        Some(&admin_jar),
        None,
    )
    .await;
    assert_eq!(
        delete_owner.status,
        StatusCode::FORBIDDEN,
        "{}",
        delete_owner.body
    );

    for (label, target) in [
        ("owner", owner_id.as_str()),
        ("admin-self", admin_id.as_str()),
        ("member", other_id.as_str()),
    ] {
        let patch = call(
            &app,
            "PATCH",
            &format!("/api/workspaces/{ws}/members/{target}"),
            Some(&admin_jar),
            Some(json!({ "role": "member" })),
        )
        .await;
        assert_eq!(
            patch.status,
            StatusCode::FORBIDDEN,
            "{label}: {}",
            patch.body
        );
        let msg = patch.error().to_ascii_lowercase();
        assert!(
            msg.contains("only an owner"),
            "{label}: expected only-an-owner, got {:?}",
            patch.error()
        );
    }

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let roles: Vec<(&str, &str)> = members.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|m| Some((m["user_id"].as_str()?, m["role"].as_str()?)))
        .collect();
    assert!(
        roles
            .iter()
            .any(|(id, role)| *id == owner_id && *role == "owner"),
        "owner must remain owner: {roles:?}"
    );
}

/// A member must not escalate via PATCH self (or a peer). Only an owner
/// can change roles; invalid roles 400 and must not persist. If the gate
/// became "self or can_manage", `admin_cannot_delete_owner_or_patch_roles`
/// would still pass and a member could promote to owner.
#[tokio::test]
async fn member_cannot_promote_self() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let (member_email, member_jar, member_login) = signup_and_login(&app).await;
    let member_id = user_id(&member_login);
    add_existing_user(&app, &owner_jar, &ws, &member_email, "member").await;

    let (peer_email, _peer_jar, peer_login) = signup_and_login(&app).await;
    let peer_id = user_id(&peer_login);
    add_existing_user(&app, &owner_jar, &ws, &peer_email, "member").await;

    for role in ["owner", "admin"] {
        let patch = call(
            &app,
            "PATCH",
            &format!("/api/workspaces/{ws}/members/{member_id}"),
            Some(&member_jar),
            Some(json!({ "role": role })),
        )
        .await;
        assert_eq!(
            patch.status,
            StatusCode::FORBIDDEN,
            "member PATCH self to {role} must be 403, not 200: {}",
            patch.body
        );
        assert_ne!(
            patch.status,
            StatusCode::OK,
            "member PATCH self to {role} must not succeed: {}",
            patch.body
        );
        let msg = patch.error().to_ascii_lowercase();
        assert!(
            msg.contains("only an owner"),
            "member PATCH self to {role}: expected only-an-owner, got {:?}",
            patch.error()
        );
    }

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let roles = member_roles(&listed);
    assert_eq!(
        roles.get(member_id.as_str()).copied(),
        Some("member"),
        "self-promote must not persist: {roles:?}"
    );

    let peer_patch = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{peer_id}"),
        Some(&member_jar),
        Some(json!({ "role": "owner" })),
    )
    .await;
    assert_eq!(
        peer_patch.status,
        StatusCode::FORBIDDEN,
        "member PATCH peer to owner must be 403: {}",
        peer_patch.body
    );
    let msg = peer_patch.error().to_ascii_lowercase();
    assert!(
        msg.contains("only an owner"),
        "member PATCH peer: expected only-an-owner, got {:?}",
        peer_patch.error()
    );

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let roles = member_roles(&listed);
    assert_eq!(
        roles.get(peer_id.as_str()).copied(),
        Some("member"),
        "peer must stay member: {roles:?}"
    );
    assert_eq!(
        roles.get(member_id.as_str()).copied(),
        Some("member"),
        "actor must stay member after peer PATCH: {roles:?}"
    );

    for bad in ["superadmin", "", "root"] {
        let patch = call(
            &app,
            "PATCH",
            &format!("/api/workspaces/{ws}/members/{member_id}"),
            Some(&owner_jar),
            Some(json!({ "role": bad })),
        )
        .await;
        assert_eq!(
            patch.status,
            StatusCode::BAD_REQUEST,
            "owner PATCH role={bad:?} must be 400: {}",
            patch.body
        );
        let msg = patch.error().to_ascii_lowercase();
        assert!(
            msg.contains("role must be owner, admin, or member"),
            "owner PATCH role={bad:?}: expected role must be owner, admin, or member, got {:?}",
            patch.error()
        );
    }

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let roles = member_roles(&listed);
    assert_eq!(
        roles.get(member_id.as_str()).copied(),
        Some("member"),
        "invalid role must not persist: {roles:?}"
    );

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{member_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(
        promoted.status,
        StatusCode::OK,
        "owner PATCH member to admin is the control that PATCH is not always 403: {}",
        promoted.body
    );
    assert_eq!(
        promoted.body["role"].as_str(),
        Some("admin"),
        "{}",
        promoted.body
    );
    assert_eq!(
        promoted.body["user_id"].as_str(),
        Some(member_id.as_str()),
        "{}",
        promoted.body
    );

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let roles = member_roles(&listed);
    assert_eq!(
        roles.get(member_id.as_str()).copied(),
        Some("admin"),
        "owner promote control must persist: {roles:?}"
    );
    assert_eq!(
        roles.get(peer_id.as_str()).copied(),
        Some("member"),
        "peer must remain member after owner control promote: {roles:?}"
    );
}
