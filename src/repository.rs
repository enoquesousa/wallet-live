use std::collections::HashMap;
use std::convert::Infallible;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use rust_decimal::Decimal;
use sqlx::{PgPool, Postgres, Transaction as SqlxTransaction};

use crate::app::AppState;
use crate::error::AppError;
use crate::models::{
    Asset, Holding, MONEY_SCALE, PortfolioSnapshot, Transaction, UserIdentity, UserRecord,
    WalletSummary,
};

pub struct Repository {
    db: PgPool,
}

impl Repository {
    /// Constrói um repository fora do fluxo de extração (ex.: em middlewares,
    /// que recebem o estado mas não passam pela injeção de dependência).
    pub fn from_state(state: &AppState) -> Self {
        Self {
            db: state.db.clone(),
        }
    }
}

impl FromRequestParts<AppState> for Repository {
    type Rejection = Infallible;

    async fn from_request_parts(
        _parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self {
            db: state.db.clone(),
        })
    }
}

impl Repository {
    pub async fn list_assets(&self) -> sqlx::Result<Vec<Asset>> {
        sqlx::query_as!(Asset, "SELECT id, name, unit_value FROM assets ORDER BY id")
            .fetch_all(&self.db)
            .await
    }

    pub async fn create_asset(&self, name: String, unit_value: Decimal) -> Result<Asset, AppError> {
        let name = validated_asset_name(name)?;
        let unit_value = validated_unit_value(unit_value)?;

        Ok(sqlx::query_as!(
            Asset,
            "INSERT INTO assets (name, unit_value) VALUES ($1, $2) RETURNING id, name, unit_value",
            name,
            unit_value
        )
        .fetch_one(&self.db)
        .await?)
    }

    pub async fn update_asset(
        &self,
        asset_id: i64,
        name: Option<String>,
        unit_value: Option<Decimal>,
    ) -> Result<Option<Asset>, AppError> {
        let name = name.map(validated_asset_name).transpose()?;
        let unit_value = unit_value.map(validated_unit_value).transpose()?;

        Ok(sqlx::query_as!(
            Asset,
            "UPDATE assets SET name = COALESCE($2, name), unit_value = COALESCE($3, unit_value) WHERE id = $1 RETURNING id, name, unit_value",
            asset_id,
            name,
            unit_value
        )
        .fetch_optional(&self.db)
        .await?)
    }

    /// Aplica novos preços a ativos identificados pelo nome (normalizado para
    /// minúsculas, sem espaços nas pontas). Faz tudo em **uma** ida ao banco:
    /// `UNNEST` transforma os dois arrays (nomes e valores) numa tabela virtual e
    /// o `UPDATE ... FROM` casa pelo nome. Antes era um `SELECT` seguido de um
    /// `UPDATE` por ativo (N+1) — agora é um statement só.
    pub async fn update_known_asset_prices(
        &self,
        updates: &HashMap<&str, Decimal>,
    ) -> Result<usize, AppError> {
        // `unzip` garante que `names[i]` corresponde a `values[i]` (iterar
        // `keys()` e `values()` em separado não daria essa garantia).
        let (names, values): (Vec<String>, Vec<Decimal>) = updates
            .iter()
            .map(|(name, value)| (name.to_string(), *value))
            .unzip();

        let result = sqlx::query!(
            r#"
            UPDATE assets AS a
            SET unit_value = u.value
            FROM UNNEST($1::text[], $2::numeric[]) AS u(name, value)
            WHERE LOWER(TRIM(a.name)) = u.name
            "#,
            &names,
            &values
        )
        .execute(&self.db)
        .await?;

        Ok(result.rows_affected() as usize)
    }

