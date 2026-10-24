use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use subtle::ConstantTimeEq;

use crate::app::AppState;
use crate::auth::user::User;
use crate::error::AppError;

/// Extrator de autorização administrativa. Como o Axum exige construir todos os
/// extratores antes de chamar o handler, basta anotar um endpoint com um
/// parâmetro do tipo `Admin` para protegê-lo: sem autorização válida, o código
/// do handler nem chega a rodar.
///
/// Duas credenciais são aceitas:
///
///  1. **Sessão com papel de admin** — um usuário autenticado cujo `role` (que
///     viaja assinado nas claims do JWT) é `admin`. É o caminho preferido: a
///     autorização deriva da identidade e é atribuível ao usuário. Revogar o
///     refresh impede a renovação, mas o JWT emitido mantém o papel até expirar.
///     Nota: o cookie de sessão é `SameSite=Strict`, o que também
///     blinda estes endpoints contra CSRF vindo de outros sites.
///  2. **Secret key no header `Authorization`** — credencial de serviço para
///     integrações máquina-a-máquina (a chave vem da `Config`, lida do ambiente
///     na inicialização).
pub struct Admin;

impl FromRequestParts<AppState> for Admin {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // Caminho 1: sessão de um usuário com papel de admin.
        if let Ok(user) = User::from_request_parts(parts, state).await {
            if user.is_admin() {
                return Ok(Admin);
            }
            // Usuário logado mas sem o papel: não adianta cair no header — ele
            // claramente está usando a sessão. Negar já.
            return Err(AppError::InvalidCredentials);
        }

        // Caminho 2: secret key de serviço no header.
        let authorization = parts
            .headers
            .get(AUTHORIZATION)
            .ok_or(AppError::MissingAuthorization)?;

        let provided = authorization
            .to_str()
            .map_err(|_| AppError::InvalidCredentials)?;

        // `ct_eq` compara em tempo constante: não vaza, pelo tempo de resposta,
        // quantos bytes do segredo bateram antes de divergir.
        if provided
            .as_bytes()
            .ct_eq(state.config.admin_secret_key.as_bytes())
            .into()
        {
            Ok(Admin)
        } else {
            Err(AppError::InvalidCredentials)
        }
    }
}
