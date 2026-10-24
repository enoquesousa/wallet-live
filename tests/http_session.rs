//! Contrato do ciclo de sessão pelo router de produção e Postgres real.
//! As regressões dependem da ordem entre middleware e handler: é preciso
//! conferir todos os Set-Cookie e o estado persistido, não só o redirect.

mod common;

use axum::http::{StatusCode, header};
use axum::response::Response;
use axum_extra::extract::cookie::Cookie;
use common::{
    app, cookie_value, cookies_from, get, get_with_cookies, post_form, register, send, text,
};
use jwt_simple::prelude::{Clock, Duration, HS256Key, MACLike};
use wallet::auth::csrf::CSRF_COOKIE;
use wallet::auth::session::{REFRESH_COOKIE, hash_token};
use wallet::auth::user::TOKEN_COOKIE;

/// O refresh continua transparente nas telas privadas. O access expirado há
/// 60 s também precisa renovar: a antiga tolerância implícita de 15 min o
/// aceitava como válido e deixava o prazo real maior que o documentado.
#[sqlx::test]
async fn protected_requests_rotate_missing_or_expired_access_tokens(db: sqlx::PgPool) {
    let app = app(db.clone());

    for expired in [false, true] {
        let username = if expired { "expired" } else { "missing" };
        let session = register(&app, username, "senha-de-teste-123").await;
        let old_refresh = request_cookie(&session, REFRESH_COOKIE);
        let cookies = cookies_without_valid_access(&session, expired);

        let response = send(&app, get_with_cookies("/assets", &cookies)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_one_session_pair(&response);
        let new_refresh = cookie_value(&response, REFRESH_COOKIE).expect("refresh renovado");
        assert_ne!(new_refresh, old_refresh);
        assert_eq!(
            active_refresh_user(&db, &new_refresh).await.as_deref(),
            Some(username)
        );
        assert!(text(response).await.contains(username));
        assert_eq!(session_counts(&db, username).await, (2, 1));

        let replay = send(
            &app,
            get_with_cookies("/assets", &format!("{REFRESH_COOKIE}={old_refresh}")),
        )
        .await;
        assert_eq!(replay.status(), StatusCode::SEE_OTHER);
        assert_eq!(replay.headers()[header::LOCATION], "/login");
        assert!(auth_cookies(&replay).is_empty());
    }
}

/// Logout precisa queimar o refresh recebido, não um token já consumido pelo
/// middleware. Nenhum Set-Cookie posterior pode recriar a sessão encerrada.
#[sqlx::test]
async fn logout_never_renews_a_missing_or_expired_access_token(db: sqlx::PgPool) {
    let app = app(db.clone());

    for expired in [false, true] {
        let username = if expired { "expired" } else { "missing" };
        let session = register(&app, username, "senha-de-teste-123").await;
        let cookies = cookies_without_valid_access(&session, expired);
        let response = send(&app, get_with_cookies("/logout", &cookies)).await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], "/login");
        let removed = auth_cookies(&response);
        assert_eq!(removed.len(), if expired { 2 } else { 1 });
        assert!(removed.iter().any(|cookie| cookie.name() == REFRESH_COOKIE));
        for cookie in removed {
            assert!(
                cookie.value().is_empty(),
                "logout não pode emitir credenciais"
            );
            assert_eq!(cookie.path(), Some("/"));
            assert_eq!(cookie.max_age(), Some(time::Duration::ZERO));
        }
        assert_eq!(session_counts(&db, username).await, (1, 0));

        let replay = send(&app, get_with_cookies("/assets", &cookies)).await;
        assert_eq!(replay.status(), StatusCode::SEE_OTHER);
        assert_eq!(replay.headers()[header::LOCATION], "/login");
        assert!(auth_cookies(&replay).is_empty());
    }
}

/// Ao entrar ou se cadastrar como outra pessoa, só o handler pode escolher os
/// novos cookies. O middleware não pode anexar a conta anterior à resposta.
#[sqlx::test]
async fn login_and_registration_do_not_restore_the_previous_account(db: sqlx::PgPool) {
    let app = app(db.clone());
    register(&app, "login_target", "senha-de-teste-123").await;

    for (path, previous, target) in [
        ("/login", "before_login", "login_target"),
        ("/register", "before_register", "registered_target"),
    ] {
        let session = register(&app, previous, "senha-de-teste-123").await;
        let form = send(&app, get(path)).await;
        let csrf = cookie_value(&form, CSRF_COOKIE).expect("CSRF do formulário");
        let cookies = format!(
            "{}; {CSRF_COOKIE}={csrf}",
            cookies_without_valid_access(&session, false)
        );
        let response = send(
            &app,
            post_form(
                path,
                &cookies,
                &format!("username={target}&password=senha-de-teste-123&csrf_token={csrf}"),
            ),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], "/");
        assert_one_session_pair(&response);
        let refresh = cookie_value(&response, REFRESH_COOKIE).expect("refresh da nova conta");
        assert_eq!(
            active_refresh_user(&db, &refresh).await.as_deref(),
            Some(target)
        );
        assert_eq!(session_counts(&db, previous).await, (1, 1));

        let page = send(&app, get_with_cookies("/assets", &cookies_from(&response))).await;
        assert_eq!(page.status(), StatusCode::OK);
        let html = text(page).await;
        assert!(html.contains(target));
        assert!(
            !html.contains(previous),
            "a resposta restaurou a identidade anterior"
        );
    }
}