    /// Garante que um ativo de mercado exista sem duplicar um alias já
    /// cadastrado. É usado pela primeira sincronização de cotações para deixar
    /// uma instalação nova demonstrável sem gravar preços fictícios em uma
    /// migration: o ativo só nasce quando há uma cotação real e positiva.
    pub async fn ensure_market_asset(
        &self,
        canonical_name: &str,
        aliases: &[&str],
        unit_value: Decimal,
    ) -> Result<bool, AppError> {
        let unit_value = validated_unit_value(unit_value)?;
        if unit_value.is_zero() {
            return Err(AppError::QuoteUnavailable);
        }

        let aliases: Vec<String> = aliases.iter().map(|alias| alias.to_string()).collect();
        let result = sqlx::query(
            r#"
            INSERT INTO assets (name, unit_value)
            SELECT $1, $2
            WHERE NOT EXISTS (
                SELECT 1
                FROM assets
                WHERE LOWER(TRIM(name)) = ANY($3::text[])
            )
            ON CONFLICT (name) DO NOTHING
            "#,
        )
        .bind(canonical_name)
        .bind(unit_value)
        .bind(&aliases)
        .execute(&self.db)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    pub async fn add_user(
        &self,
        username: String,
        password_hash: String,
    ) -> sqlx::Result<UserRecord> {
        sqlx::query_as!(
            UserRecord,
            "INSERT INTO users (username, password_hash) VALUES ($1, $2) RETURNING id, username, password_hash, balance, role",
            username,
            password_hash
        )
        .fetch_one(&self.db)
        .await
    }

    pub async fn get_user_by_name(&self, username: &str) -> sqlx::Result<Option<UserRecord>> {
        sqlx::query_as!(
            UserRecord,
            "SELECT id, username, password_hash, balance, role FROM users WHERE username = $1",
            username
        )
        .fetch_optional(&self.db)
        .await
    }

    /// Promove/rebaixa um usuário. O CHECK do banco limita os papéis válidos
    /// ('user'/'admin'); violá-lo vira erro de banco.
    pub async fn set_user_role(&self, user_id: i64, role: &str) -> sqlx::Result<()> {
        sqlx::query!("UPDATE users SET role = $2 WHERE id = $1", user_id, role)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    /// Abre uma sessão de refresh para o usuário: guarda a HASH do token (nunca
    /// o valor) com a expiração dada.
    pub async fn create_session(
        &self,
        user_id: i64,
        token_hash: &[u8],
        expires_at: time::OffsetDateTime,
    ) -> sqlx::Result<()> {
        sqlx::query!(
            "INSERT INTO sessions (user_id, token_hash, expires_at) VALUES ($1, $2, $3)",
            user_id,
            token_hash,
            expires_at
        )
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// Rotaciona uma sessão: revoga atomicamente a sessão antiga (só se ainda
    /// viva e não expirada) e abre uma nova com o novo token. O `UPDATE ...
    /// RETURNING` é a peça-chave — ele "reivindica" a sessão numa operação só,
    /// então um token roubado e o legítimo não conseguem ambos rotacionar: o
    /// segundo a chegar encontra a sessão já revogada e recebe `None`.
    pub async fn rotate_session(
        &self,
        old_hash: &[u8],
        new_hash: &[u8],
        expires_at: time::OffsetDateTime,
    ) -> Result<Option<UserIdentity>, AppError> {
        let mut tx = self.db.begin().await?;

        let Some(claimed) = sqlx::query!(
            r#"
            UPDATE sessions
            SET revoked_at = NOW()
            WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > NOW()
            RETURNING user_id
            "#,
            old_hash
        )
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(None);
        };

        sqlx::query!(
            "INSERT INTO sessions (user_id, token_hash, expires_at) VALUES ($1, $2, $3)",
            claimed.user_id,
            new_hash,
            expires_at
        )
        .execute(&mut *tx)
        .await?;

        let identity = sqlx::query_as!(
            UserIdentity,
            "SELECT id, username, role FROM users WHERE id = $1",
            claimed.user_id
        )
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(Some(identity))
    }

    /// Revoga o refresh token no servidor, inclusive suas cópias. Os JWTs de
    /// acesso já emitidos são stateless e continuam válidos até expirar.
    pub async fn revoke_session(&self, token_hash: &[u8]) -> sqlx::Result<()> {
        sqlx::query!(
            "UPDATE sessions SET revoked_at = NOW() WHERE token_hash = $1 AND revoked_at IS NULL",
            token_hash
        )
        .execute(&self.db)
        .await?;
        Ok(())
    }

    // Os agregados calculados no SQL (aqui, em `list_holdings` e no INSERT de
    // snapshots) saem com ROUND(..., 8): produtos e somas de NUMERIC acumulam
    // escala sem limite, e um resultado com mais de 28 dígitos significativos
    // não é decodificável como `rust_decimal::Decimal` — a leitura falharia
    // mesmo com cada coluna armazenada dentro do invariante de escala.
    pub async fn wallet_summary(&self, user_id: i64) -> sqlx::Result<WalletSummary> {
        sqlx::query_as!(
            WalletSummary,
            r#"
            SELECT
                u.balance,
                ROUND(COALESCE(SUM(h.quantity * a.unit_value), 0), 8) AS "holdings_value!",
                ROUND(u.balance + COALESCE(SUM(h.quantity * a.unit_value), 0), 8) AS "total_value!",
                ROUND(COALESCE(SUM(h.quantity * h.avg_cost), 0), 8) AS "total_invested!",
                ROUND(COALESCE(SUM(h.quantity * (a.unit_value - h.avg_cost)), 0), 8) AS "total_delta!"
            FROM users u
            LEFT JOIN holdings h ON h.user_id = u.id
            LEFT JOIN assets a ON a.id = h.asset_id
            WHERE u.id = $1
            GROUP BY u.id, u.balance
            "#,
            user_id
        )
        .fetch_one(&self.db)
        .await
    }

    pub async fn list_holdings(&self, user_id: i64) -> sqlx::Result<Vec<Holding>> {
        sqlx::query_as!(
            Holding,
            r#"
            SELECT
                a.id,
                a.name,
                a.unit_value,
                h.quantity AS "quantity_owned!",
                h.avg_cost,
                ROUND(h.quantity * a.unit_value, 8) AS "current_value!",
                ROUND(h.quantity * h.avg_cost, 8) AS "invested_value!",
                ROUND(h.quantity * (a.unit_value - h.avg_cost), 8) AS "value_delta!"
            FROM holdings h
            JOIN assets a ON a.id = h.asset_id
            WHERE h.user_id = $1
            ORDER BY a.id
            "#,
            user_id
        )
        .fetch_all(&self.db)
        .await
    }

    /// Página do extrato, da transação mais recente para a mais antiga. O
    /// desempate por `id` torna a ordem determinística mesmo com timestamps
    /// idênticos — sem ele, itens poderiam repetir/sumir entre páginas.
    pub async fn list_transactions(
        &self,
        user_id: i64,
        limit: i64,
        offset: i64,
    ) -> sqlx::Result<Vec<Transaction>> {
        sqlx::query_as!(
            Transaction,
            r#"
            SELECT
                t.id,
                t.kind,
                a.name AS "asset_name?",
                t.quantity,
                t.unit_value,
                t.cash_delta,
                t.created_at
            FROM transactions t
            LEFT JOIN assets a ON a.id = t.asset_id
            WHERE t.user_id = $1
            ORDER BY t.created_at DESC, t.id DESC
            LIMIT $2 OFFSET $3
            "#,
            user_id,
            limit,
            offset
        )
        .fetch_all(&self.db)
        .await
    }

    /// Fotografa o patrimônio de TODOS os usuários (caixa + posições a preço de
    /// mercado) num único INSERT..SELECT. Chamado após cada rodada de cotações
    /// — o momento em que os preços (e portanto o patrimônio) mudam.
    pub async fn record_portfolio_snapshots(&self) -> sqlx::Result<u64> {
        let result = sqlx::query!(
            r#"
            INSERT INTO portfolio_snapshots (user_id, total_value)
            SELECT u.id, ROUND(u.balance + COALESCE(SUM(h.quantity * a.unit_value), 0), 8)
            FROM users u
            LEFT JOIN holdings h ON h.user_id = u.id
            LEFT JOIN assets a ON a.id = h.asset_id
            GROUP BY u.id, u.balance
            "#
        )
        .execute(&self.db)
        .await?;

        Ok(result.rows_affected())
    }

    /// Últimos `limit` pontos da série do patrimônio de um usuário, do mais
    /// antigo para o mais novo (a ordem que o gráfico desenha).
    pub async fn list_portfolio_snapshots(
        &self,
        user_id: i64,
        limit: i64,
    ) -> sqlx::Result<Vec<PortfolioSnapshot>> {
        let mut snapshots = sqlx::query_as!(
            PortfolioSnapshot,
            r#"
            SELECT total_value, captured_at
            FROM portfolio_snapshots
            WHERE user_id = $1
            ORDER BY captured_at DESC, id DESC
            LIMIT $2
            "#,
            user_id,
            limit
        )
        .fetch_all(&self.db)
        .await?;

        snapshots.reverse();
        Ok(snapshots)
    }

    /// Extrato COMPLETO do usuário, sem paginação — alimenta a exportação CSV.
    /// Mesma ordenação estável do extrato paginado.
    pub async fn list_all_transactions(&self, user_id: i64) -> sqlx::Result<Vec<Transaction>> {
        sqlx::query_as!(
            Transaction,
            r#"
            SELECT
                t.id,
                t.kind,
                a.name AS "asset_name?",
                t.quantity,
                t.unit_value,
                t.cash_delta,
                t.created_at
            FROM transactions t
            LEFT JOIN assets a ON a.id = t.asset_id
            WHERE t.user_id = $1
            ORDER BY t.created_at DESC, t.id DESC
            "#,
            user_id
        )
        .fetch_all(&self.db)
        .await
    }

    /// Total de transações do usuário — insumo do "tem próxima página?".
    pub async fn count_transactions(&self, user_id: i64) -> sqlx::Result<i64> {
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM transactions WHERE user_id = $1"#,
            user_id
        )
        .fetch_one(&self.db)
        .await
    }

    pub async fn deposit(&self, user_id: i64, amount: Decimal) -> Result<(), AppError> {
        // Depósito é dinheiro em BRL: centavos (2 casas) são a menor unidade,
        // como o próprio formulário (`step="0.01"`) já sinaliza. Escala maior
        // injetaria dígitos espúrios no saldo.
        if amount <= Decimal::ZERO || amount.normalize().scale() > 2 {
            return Err(AppError::InvalidAmount);
        }

        let mut tx = self.db.begin().await?;

        sqlx::query!(
            "UPDATE users SET balance = balance + $2 WHERE id = $1",
            user_id,
            amount
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query!(
            "INSERT INTO transactions (user_id, kind, cash_delta) VALUES ($1, 'deposit', $2)",
            user_id,
            amount
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn buy_asset(
        &self,
        user_id: i64,
        asset_id: i64,
        quantity: Decimal,
    ) -> Result<(), AppError> {
        if quantity <= Decimal::ZERO || quantity.normalize().scale() > MONEY_SCALE {
            return Err(AppError::InvalidAmount);
        }

        let mut tx = self.db.begin().await?;
        let asset = asset_for_update(&mut tx, asset_id).await?;
        if asset.unit_value <= Decimal::ZERO {
            return Err(AppError::QuoteUnavailable);
        }
        // Arredonda o movimento de caixa para a escala canônica: preço e
        // quantidade já são ≤ 8 casas, mas o produto pode chegar a 16 — e o
        // saldo/extrato devem manter o invariante de `MONEY_SCALE`.
        let cost = (asset.unit_value * quantity).round_dp(MONEY_SCALE);
        if cost <= Decimal::ZERO {
            return Err(AppError::TradeTooSmall);
        }

        let user = sqlx::query!(
            "SELECT balance FROM users WHERE id = $1 FOR UPDATE",
            user_id
        )
        .fetch_one(&mut *tx)
        .await?;

        if user.balance < cost {
            return Err(AppError::InsufficientBalance);
        }

        sqlx::query!(
            "UPDATE users SET balance = balance - $2 WHERE id = $1",
            user_id,
            cost
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query!(
            r#"
            INSERT INTO holdings (user_id, asset_id, quantity, avg_cost)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (user_id, asset_id) DO UPDATE SET
                avg_cost = ROUND(
                    ((holdings.quantity * holdings.avg_cost) + (EXCLUDED.quantity * EXCLUDED.avg_cost))
                        / (holdings.quantity + EXCLUDED.quantity),
                    8),
                quantity = holdings.quantity + EXCLUDED.quantity
            "#,
            user_id,
            asset_id,
            quantity,
            asset.unit_value
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query!(
            "INSERT INTO transactions (user_id, kind, asset_id, quantity, unit_value, cash_delta) VALUES ($1, 'buy', $2, $3, $4, $5)",
            user_id,
            asset_id,
            quantity,
            asset.unit_value,
            -cost
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn sell_asset(
        &self,
        user_id: i64,
        asset_id: i64,
        quantity: Decimal,
    ) -> Result<(), AppError> {
        if quantity <= Decimal::ZERO || quantity.normalize().scale() > MONEY_SCALE {
            return Err(AppError::InvalidAmount);
        }

        let mut tx = self.db.begin().await?;
        let asset = asset_for_update(&mut tx, asset_id).await?;
        if asset.unit_value <= Decimal::ZERO {
            return Err(AppError::QuoteUnavailable);
        }

        let holding = sqlx::query!(
            "SELECT quantity FROM holdings WHERE user_id = $1 AND asset_id = $2 FOR UPDATE",
            user_id,
            asset_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::InsufficientHoldings)?;

        if holding.quantity < quantity {
            return Err(AppError::InsufficientHoldings);
        }

        let proceeds = (asset.unit_value * quantity).round_dp(MONEY_SCALE);
        if proceeds <= Decimal::ZERO {
            return Err(AppError::TradeTooSmall);
        }

        sqlx::query!(
            "UPDATE users SET balance = balance + $2 WHERE id = $1",
            user_id,
            proceeds
        )
        .execute(&mut *tx)
        .await?;

        if holding.quantity == quantity {
            sqlx::query!(
                "DELETE FROM holdings WHERE user_id = $1 AND asset_id = $2",
                user_id,
                asset_id
            )
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query!(
                "UPDATE holdings SET quantity = quantity - $3 WHERE user_id = $1 AND asset_id = $2",
                user_id,
                asset_id,
                quantity
            )
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query!(
            "INSERT INTO transactions (user_id, kind, asset_id, quantity, unit_value, cash_delta) VALUES ($1, 'sell', $2, $3, $4, $5)",
            user_id,
            asset_id,
            quantity,
            asset.unit_value,
            proceeds
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }
}

/// Normaliza e valida o nome de um ativo: sem espaços nas pontas e nunca vazio.
/// Validar aqui (no repository, por onde toda escrita passa) garante que nenhum
/// caminho — API do admin, seed, teste — grava um nome inválido.
fn validated_asset_name(name: String) -> Result<String, AppError> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::InvalidAssetName);
    }
    Ok(name)
}

/// Um preço nunca pode ser negativo (zero é permitido no catálogo para
/// representar um ativo ainda sem cotação). Também arredondamos toda escrita
/// administrativa para a escala canônica: sem isso a API poderia reintroduzir
/// valores que os agregados financeiros não conseguem decodificar.
fn validated_unit_value(unit_value: Decimal) -> Result<Decimal, AppError> {
    if unit_value < Decimal::ZERO {
        return Err(AppError::NegativeUnitValue);
    }
    Ok(unit_value.round_dp(MONEY_SCALE))
}

async fn asset_for_update(
    tx: &mut SqlxTransaction<'_, Postgres>,
    asset_id: i64,
) -> Result<Asset, AppError> {
    sqlx::query_as!(
        Asset,
        "SELECT id, name, unit_value FROM assets WHERE id = $1 FOR UPDATE",
        asset_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(AppError::AssetDoesNotExist)
}

#[cfg(test)]
impl From<PgPool> for Repository {
    fn from(db: PgPool) -> Self {
        Self { db }
    }
}

// Testes do núcleo financeiro: depósito, compra, venda, custo médio e as guardas
// de saldo/posição. É a parte do sistema que mexe em dinheiro — a que mais merece
// rede de proteção. Cada `#[sqlx::test]` roda num banco efêmero próprio (migrações
// aplicadas automaticamente), então os testes são isolados e podem rodar em
// paralelo.
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    async fn new_user(repo: &Repository, name: &str) -> i64 {
        repo.add_user(name.to_string(), "stub-hash".to_string())
            .await
            .expect("create user")
            .id
    }

    async fn new_asset(repo: &Repository, name: &str, price: Decimal) -> i64 {
        repo.create_asset(name.to_string(), price)
            .await
            .expect("create asset")
            .id
    }

    #[sqlx::test]
    async fn deposit_credits_balance_and_logs_transaction(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;

        repo.deposit(uid, dec!(100)).await.expect("deposit");

        let summary = repo.wallet_summary(uid).await.expect("summary");
        assert_eq!(summary.balance, dec!(100));

        let txs = repo
            .list_transactions(uid, 25, 0)
            .await
            .expect("transactions");
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].kind, "deposit");
        assert_eq!(txs[0].cash_delta, dec!(100));
    }

    #[sqlx::test]
    async fn transactions_paginate_newest_first_without_gaps(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;

        // Três depósitos com valores distintos para rastrear a ordem.
        repo.deposit(uid, dec!(1)).await.unwrap();
        repo.deposit(uid, dec!(2)).await.unwrap();
        repo.deposit(uid, dec!(3)).await.unwrap();

        assert_eq!(repo.count_transactions(uid).await.unwrap(), 3);

        // Página 1 (2 itens): as duas mais recentes, na ordem 3 -> 2.
        let first = repo.list_transactions(uid, 2, 0).await.unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].cash_delta, dec!(3));
        assert_eq!(first[1].cash_delta, dec!(2));

        // Página 2: só a mais antiga, sem repetir nem pular nenhuma.
        let second = repo.list_transactions(uid, 2, 2).await.unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].cash_delta, dec!(1));
    }

    #[sqlx::test]
    async fn deposit_rejects_non_positive_amounts(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;

        assert!(matches!(
            repo.deposit(uid, dec!(0)).await,
            Err(AppError::InvalidAmount)
        ));
        assert!(matches!(
            repo.deposit(uid, dec!(-5)).await,
            Err(AppError::InvalidAmount)
        ));

        assert_eq!(repo.wallet_summary(uid).await.unwrap().balance, dec!(0));
    }

    #[sqlx::test]
    async fn deposits_and_trades_reject_excessive_scale(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;

        // Depósito é BRL: nada além de centavos.
        assert!(matches!(
            repo.deposit(uid, dec!(1.001)).await,
            Err(AppError::InvalidAmount)
        ));
        // Zeros à direita não contam como escala (1.010 == 1.01).
        repo.deposit(uid, dec!(1.010))
            .await
            .expect("trailing zero ok");

        // Quantidade além da escala canônica (8) é rejeitada antes de tocar o
        // banco — por isso nem precisa de ativo existente.
        assert!(matches!(
            repo.buy_asset(uid, 1, dec!(0.000000001)).await,
            Err(AppError::InvalidAmount)
        ));
        assert!(matches!(
            repo.sell_asset(uid, 1, dec!(0.000000001)).await,
            Err(AppError::InvalidAmount)
        ));
    }

    /// Regressão do incidente de 2026-07-22: preços de cotação gravados como
    /// `1/taxa` SEM arredondar deixaram NUMERIC de escala 28 no banco;
    /// produtos e somas desses valores passam dos 28 dígitos significativos do
    /// `Decimal` e a LEITURA quebrava (`value not representable`) — /assets
    /// respondia 500 para qualquer conta com posições. O estado patológico é
    /// plantado por fora dos caminhos de escrita atuais (que já arredondam) e
    /// as leituras têm de continuar decodificando, graças ao ROUND nas queries.
    #[sqlx::test]
    async fn legacy_high_scale_money_still_renders_the_wallet(db: PgPool) {
        let repo = Repository::from(db.clone());
        let uid = new_user(&repo, "esousa").await;
        let aid = new_asset(&repo, "dolar", dec!(5)).await;

        sqlx::query("UPDATE assets SET unit_value = 5.0661127716702973808196970465 WHERE id = $1")
            .bind(aid)
            .execute(&db)
            .await
            .unwrap();
        // Espelha o que os fluxos antigos deixaram: valores que INDIVIDUALMENTE
        // ainda cabem num Decimal (foram gravados por ele), mas cuja escala
        // explode qualquer produto/soma no SQL.
        sqlx::query("UPDATE users SET balance = 949.1847075186258425734544376 WHERE id = $1")
            .bind(uid)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO holdings (user_id, asset_id, quantity, avg_cost) \
             VALUES ($1, $2, 10, 5.0815292481374157426545562388)",
        )
        .bind(uid)
        .bind(aid)
        .execute(&db)
        .await
        .unwrap();

        // Sem o ROUND, cada uma destas leituras falhava com ColumnDecode.
        let summary = repo.wallet_summary(uid).await.expect("summary decodes");
        assert_eq!(summary.balance, dec!(949.1847075186258425734544376));
        assert_eq!(summary.holdings_value, dec!(50.66112772));
        assert_eq!(summary.total_value, dec!(999.84583524));

        let holdings = repo.list_holdings(uid).await.expect("holdings decode");
        assert_eq!(holdings[0].invested_value, dec!(50.81529248));

        // O snapshot novo já nasce na escala canônica e o gráfico decodifica.
        repo.record_portfolio_snapshots().await.expect("snapshot");
        let snapshots = repo
            .list_portfolio_snapshots(uid, 10)
            .await
            .expect("snapshots decode");
        assert_eq!(snapshots.last().unwrap().total_value, dec!(999.84583524));

        // Comprar mais ao preço legado mantém o custo médio dentro do
        // invariante (ROUND no upsert), em vez de propagar a escala doente.
        repo.deposit(uid, dec!(100)).await.unwrap();
        repo.buy_asset(uid, aid, dec!(1)).await.expect("buy");
        let holdings = repo.list_holdings(uid).await.unwrap();
        assert!(
            holdings[0].avg_cost.scale() <= MONEY_SCALE,
            "avg_cost escala {} > {}",
            holdings[0].avg_cost.scale(),
            MONEY_SCALE
        );
    }

