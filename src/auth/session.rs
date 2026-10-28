use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::app::AppState;
use crate::auth::user::{TOKEN_COOKIE, User};
use crate::config::Config;
use crate::error::AppError;
use crate::repository::Repository;

/// Nome do cookie que guarda o refresh token no navegador.
pub const REFRESH_COOKIE: &str = "refresh_token";

/// O refresh token em texto claro — só existe em memória e no cookie do
/// navegador. No banco vai apenas a hash SHA-256 (ver a migração de `sessions`):
/// um vazamento do banco não vaza tokens utilizáveis.
pub struct RefreshToken(String);

impl RefreshToken {
    /// 32 bytes de aleatoriedade do SO. Opaco de propósito: diferente do JWT,
    /// não carrega dado nenhum — todo o significado está na linha de `sessions`.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self(URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn hash(&self) -> Vec<u8> {
        hash_token(&self.0)
    }
}

/// SHA-256 do valor do token, como guardado em `sessions.token_hash`.
pub fn hash_token(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}

/// Cookie do token de ACESSO (JWT curto). `Max-Age` usa o TTL nominal do
/// token; a validação da assinatura admite apenas a tolerância de relógio
/// explícita em `User::from_auth_token`.
pub fn access_cookie(user: &User, config: &Config) -> Result<Cookie<'static>, AppError> {
    Ok(Cookie::build((
        TOKEN_COOKIE,
        user.auth_token(&config.jwt_secret, config.session_ttl_minutes)?,
    ))
    .http_only(true)
    .same_site(SameSite::Strict)
    .secure(config.cookie_secure)
    .path("/")
    .max_age(time::Duration::minutes(config.session_ttl_minutes as i64))
    .build())
}

/// Cookie do REFRESH token (longo). Quando o acesso expira, é ele que renova a
/// sessão — com rotação: cada uso queima o token antigo e emite um novo.
pub fn refresh_cookie(token: &RefreshToken, config: &Config) -> Cookie<'static> {
    Cookie::build((REFRESH_COOKIE, token.0.clone()))
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(config.cookie_secure)
        .path("/")
        .max_age(time::Duration::days(config.refresh_ttl_days as i64))
        .build()
}

/// Instante de expiração de uma sessão criada agora, segundo a configuração.
pub fn session_expiry(config: &Config) -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc() + time::Duration::days(config.refresh_ttl_days as i64)
}

/// Middleware que mantém a sessão viva sem novo login: se o token de acesso
/// expirou mas o refresh token ainda é válido no banco, ROTACIONA a sessão
/// (revoga a linha antiga, cria uma nova) e emite os dois cookies renovados na
/// resposta. O `User` renovado vai nas extensions da requisição, de onde o
/// extrator o recupera — o handler nem fica sabendo que houve renovação.
/// Login, cadastro e logout controlam seus próprios cookies e não renovam a
/// sessão anterior.
pub async fn refresh_session(
    State(state): State<AppState>,
    jar: CookieJar,
    mut request: Request,
    next: Next,
) -> Response {
    // Estas rotas criam ou encerram sessões. Renovar antes delas consumiria o
    // refresh que o logout precisa revogar; depois do handler, os Set-Cookie
    // da renovação ainda poderiam sobrescrever a remoção ou a nova conta.
    if matches!(request.uri().path(), "/login" | "/register" | "/logout") {
        return next.run(request).await;
    }

    // Acesso ainda válido? Nada a fazer.
    let has_valid_access = jar
        .get(TOKEN_COOKIE)
        .is_some_and(|c| User::from_auth_token(c.value(), &state.config.jwt_secret).is_ok());
    if has_valid_access {
        return next.run(request).await;
    }

    let Some(refresh) = jar.get(REFRESH_COOKIE) else {
        return next.run(request).await;
    };

    let repository = Repository::from_state(&state);
    let new_token = RefreshToken::generate();
    let rotated = repository
        .rotate_session(
            &hash_token(refresh.value()),
            &new_token.hash(),
            session_expiry(&state.config),
        )
        .await;

    match rotated {
        Ok(Some(identity)) => {
            let user = User::new(identity.id, identity.username, identity.role);
            let Ok(access) = access_cookie(&user, &state.config) else {
                // Falhou assinar o novo JWT: segue sem renovar; o extrator
                // devolve o 401/redirect normal.
                return next.run(request).await;
            };

            request.extensions_mut().insert(user);
            let response = next.run(request).await;

            // A jar emite Set-Cookie só para o que foi adicionado a ela — os
            // cookies que o próprio handler setou na resposta são preservados.
            (
                jar.add(access)
                    .add(refresh_cookie(&new_token, &state.config)),
                response,
            )
                .into_response()
        }
        // Sessão inexistente, revogada ou expirada — ou erro de banco: segue o
        // fluxo normal (o extrator manda para o login).
        _ => next.run(request).await,
    }
}