/// A exclusão abrange GET e os caminhos de erro: visitar o formulário ou errar
/// senha/CSRF não pode consumir o refresh antigo nem criar sessões escondidas.
#[sqlx::test]
async fn authentication_pages_and_rejected_forms_do_not_rotate_refresh_tokens(db: sqlx::PgPool) {
    let app = app(db.clone());
    let session = register(&app, "breno", "senha-de-teste-123").await;
    let refresh_only = cookies_without_valid_access(&session, false);

    for path in ["/login", "/register"] {
        let response = send(&app, get_with_cookies(path, &refresh_only)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(auth_cookies(&response).is_empty());
    }

    let form = send(&app, get("/login")).await;
    let csrf = cookie_value(&form, CSRF_COOKIE).expect("CSRF do formulário");
    let cookies = format!("{refresh_only}; {CSRF_COOKIE}={csrf}");

    for (path, password, submitted_csrf) in [
        ("/login", "senha-incorreta", csrf.as_str()),
        ("/register", "senha-de-teste-123", csrf.as_str()),
        ("/login", "senha-de-teste-123", "csrf-invalido"),
        ("/register", "senha-de-teste-123", "csrf-invalido"),
    ] {
        let response = send(
            &app,
            post_form(
                path,
                &cookies,
                &format!("username=breno&password={password}&csrf_token={submitted_csrf}"),
            ),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], path);
        assert!(auth_cookies(&response).is_empty());
    }
    assert_eq!(session_counts(&db, "breno").await, (1, 1));
}

/// O contrato é JWT stateless: logout revoga o refresh, mas uma cópia do access
/// já emitido continua aceita até expirar. Um access vencido há 60 s é recusado,
/// pois já passou da tolerância explícita de 30 s.
#[sqlx::test]
async fn logout_revokes_refresh_but_access_lasts_until_its_expiry(db: sqlx::PgPool) {
    let app = app(db.clone());
    let session = register(&app, "breno", "senha-de-teste-123").await;
    let response = send(&app, get_with_cookies("/logout", &session)).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(session_counts(&db, "breno").await, (1, 0));

    let copied_access = format!("{TOKEN_COOKIE}={}", request_cookie(&session, TOKEN_COOKIE));
    let page = send(&app, get_with_cookies("/assets", &copied_access)).await;
    assert_eq!(page.status(), StatusCode::OK);
    assert!(auth_cookies(&page).is_empty());

    for cookies in [
        cookies_without_valid_access(&session, false),
        cookies_without_valid_access(&session, true),
    ] {
        let response = send(&app, get_with_cookies("/assets", &cookies)).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], "/login");
        assert!(auth_cookies(&response).is_empty());
    }
}

fn request_cookie<'a>(cookies: &'a str, name: &str) -> &'a str {
    cookies
        .split(';')
        .map(str::trim)
        .find_map(|pair| pair.split_once('=').filter(|(key, _)| *key == name))
        .map(|(_, value)| value)
        .expect("cookie emitido pelo registro")
}

/// Reassina claims reais com expiração no passado, sem sleep e sem depender da
/// tolerância padrão da biblioteca. A assinatura e a identidade seguem válidas.
fn cookies_without_valid_access(session: &str, expired: bool) -> String {
    let refresh = format!(
        "{REFRESH_COOKIE}={}",
        request_cookie(session, REFRESH_COOKIE)
    );
    if !expired {
        return refresh;
    }

    let config = common::test_config(String::new());
    let key = HS256Key::from_bytes(config.jwt_secret.as_bytes());
    let mut claims = key
        .verify_token::<serde_json::Value>(request_cookie(session, TOKEN_COOKIE), None)
        .expect("JWT recém-emitido pelo registro");
    let expires_at = Clock::now_since_epoch() - Duration::from_secs(60);
    claims.issued_at = Some(expires_at - Duration::from_mins(10));
    claims.invalid_before = claims.issued_at;
    claims.expires_at = Some(expires_at);
    let expired_token = key.authenticate(claims).expect("JWT de teste assinado");
    format!("{refresh}; {TOKEN_COOKIE}={expired_token}")
}

/// Lê TODOS os Set-Cookie. Procurar apenas o primeiro esconderia justamente o
/// cookie que o middleware anexava depois do login/logout.
fn auth_cookies(response: &Response) -> Vec<Cookie<'static>> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| {
            Cookie::parse(value.to_str().expect("Set-Cookie válido").to_owned())
                .expect("cookie válido")
        })
        .filter(|cookie| matches!(cookie.name(), TOKEN_COOKIE | REFRESH_COOKIE))
        .collect()
}

fn assert_one_session_pair(response: &Response) {
    let cookies = auth_cookies(response);
    assert_eq!(
        cookies.len(),
        2,
        "a resposta deve emitir apenas um par de sessão"
    );
    for name in [TOKEN_COOKIE, REFRESH_COOKIE] {
        let matching: Vec<_> = cookies
            .iter()
            .filter(|cookie| cookie.name() == name)
            .collect();
        assert_eq!(matching.len(), 1, "cookie duplicado: {name}");
        assert!(!matching[0].value().is_empty());
    }
}

async fn session_counts(db: &sqlx::PgPool, username: &str) -> (i64, i64) {
    sqlx::query_as(
        "SELECT COUNT(*), COUNT(*) FILTER (WHERE s.revoked_at IS NULL AND s.expires_at > NOW()) \
         FROM sessions s JOIN users u ON u.id = s.user_id WHERE u.username = $1",
    )
    .bind(username)
    .fetch_one(db)
    .await
    .expect("contagem das sessões do usuário")
}

async fn active_refresh_user(db: &sqlx::PgPool, refresh: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT u.username FROM sessions s JOIN users u ON u.id = s.user_id \
         WHERE s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > NOW()",
    )
    .bind(hash_token(refresh))
    .fetch_optional(db)
    .await
    .expect("identidade do refresh ativo")
}