    #[sqlx::test]
    async fn buy_debits_balance_and_opens_holding(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;
        let aid = new_asset(&repo, "bitcoin", dec!(10)).await;
        repo.deposit(uid, dec!(100)).await.unwrap();

        repo.buy_asset(uid, aid, dec!(3)).await.expect("buy");

        let summary = repo.wallet_summary(uid).await.unwrap();
        assert_eq!(summary.balance, dec!(70));
        assert_eq!(summary.holdings_value, dec!(30));

        let holdings = repo.list_holdings(uid).await.unwrap();
        assert_eq!(holdings.len(), 1);
        assert_eq!(holdings[0].quantity_owned, dec!(3));
        assert_eq!(holdings[0].avg_cost, dec!(10));
    }

    #[sqlx::test]
    async fn buy_rejects_when_balance_is_insufficient(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;
        let aid = new_asset(&repo, "bitcoin", dec!(10)).await;
        repo.deposit(uid, dec!(10)).await.unwrap();

        // Custo 20 (2 @ 10) maior que o saldo 10.
        let result = repo.buy_asset(uid, aid, dec!(2)).await;
        assert!(matches!(result, Err(AppError::InsufficientBalance)));

        // A transação inteira foi revertida: saldo intacto, nenhuma posição.
        assert_eq!(repo.wallet_summary(uid).await.unwrap().balance, dec!(10));
        assert!(repo.list_holdings(uid).await.unwrap().is_empty());
    }

