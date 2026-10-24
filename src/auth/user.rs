use std::convert::Infallible;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::cookie::CookieJar;
use jwt_simple::prelude::*;
use password_auth::VerifyError;

use crate::app::AppState;
use crate::error::AppError;
use crate::repository::Repository;

/// Nome do cookie onde o token de acesso é guardado no navegador.
pub const TOKEN_COOKIE: &str = "token";

/// Dados que embutimos no JWT (as "claims" customizadas). Vira JSON, então
/// precisa de Serialize/Deserialize. NÃO é criptografado — qualquer um lê o
/// conteúdo; a assinatura é que prova a autenticidade.
#[derive(Serialize, Deserialize)]
struct UserClaims {
    id: i64,
    username: String,
    role: String,
}

/// Um usuário já autenticado no sistema. Diferente do `Admin`, ele carrega dados
/// (id e nome) que usaremos depois para acessar as relações do usuário. Os campos
/// são privados de propósito: a única forma de obter um `User` é passando por um
/// dos fluxos de autenticação abaixo, então um `User` em mãos é prova de que o
/// fluxo foi cumprido. Não guardamos a senha nem a hash: já foi autenticado.
#[derive(Clone)]
pub struct User {
    id: i64,
    username: String,
    role: String,
}

impl User {
    /// Visível só dentro da crate: além dos fluxos de senha abaixo, o módulo de
    /// sessão reconstrói um `User` ao rotacionar um refresh token válido.
    pub(crate) fn new(id: i64, username: String, role: String) -> Self {
        Self { id, username, role }
    }

    pub const fn id(&self) -> i64 {
        self.id
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    /// O papel vem das claims do JWT — assinadas, então tão confiáveis quanto o
    /// resto do token. Rebaixar um admin só surte efeito em um novo token;
    /// os já emitidos continuam válidos até expirar, com a tolerância de
    /// relógio da validação. Revogar o refresh não revoga esses JWTs.
    pub fn is_admin(&self) -> bool {
        self.role == crate::models::ROLE_ADMIN
    }

    /// Gera o JWT de acesso deste usuário, assinado com `secret` e válido por
    /// `ttl_minutes` (vem da `Config`; curto de propósito — a longevidade da
    /// sessão é papel do refresh token, ver `auth::session`).
    pub fn auth_token(&self, secret: &str, ttl_minutes: u64) -> Result<String, AppError> {
        let key = HS256Key::from_bytes(secret.as_bytes());
        let claims = Claims::with_custom_claims(
            UserClaims {
                id: self.id,
                username: self.username.clone(),
                role: self.role.clone(),
            },
            Duration::from_mins(ttl_minutes),
        );
        let token = key.authenticate(claims)?;
        Ok(token)
    }

    /// Reconstrói um `User` a partir de um token. Usa a mesma chave para VALIDAR
    /// a assinatura: se o token foi fabricado/adulterado ou expirou além da
    /// tolerância de 30 s, falha. Só depois confiamos no conteúdo das claims.
    pub(crate) fn from_auth_token(token: &str, secret: &str) -> Result<Self, AppError> {
        let key = HS256Key::from_bytes(secret.as_bytes());
        // O padrão da biblioteca admite 15 minutos extras, mais que o TTL
        // nominal de acesso. Limitamos a folga para pequenos desvios de relógio.
        let options = VerificationOptions {
            time_tolerance: Some(Duration::from_secs(30)),
            ..Default::default()
        };
        let claims = key.verify_token::<UserClaims>(token, Some(options))?;
        Ok(Self::new(
            claims.custom.id,
            claims.custom.username,
            claims.custom.role,
        ))
    }
}

/// Extrai um `User` autenticado da requisição: lê o cookie `token` (que o
/// navegador reenvia automaticamente) e o valida. Basta anotar um handler com um
/// parâmetro `User` para exigir sessão válida.
impl FromRequestParts<AppState> for User {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // O middleware de sessão pode já ter renovado o usuário nesta mesma
        // requisição (access expirado + refresh válido): ele fica nas extensions.
        if let Some(user) = parts.extensions.get::<User>() {
            return Ok(user.clone());
        }