    #[sqlx::test]
    async fn buy_rejects_unknown_asset(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;
        repo.deposit(uid, dec!(100)).await.unwrap();

        let result = repo.buy_asset(uid, 999, dec!(1)).await;
        assert!(matches!(result, Err(AppError::AssetDoesNotExist)));
    }

    #[sqlx::test]
    async fn buying_more_averages_the_cost_basis(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;
        let aid = new_asset(&repo, "bitcoin", dec!(10)).await;
        repo.deposit(uid, dec!(1000)).await.unwrap();

        repo.buy_asset(uid, aid, dec!(2)).await.unwrap(); // 2 @ 10
        repo.update_asset(aid, None, Some(dec!(20))).await.unwrap();
        repo.buy_asset(uid, aid, dec!(2)).await.unwrap(); // 2 @ 20

        let holdings = repo.list_holdings(uid).await.unwrap();
        assert_eq!(holdings[0].quantity_owned, dec!(4));
        // (2*10 + 2*20) / 4 = 15
        assert_eq!(holdings[0].avg_cost, dec!(15));
    }

    #[sqlx::test]
    async fn selling_everything_closes_the_position(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;
        let aid = new_asset(&repo, "bitcoin", dec!(10)).await;
        repo.deposit(uid, dec!(100)).await.unwrap();
        repo.buy_asset(uid, aid, dec!(3)).await.unwrap(); // saldo 70, 3 un

        repo.sell_asset(uid, aid, dec!(3)).await.expect("sell");

        assert!(repo.list_holdings(uid).await.unwrap().is_empty());
        // 70 + 3 * 10 de volta.
        assert_eq!(repo.wallet_summary(uid).await.unwrap().balance, dec!(100));
    }

    #[sqlx::test]
    async fn partial_sell_keeps_remaining_units(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;
        let aid = new_asset(&repo, "bitcoin", dec!(10)).await;
        repo.deposit(uid, dec!(100)).await.unwrap();
        repo.buy_asset(uid, aid, dec!(4)).await.unwrap(); // saldo 60

        repo.sell_asset(uid, aid, dec!(1)).await.unwrap(); // +10 -> 70

        let holdings = repo.list_holdings(uid).await.unwrap();
        assert_eq!(holdings[0].quantity_owned, dec!(3));
        // Vender não altera o custo médio das unidades restantes.
        assert_eq!(holdings[0].avg_cost, dec!(10));
        assert_eq!(repo.wallet_summary(uid).await.unwrap().balance, dec!(70));
    }

    #[sqlx::test]
    async fn sell_rejects_more_than_owned(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;
        let aid = new_asset(&repo, "bitcoin", dec!(10)).await;
        repo.deposit(uid, dec!(100)).await.unwrap();
        repo.buy_asset(uid, aid, dec!(1)).await.unwrap();

        // Mais do que a posição comporta.
        assert!(matches!(
            repo.sell_asset(uid, aid, dec!(2)).await,
            Err(AppError::InsufficientHoldings)
        ));

        // Vender um ativo sem nenhuma posição também é rejeitado.
        let other = new_asset(&repo, "ethereum", dec!(5)).await;
        assert!(matches!(
            repo.sell_asset(uid, other, dec!(1)).await,
            Err(AppError::InsufficientHoldings)
        ));
    }