        // A cookie jar é só uma leitura dos headers da requisição.
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar
            .get(TOKEN_COOKIE)
            .ok_or(AppError::MissingAuthorization)?
            .value();

        Self::from_auth_token(token, &state.config.jwt_secret)
    }
}

/// Versão tolerante: nunca falha. Útil em telas que devem redirecionar para o
/// login quando não há sessão válida, em vez de devolver um erro. Token ausente,
/// inválido ou expirado viram `None`.
impl FromRequestParts<AppState> for Option<User> {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(User::from_request_parts(parts, state).await.ok())
    }
}

/// Um usuário que ainda NÃO foi autenticado: tem só o nome e a senha em texto
/// livre que vieram do formulário. Pode virar um `User` de duas formas: provando
/// a senha de um usuário existente (`authenticate`) ou se cadastrando como um
/// usuário novo (`register`).
pub struct UnauthenticatedUser {
    username: String,
    password: String,
}

impl UnauthenticatedUser {
    pub fn new(username: String, password: String) -> Self {
        Self {
            username: username.trim().to_string(),
            password,
        }
    }

    /// Tenta autenticar este usuário contra o banco. Encapsulamos toda a lógica
    /// aqui (puxar o registro, conferir a hash) para que os endpoints não
    /// precisem conhecer esses detalhes — eles só repassam o `Repository`.
    pub async fn authenticate(&self, repository: &Repository) -> Result<User, AppError> {
        if self.username.is_empty() || self.password.is_empty() {
            return Err(AppError::InvalidCredentials);
        }

        // Se o usuário não existe, sinalizamos com um erro próprio: o endpoint o
        // trata para decidir entre login e cadastro.
        let user_record = match repository.get_user_by_name(&self.username).await? {
            Some(user_record) => user_record,
            None => return Err(AppError::UserDoesNotExist),
        };

        // Não sabemos (by design) como a hash é verificada — delegamos para a
        // biblioteca, que escolhe o algoritmo adequado.
        match password_auth::verify_password(&self.password, &user_record.password_hash) {
            Ok(()) => Ok(User::new(
                user_record.id,
                user_record.username,
                user_record.role,
            )),
            Err(VerifyError::PasswordInvalid) => Err(AppError::InvalidCredentials),
            // A hash guardada inclui o algoritmo usado; se ela não parseia, algo
            // muito errado aconteceu (usuário inserido por outra via). Melhor
            // derrubar o programa do que continuar num estado inconsistente.
            Err(VerifyError::Parse(error)) => {
                panic!("failed to parse stored password hash: {error}");
            }
        }
    }

    /// Cadastra este usuário como novo. Consome `self` para mover os dados direto
    /// para o banco. Gera a hash da senha aqui — o repository nunca vê texto livre.
    pub async fn register(self, repository: &Repository) -> Result<User, AppError> {
        if !valid_registration(&self.username, &self.password) {
            return Err(AppError::InvalidRegistration);
        }

        let password_hash = password_auth::generate_hash(&self.password);

        match repository.add_user(self.username, password_hash).await {
            Ok(user_record) => Ok(User::new(
                user_record.id,
                user_record.username,
                user_record.role,
            )),
            // O username é UNIQUE: violar a constraint vira um erro específico e
            // mais útil do que um 500 genérico de banco.
            Err(sqlx::Error::Database(db_error)) if db_error.is_unique_violation() => {
                Err(AppError::UsernameTaken)
            }
            Err(error) => Err(AppError::Database(error)),
        }
    }
}

fn valid_registration(username: &str, password: &str) -> bool {
    let username_len = username.chars().count();
    let password_len = password.chars().count();
    (3..=32).contains(&username_len) && (8..=128).contains(&password_len)
}

#[cfg(test)]
mod tests {
    use super::valid_registration;

    #[test]
    fn registration_requires_a_reasonable_username_and_password() {
        assert!(valid_registration("alice", "correct horse"));
        assert!(!valid_registration("ab", "correct horse"));
        assert!(!valid_registration("alice", "short"));
        assert!(!valid_registration(&"a".repeat(33), "correct horse"));
        assert!(!valid_registration("alice", &"x".repeat(129)));
    }
}