    #[sqlx::test]
    async fn users_default_to_the_user_role_and_can_be_promoted(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;

        let record = repo.get_user_by_name("alice").await.unwrap().unwrap();
        assert_eq!(record.role, "user");

        repo.set_user_role(uid, "admin").await.expect("promote");
        let record = repo.get_user_by_name("alice").await.unwrap().unwrap();
        assert_eq!(record.role, "admin");

        // Papel fora do CHECK ('user'/'admin') é rejeitado pelo banco.
        assert!(repo.set_user_role(uid, "root").await.is_err());
    }

    fn future_expiry() -> time::OffsetDateTime {
        time::OffsetDateTime::now_utc() + time::Duration::days(14)
    }

    #[sqlx::test]
    async fn session_rotation_returns_the_user_and_burns_the_old_token(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;

        repo.create_session(uid, b"old-hash", future_expiry())
            .await
            .expect("create session");

        // Primeira rotação: reivindica a sessão antiga e devolve a identidade.
        let identity = repo
            .rotate_session(b"old-hash", b"new-hash", future_expiry())
            .await
            .expect("rotate")
            .expect("session is live");
        assert_eq!(identity.id, uid);
        assert_eq!(identity.username, "alice");

        // Replay do token antigo: já foi queimado na rotação, não funciona mais.
        assert!(
            repo.rotate_session(b"old-hash", b"another", future_expiry())
                .await
                .expect("rotate")
                .is_none()
        );

        // O token novo emitido pela rotação está válido.
        assert!(
            repo.rotate_session(b"new-hash", b"newer", future_expiry())
                .await
                .expect("rotate")
                .is_some()
        );
    }

    #[sqlx::test]
    async fn revoked_session_cannot_rotate(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;

        repo.create_session(uid, b"hash", future_expiry())
            .await
            .unwrap();
        repo.revoke_session(b"hash").await.expect("revoke");

        assert!(
            repo.rotate_session(b"hash", b"new", future_expiry())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[sqlx::test]
    async fn expired_session_cannot_rotate(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;

        // Sessão que já nasceu expirada (expiry no passado).
        let past = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
        repo.create_session(uid, b"hash", past).await.unwrap();

        assert!(
            repo.rotate_session(b"hash", b"new", future_expiry())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[sqlx::test]
    async fn unknown_token_cannot_rotate(db: PgPool) {
        let repo = Repository::from(db);

        assert!(
            repo.rotate_session(b"never-seen", b"new", future_expiry())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[sqlx::test]
    async fn portfolio_snapshots_capture_cash_plus_holdings(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;
        let aid = new_asset(&repo, "bitcoin", dec!(10)).await;
        repo.deposit(uid, dec!(100)).await.unwrap();
        repo.buy_asset(uid, aid, dec!(3)).await.unwrap(); // caixa 70 + 3×10 = 100

        let recorded = repo.record_portfolio_snapshots().await.expect("snapshot");
        assert_eq!(recorded, 1); // um usuário, uma linha

        // Preço dobra: o próximo snapshot captura o novo patrimônio.
        repo.update_asset(aid, None, Some(dec!(20))).await.unwrap();
        repo.record_portfolio_snapshots().await.unwrap();

        let series = repo
            .list_portfolio_snapshots(uid, 60)
            .await
            .expect("series");
        assert_eq!(series.len(), 2);
        // Ordem do gráfico: do mais antigo para o mais novo.
        assert_eq!(series[0].total_value, dec!(100)); // 70 + 3×10
        assert_eq!(series[1].total_value, dec!(130)); // 70 + 3×20

        // O limite corta pelos MAIS RECENTES.
        let last_only = repo.list_portfolio_snapshots(uid, 1).await.unwrap();
        assert_eq!(last_only.len(), 1);
        assert_eq!(last_only[0].total_value, dec!(130));
    }

    #[sqlx::test]
    async fn asset_creation_rejects_invalid_input(db: PgPool) {
        let repo = Repository::from(db);

        // Nome vazio (ou só espaços) é rejeitado antes de tocar o banco.
        assert!(matches!(
            repo.create_asset("   ".to_string(), dec!(1)).await,
            Err(AppError::InvalidAssetName)
        ));
        // Preço negativo inverteria a matemática da carteira.
        assert!(matches!(
            repo.create_asset("bitcoin".to_string(), dec!(-1)).await,
            Err(AppError::NegativeUnitValue)
        ));

        assert!(repo.list_assets().await.unwrap().is_empty());
    }

    #[sqlx::test]
    async fn asset_update_rejects_invalid_input(db: PgPool) {
        let repo = Repository::from(db);
        let aid = new_asset(&repo, "bitcoin", dec!(10)).await;

        assert!(matches!(
            repo.update_asset(aid, Some("  ".to_string()), None).await,
            Err(AppError::InvalidAssetName)
        ));
        assert!(matches!(
            repo.update_asset(aid, None, Some(dec!(-3))).await,
            Err(AppError::NegativeUnitValue)
        ));

        // O ativo permanece intocado após as tentativas inválidas.
        let assets = repo.list_assets().await.unwrap();
        assert_eq!(assets[0].name, "bitcoin");
        assert_eq!(assets[0].unit_value, dec!(10));
    }

    #[sqlx::test]
    async fn admin_prices_are_capped_at_the_canonical_scale(db: PgPool) {
        let repo = Repository::from(db);

        let asset = repo
            .create_asset("bitcoin".to_string(), dec!(1.123456789))
            .await
            .expect("create asset");
        assert_eq!(asset.unit_value, dec!(1.12345679));

        let asset = repo
            .update_asset(asset.id, None, Some(dec!(9.876543219)))
            .await
            .expect("update asset")
            .expect("known asset");
        assert_eq!(asset.unit_value, dec!(9.87654322));
    }

    #[sqlx::test]
    async fn asset_name_is_trimmed_on_write(db: PgPool) {
        let repo = Repository::from(db);

        let asset = repo
            .create_asset("  bitcoin  ".to_string(), dec!(10))
            .await
            .expect("create asset");

        assert_eq!(asset.name, "bitcoin");
    }

    #[sqlx::test]
    async fn unquoted_assets_cannot_be_bought_or_sold_for_free(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;
        let aid = new_asset(&repo, "bitcoin", dec!(0)).await;
        repo.deposit(uid, dec!(100)).await.unwrap();

        assert!(matches!(
            repo.buy_asset(uid, aid, dec!(1)).await,
            Err(AppError::QuoteUnavailable)
        ));

        // Abre uma posição com preço válido e depois simula a perda da
        // cotação. Vender por zero também precisa ser bloqueado.
        repo.update_asset(aid, None, Some(dec!(10))).await.unwrap();
        repo.buy_asset(uid, aid, dec!(1)).await.unwrap();
        repo.update_asset(aid, None, Some(dec!(0))).await.unwrap();

        assert!(matches!(
            repo.sell_asset(uid, aid, dec!(1)).await,
            Err(AppError::QuoteUnavailable)
        ));
        assert_eq!(
            repo.list_holdings(uid).await.unwrap()[0].quantity_owned,
            dec!(1)
        );
    }

    #[sqlx::test]
    async fn trades_that_round_to_zero_do_not_move_cash_or_holdings(db: PgPool) {
        let repo = Repository::from(db);
        let uid = new_user(&repo, "alice").await;
        let aid = new_asset(&repo, "micro", dec!(0.00000001)).await;
        repo.deposit(uid, dec!(1)).await.unwrap();

        assert!(matches!(
            repo.buy_asset(uid, aid, dec!(0.00000001)).await,
            Err(AppError::TradeTooSmall)
        ));
        assert!(repo.list_holdings(uid).await.unwrap().is_empty());
        assert_eq!(repo.wallet_summary(uid).await.unwrap().balance, dec!(1));

        // Cria uma posição válida e reduz a cotação para que a venda tentada
        // também tenha total abaixo da menor unidade monetária suportada.
        repo.update_asset(aid, None, Some(dec!(1))).await.unwrap();
        repo.buy_asset(uid, aid, dec!(1)).await.unwrap();
        repo.update_asset(aid, None, Some(dec!(0.00000001)))
            .await
            .unwrap();

        assert!(matches!(
            repo.sell_asset(uid, aid, dec!(0.00000001)).await,
            Err(AppError::TradeTooSmall)
        ));
        assert_eq!(
            repo.list_holdings(uid).await.unwrap()[0].quantity_owned,
            dec!(1)
        );
        assert_eq!(repo.wallet_summary(uid).await.unwrap().balance, dec!(0));
    }

    #[sqlx::test]
    async fn market_bootstrap_inserts_once_and_respects_existing_aliases(db: PgPool) {
        let repo = Repository::from(db);
        let aliases = ["bitcoin", "btc"];

        assert!(
            repo.ensure_market_asset("bitcoin", &aliases, dec!(500000))
                .await
                .unwrap()
        );
        assert!(
            !repo
                .ensure_market_asset("bitcoin", &aliases, dec!(510000))
                .await
                .unwrap()
        );

        // Um catálogo que já usa o alias "BTC" não deve ganhar uma segunda
        // linha chamada "bitcoin".
        let eth_aliases = ["ethereum", "eth"];
        new_asset(&repo, "ETH", dec!(10)).await;
        assert!(
            !repo
                .ensure_market_asset("ethereum", &eth_aliases, dec!(20000))
                .await
                .unwrap()
        );

        let assets = repo.list_assets().await.unwrap();
        assert_eq!(assets.len(), 2);
        assert_eq!(assets[0].unit_value, dec!(500000));
    }

    #[sqlx::test]
    async fn update_known_asset_prices_matches_by_normalized_name(db: PgPool) {
        let repo = Repository::from(db);
        // "Bitcoin" com maiúscula deve casar com a chave "bitcoin" (normalização).
        let btc = new_asset(&repo, "Bitcoin", dec!(1)).await;
        let real = new_asset(&repo, "real", dec!(1)).await;
        let eth = new_asset(&repo, "ethereum", dec!(7)).await;

        let mut updates = HashMap::new();
        updates.insert("bitcoin", dec!(500000));
        updates.insert("real", dec!(1));
        updates.insert("dolar", dec!(5)); // sem ativo correspondente: ignorado

        let count = repo
            .update_known_asset_prices(&updates)
            .await
            .expect("update prices");
        assert_eq!(count, 2);

        let assets = repo.list_assets().await.unwrap();
        let price_of = |id: i64| assets.iter().find(|a| a.id == id).unwrap().unit_value;
        assert_eq!(price_of(btc), dec!(500000));
        assert_eq!(price_of(real), dec!(1));
        assert_eq!(price_of(eth), dec!(7)); // intocado
    }
}
