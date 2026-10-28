use std::convert::Infallible;

use askama::Template;
use axum::Router;
use axum::extract::{Form, FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use rust_decimal::Decimal;
use serde::Deserialize;
use tracing::instrument;

use crate::app::AppState;
use crate::auth::csrf::{ensure_csrf_token, verify_csrf};
use crate::auth::session::{
    REFRESH_COOKIE, RefreshToken, access_cookie, hash_token, refresh_cookie, session_expiry,
};
use crate::auth::user::{TOKEN_COOKIE, UnauthenticatedUser, User};
use crate::config::Config;
use crate::error::AppError;
use crate::i18n::{Locale, Strings, lang_cookie};
use crate::market::{Coin, PriceChart, Range};
use crate::models::{Asset, Holding, Transaction, WalletSummary};
use crate::repository::Repository;
use crate::routes::flash::{Flash, business_flash, set_flash, take_flash};
use crate::services::portfolio::{EquityChart, PortfolioService, WalletView};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/static/app.css", get(app_css))
        .route("/static/htmx.js", get(htmx_js))
        .route("/static/money-input.js", get(money_input_js))
        .route("/login", get(login_page).post(login))
        .route("/register", get(register_page).post(register))
        .route("/logout", get(logout))
        .route("/assets", get(assets_page))
        .route("/market", get(market_page))
        .route("/transactions.csv", get(transactions_csv))
        .route("/deposit", get(deposit_page).post(deposit))
        .route("/buy", get(buy_page).post(buy_asset))
        .route("/sell", get(sell_page).post(sell_asset))
        .route("/quotes/sync", post(sync_quotes))
        .route("/lang/{code}", get(set_language))
}

#[derive(Deserialize)]
struct LangQuery {
    next: Option<String>,
}

/// Troca o idioma da interface: grava o cookie `lang` e volta para a página de
/// origem (`?next=`). Código desconhecido não grava nada — só redireciona.
#[instrument(skip_all)]
async fn set_language(
    State(state): State<AppState>,
    Path(code): Path<String>,
    jar: CookieJar,
    Query(query): Query<LangQuery>,
) -> (CookieJar, Redirect) {
    let jar = match Locale::from_tag(&code) {
        Some(locale) => jar.add(lang_cookie(locale, state.config.cookie_secure)),
        None => jar,
    };

    (jar, Redirect::to(sanitized_next(query.next.as_deref())))
}

/// Valida o destino do retorno pós-troca de idioma: só caminhos locais
/// absolutos ("/algo"). Nada de "//host" (URL relativa a protocolo) nem URLs
/// completas — um `next` vindo da query string nunca vira open redirect.
fn sanitized_next(next: Option<&str>) -> &str {
    match next {
        Some(path) if path.starts_with('/') && !path.starts_with("//") => path,
        _ => "/",
    }
}

/// CSS da interface, servido do próprio binário (`include_str!` embute o
/// arquivo em tempo de compilação, como o askama faz com os templates): nada de
/// CDN de terceiros — sem dependência externa em runtime, sem telemetria
/// alheia, e a CSP trava `style-src` em `'self'`.
///
/// É CSS **pré-compilado** (ver `styles/app.css`), não o Play CDN do Tailwind:
/// aquele era um compilador rodando no navegador, que injetava `<style>` em
/// runtime e por isso exigia `'unsafe-inline'` na política.
#[instrument(skip_all)]
async fn app_css(headers: HeaderMap) -> Response {
    const BODY: &str = include_str!("../../static/app.css");
    static TAG: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    static_asset(
        &headers,
        "text/css; charset=utf-8",
        BODY,
        TAG.get_or_init(|| content_tag(BODY)),
    )
}

/// htmx servido do binário, pelo mesmo motivo do Tailwind acima: zero CDN e a
/// CSP continua com `script-src 'self'`. É ele que transforma os links e
/// formulários da carteira (atributos `hx-*` nos templates) em trocas parciais
/// de HTML — o servidor segue renderizando tudo (SSR); só muda o quanto de
/// página viaja e é re-desenhado por operação.
#[instrument(skip_all)]
async fn htmx_js(headers: HeaderMap) -> Response {
    const BODY: &str = include_str!("../../static/htmx.js");
    static TAG: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    static_asset(
        &headers,
        "application/javascript",
        BODY,
        TAG.get_or_init(|| content_tag(BODY)),
    )
}

/// Máscara monetária do campo de depósito, pelo mesmo motivo dos dois acima:
/// zero CDN, mesma CSP. Progressive enhancement puro — o campo já funciona
/// sem este arquivo (ver `static/money-input.js`).
#[instrument(skip_all)]
async fn money_input_js(headers: HeaderMap) -> Response {
    const BODY: &str = include_str!("../../static/money-input.js");
    static TAG: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    static_asset(
        &headers,
        "application/javascript",
        BODY,
        TAG.get_or_init(|| content_tag(BODY)),
    )
}

/// Política de cache dos assets do binário.
///
/// A URL deles é FIXA e o conteúdo muda a cada build — a combinação que torna
/// um `max-age` longo uma armadilha, não uma otimização: depois de recompilar,
/// o navegador continua servindo o CSS da versão anterior até o prazo vencer, e
/// a tela abre com o estilo de outro binário (foi exatamente o que aconteceu
/// quando a tela de mercado entrou: layout de duas colunas no HTML, CSS antigo
/// no cache, painel empilhado na tela).
///
/// `no-cache` não é "não guarde" — é "guarde e pergunte antes de usar". Com o
/// `ETag` do conteúdo, a pergunta cabe num 304 vazio: o arquivo só desce de
/// novo quando muda de verdade, e quando muda desce na primeira visita.
const ASSET_CACHE: &str = "public, no-cache";

/// Responde o asset, ou um 304 quando o navegador já tem esta versão.
fn static_asset(
    request: &HeaderMap,
    content_type: &'static str,
    body: &'static str,
    tag: &str,
) -> Response {
    let headers = [
        (header::CONTENT_TYPE, content_type),
        (header::CACHE_CONTROL, ASSET_CACHE),
        (header::ETAG, tag),
    ];

    if has_tag(request, tag) {
        return (StatusCode::NOT_MODIFIED, headers).into_response();
    }

    (headers, body).into_response()
}

/// O `If-None-Match` pode trazer uma LISTA de etiquetas (e cada uma pode vir
/// marcada como fraca, `W/"…"`), então comparar a string inteira erraria.
fn has_tag(request: &HeaderMap, tag: &str) -> bool {
    request
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .map(|candidate| candidate.trim().trim_start_matches("W/"))
                .any(|candidate| candidate == tag || candidate == "*")
        })
}

/// Impressão digital do conteúdo, no formato de `ETag` (entre aspas). Metade do
/// SHA-256 basta: aqui a etiqueta só precisa mudar quando o arquivo muda, não
/// resistir a alguém tentando forjar uma colisão.
fn content_tag(body: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(body.as_bytes());
    let mut tag = String::with_capacity(34);

    tag.push('"');
    for byte in &digest[..16] {
        use std::fmt::Write;
        let _ = write!(tag, "{byte:02x}");
    }
    tag.push('"');

    tag
}

/// `true` quando a resposta deve ser o fragmento parcial da carteira, não a
/// página inteira: o htmx marca suas requisições com `HX-Request: true`. A
/// exceção é a restauração de histórico (voltar/avançar com o cache local
/// expirado), que vem com `HX-History-Restore-Request` e espera a página
/// COMPLETA para reconstruir o estado do zero.
fn is_htmx_request(headers: &HeaderMap) -> bool {
    headers
        .get("hx-request")
        .is_some_and(|value| value == "true")
}

fn is_partial_request(headers: &HeaderMap) -> bool {
    is_htmx_request(headers) && !headers.contains_key("hx-history-restore-request")
}

/// Extrator do sinal acima. Sem JavaScript (ou sem o header), tudo cai no
/// caminho clássico de página cheia — htmx aqui é *progressive enhancement*.
struct HxRequest(bool);

impl<S: Send + Sync> FromRequestParts<S> for HxRequest {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Infallible> {
        Ok(Self(is_partial_request(&parts.headers)))
    }
}

/// `User` para telas HTML: sem sessão válida, manda para `/login` em vez de
/// devolver JSON.
///
/// O extrator `User` rejeita com `AppError`, que vira `{"error": "..."}` — a
/// resposta certa para a API, e a errada para quem abriu um link no navegador
/// com o token expirado. Em navegação clássica devolvemos o redirect HTTP; numa
/// requisição htmx usamos `HX-Redirect`, para que a tela de login substitua a
/// página inteira em vez de ser encaixada dentro do fragmento da carteira.
struct SessionUser(User);

impl FromRequestParts<AppState> for SessionUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        User::from_request_parts(parts, state)
            .await
            .map(Self)
            .map_err(|_| unauthenticated_page_response(&parts.headers))
    }
}

fn unauthenticated_page_response(headers: &HeaderMap) -> Response {
    if is_htmx_request(headers) {
        let mut response = StatusCode::UNAUTHORIZED.into_response();
        response
            .headers_mut()
            .insert("hx-redirect", HeaderValue::from_static("/login"));
        response
    } else {
        Redirect::to("/login").into_response()
    }
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    is_register: bool,
    csrf_token: String,
    flash: Option<Flash>,
    t: &'static Strings,
}

impl LoginPage {
    /// Caminho desta tela (login ou cadastro): destino do formulário e retorno
    /// (`?next=`) da troca de idioma.
    fn form_path(&self) -> &'static str {
        if self.is_register {
            "/register"
        } else {
            "/login"
        }
    }
}

/// Toda página com formulário garante um token CSRF na jar e o embute num campo
/// oculto; os POSTs correspondentes conferem os dois (ver `auth::csrf`). O
/// flash (se houver) é consumido aqui e vira o banner de feedback.
#[instrument(skip_all)]
async fn login_page(
    State(state): State<AppState>,
    jar: CookieJar,
    locale: Locale,
) -> Result<(CookieJar, Html<String>), AppError> {
    let (jar, flash) = take_flash(jar);
    let (jar, csrf_token) = ensure_csrf_token(jar, state.config.cookie_secure);
    let page = LoginPage {
        is_register: false,
        csrf_token,
        flash,
        t: locale.strings(),
    };
    Ok((jar, Html(page.render()?)))
}

#[instrument(skip_all)]
async fn register_page(
    State(state): State<AppState>,
    jar: CookieJar,
    locale: Locale,
) -> Result<(CookieJar, Html<String>), AppError> {
    let (jar, flash) = take_flash(jar);
    let (jar, csrf_token) = ensure_csrf_token(jar, state.config.cookie_secure);
    let page = LoginPage {
        is_register: true,
        csrf_token,
        flash,
        t: locale.strings(),
    };
    Ok((jar, Html(page.render()?)))
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
    csrf_token: String,
}

#[instrument(skip_all)]
async fn login(
    State(state): State<AppState>,
    repository: Repository,
    jar: CookieJar,
    locale: Locale,
    Form(form): Form<LoginForm>,
) -> Result<(CookieJar, Redirect), AppError> {
    match authenticate_form(&state, &repository, &jar, form).await {
        Ok(user) => {
            let jar = start_session(jar, &user, &repository, &state.config).await?;
            Ok((jar, Redirect::to("/")))
        }
        // Erro de negócio vira banner na própria tela de login; erro interno
        // propaga (o `?` do business_flash) para o fluxo de 500.
        Err(error) => {
            let flash = business_flash(error, locale.strings())?;
            Ok((
                set_flash(jar, &flash, state.config.cookie_secure),
                Redirect::to("/login"),
            ))
        }
    }
}

/// O miolo do login: CSRF, lockout e conferência de credencial.
async fn authenticate_form(
    state: &AppState,
    repository: &Repository,
    jar: &CookieJar,
    form: LoginForm,
) -> Result<User, AppError> {
    verify_csrf(jar, &form.csrf_token)?;
    let username = form.username.trim().to_string();

    // Lockout ANTES de conferir a senha: durante o bloqueio nem a senha certa
    // passa, então força bruta não extrai sinal nenhum das tentativas.
    state.login_throttle.ensure_allowed(&username).await?;

    match UnauthenticatedUser::new(username.clone(), form.password)
        .authenticate(repository)
        .await
    {
        Ok(user) => {
            state.login_throttle.record_success(&username).await;
            Ok(user)
        }
        // Só falhas de credencial alimentam o contador — sondagem de username
        // e senha errada contam igual; erro de banco não.
        Err(error @ (AppError::InvalidCredentials | AppError::UserDoesNotExist)) => {
            state.login_throttle.record_failure(&username).await;
            Err(error)
        }
        Err(error) => Err(error),
    }
}

#[instrument(skip_all)]
async fn register(
    State(state): State<AppState>,
    repository: Repository,
    jar: CookieJar,
    locale: Locale,
    Form(form): Form<LoginForm>,
) -> Result<(CookieJar, Redirect), AppError> {
    let outcome = async {
        verify_csrf(&jar, &form.csrf_token)?;
        UnauthenticatedUser::new(form.username, form.password)
            .register(&repository)
            .await
    }
    .await;

    match outcome {
        Ok(user) => {
            let jar = start_session(jar, &user, &repository, &state.config).await?;
            Ok((jar, Redirect::to("/")))
        }
        Err(error) => {
            let flash = business_flash(error, locale.strings())?;
            Ok((
                set_flash(jar, &flash, state.config.cookie_secure),
                Redirect::to("/register"),
            ))
        }
    }
}

/// Exporta o extrato completo do usuário autenticado como download CSV.
#[instrument(skip_all)]
async fn transactions_csv(
    SessionUser(user): SessionUser,
    repository: Repository,
) -> Result<impl axum::response::IntoResponse, AppError> {
    let transactions = repository.list_all_transactions(user.id()).await?;

    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"extrato.csv\"",
            ),
        ],
        transactions_to_csv(&transactions),
    ))
}

/// Monta o CSV do extrato no padrão pt-BR de planilha: separador `;` e decimais
/// com vírgula (é o que o Excel/LibreOffice em português esperam). Campos de
/// texto vão entre aspas, com aspas internas dobradas (RFC 4180).
fn transactions_to_csv(transactions: &[Transaction]) -> String {
    let mut csv = String::from("data;tipo;ativo;quantidade;preco_unitario;movimento_caixa\n");

    for tx in transactions {
        let date = tx
            .created_at
            .format(
                &time::format_description::parse("[year]-[month]-[day] [hour]:[minute]")
                    .expect("static format"),
            )
            .unwrap_or_default();
        let kind = match tx.kind.as_str() {
            "deposit" => "deposito",
            "buy" => "compra",
            "sell" => "venda",
            other => other,
        };
        let asset = tx.asset_name.as_deref().unwrap_or("-");
        let quantity = tx
            .quantity
            .map(|q| decimal_ptbr(&q))
            .unwrap_or_else(|| "-".to_string());
        let unit_value = tx
            .unit_value
            .map(|v| decimal_ptbr(&v))
            .unwrap_or_else(|| "-".to_string());

        csv.push_str(&format!(
            "{date};{kind};{};{quantity};{unit_value};{}\n",
            csv_field(asset),
            decimal_ptbr(&tx.cash_delta)
        ));
    }

    csv
}

/// Decimal com vírgula, sem zeros supérfluos.
fn decimal_ptbr(value: &Decimal) -> String {
    value.normalize().to_string().replace('.', ",")
}

/// Campo de texto CSV: aspas em volta, aspas internas dobradas.
fn csv_field(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Emite o par de cookies da sessão: o JWT de acesso (curto) e o refresh token
/// (longo), cujo registro vai para a tabela `sessions` — é ele que permite
/// renovar o acesso sem novo login e revogar a sessão no logout.
async fn start_session(
    jar: CookieJar,
    user: &User,
    repository: &Repository,
    config: &Config,
) -> Result<CookieJar, AppError> {
    let refresh = RefreshToken::generate();
    repository
        .create_session(user.id(), &refresh.hash(), session_expiry(config))
        .await?;

    Ok(jar
        .add(access_cookie(user, config)?)
        .add(refresh_cookie(&refresh, config)))
}

/// Remove os cookies e tenta revogar o refresh no servidor, inclusive suas
/// cópias. Um JWT de acesso já copiado continua válido até expirar; ele não
/// consulta a tabela de sessões.
#[instrument(skip_all)]
async fn logout(repository: Repository, jar: CookieJar) -> (CookieJar, Redirect) {
    if let Some(refresh) = jar.get(REFRESH_COOKIE) {
        // Falha ao revogar (banco fora etc.) não impede o logout local; a
        // sessão ainda expira sozinha pelo expires_at.
        let _ = repository
            .revoke_session(&hash_token(refresh.value()))
            .await;
    }

    (
        jar.remove(Cookie::build(TOKEN_COOKIE).path("/").build())
            .remove(Cookie::build(REFRESH_COOKIE).path("/").build()),
        Redirect::to("/login"),
    )
}

#[instrument(skip_all)]
async fn index(maybe_user: Option<User>) -> Redirect {
    match maybe_user {
        Some(_) => Redirect::to("/assets"),
        None => Redirect::to("/login"),
    }
}

/// Tudo o que o miolo da carteira (o `<main id="wallet">`) precisa para se
/// desenhar. É compartilhado pelos DOIS templates: a página completa
/// (`assets.html`, que inclui o fragmento) e o fragmento sozinho
/// (`wallet.html`), devolvido nas requisições parciais do htmx.
struct WalletData {
    holdings: Vec<Holding>,
    available_assets: Vec<Asset>,
    transactions: Vec<Transaction>,
    summary: WalletSummary,
    action: WalletAction,
    csrf_token: String,
    page: u32,
    has_prev: bool,
    has_next: bool,
    flash: Option<Flash>,
    chart: EquityChart,
    t: &'static Strings,
}

impl WalletData {
    // Qual formulário está aberto. Os templates usam isto para marcar o botão
    // correspondente como ativo (`aria-current`), já que o askama não avalia
    // `matches!` sobre o enum direto.
    fn is_deposit(&self) -> bool {
        matches!(self.action, WalletAction::Deposit)
    }

    fn is_buy(&self) -> bool {
        matches!(self.action, WalletAction::Buy)
    }

    fn is_sell(&self) -> bool {
        matches!(self.action, WalletAction::Sell)
    }

    fn deposit_is_primary(&self) -> bool {
        matches!(self.action, WalletAction::None | WalletAction::Deposit)
    }

    fn new(
        view: WalletView,
        action: WalletAction,
        csrf_token: String,
        flash: Option<Flash>,
        locale: Locale,
    ) -> Self {
        Self {
            holdings: view.holdings,
            available_assets: view.available_assets,
            transactions: view.transactions,
            summary: view.summary,
            action,
            csrf_token,
            page: view.page,
            has_prev: view.has_prev,
            has_next: view.has_next,
            flash,
            chart: view.chart,
            t: locale.strings(),
        }
    }
}

#[derive(Template)]
#[template(path = "assets.html")]
struct AssetsPage {
    user: User,
    wallet: WalletData,
    t: &'static Strings,
}

#[derive(Template)]
#[template(path = "wallet.html")]
struct WalletFragment {
    wallet: WalletData,
}

/// Dados da tela de mercado.
///
/// As moedas chegam por REFERÊNCIA ao snapshot que o job mantém: a
/// renderização não copia a lista (nem as séries temporais dentro dela), só
/// empresta enquanto desenha. O `Arc` do snapshot fica vivo no handler até o
/// HTML estar pronto.
struct MarketData<'a> {
    /// O que a lista lateral mostra: todas as moedas ou o resultado da busca.
    coins: Vec<&'a Coin>,
    /// A moeda em foco no painel. `None` só antes da primeira rodada do job.
    selected: Option<&'a Coin>,
    /// Série projetada da moeda em foco. `None` quando a fonte não mandou
    /// pontos suficientes para desenhar uma linha.
    chart: Option<PriceChart>,
    range: Range,
    /// Termo da busca, devolvido ao campo para a tela não esquecer o filtro.
    query: String,
    /// URL do estado atual (moeda + período + busca): é o que o poller de 60 s
    /// repete para atualizar os números sem mexer no que o usuário escolheu.
    state_url: String,
    day_url: String,
    week_url: String,
    updated_at: Option<time::OffsetDateTime>,
    refresh_failed: bool,
    t: &'static Strings,
}

impl MarketData<'_> {
    /// URL que seleciona esta moeda preservando período e busca. É o `href`
    /// real do link — sem JavaScript, clicar na lista continua trocando a
    /// moeda do painel.
    fn coin_url(&self, coin: &Coin) -> String {
        market_url(&coin.id, self.range, &self.query)
    }

    fn is_selected(&self, coin: &Coin) -> bool {
        self.selected.is_some_and(|selected| selected.id == coin.id)
    }

    fn is_day(&self) -> bool {
        self.range.is_day()
    }

    fn is_week(&self) -> bool {
        self.range.is_week()
    }

    /// Agregado em BRL na forma compacta das mesas de operação.
    fn compact_brl(&self, value: &Decimal) -> String {
        compact_brl(value, self.t)
    }

    /// Mesma escala, sem moeda — a oferta em circulação é contada em unidades
    /// da própria cripto.
    fn compact_units(&self, value: &Decimal) -> String {
        compact_units(value, self.t)
    }
}

#[derive(Template)]
#[template(path = "market.html")]
struct MarketPage<'a> {
    user: User,
    market: MarketData<'a>,
    t: &'static Strings,
}

#[derive(Template)]
#[template(path = "market_dashboard.html")]
struct MarketFragment<'a> {
    market: MarketData<'a>,
}

#[derive(Deserialize)]
struct MarketQuery {
    /// Identificador da CoinGecko da moeda em foco.
    coin: Option<String>,
    /// Janela do gráfico (`24h` ou `7d`).
    range: Option<String>,
    /// Busca da lista lateral.
    q: Option<String>,
}

/// Tela de mercado: painel da moeda selecionada e a lista lateral com todas as
/// variações.
///
/// Só lê o snapshot que o job de segundo plano mantém — nunca chama a API de
/// fora no caminho da requisição. Assim a página responde no mesmo tempo com
/// um usuário ou com mil, o limite da fonte gratuita não depende de quantas
/// pessoas abriram a tela, e trocar de moeda ou de período não custa uma
/// chamada externa: os dados dos 100 ativos já estão em memória.
#[instrument(skip_all)]
async fn market_page(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    hx: HxRequest,
    locale: Locale,
    Query(query): Query<MarketQuery>,
) -> Result<Html<String>, AppError> {
    let snapshot = state.market.snapshot().await;
    let range = query
        .range
        .as_deref()
        .and_then(Range::from_tag)
        .unwrap_or_default();
    let needle = search_needle(query.q.as_deref());

    let selected = snapshot.select(query.coin.as_deref());
    let coins: Vec<&Coin> = snapshot
        .coins
        .iter()
        .filter(|coin| needle.is_empty() || coin.matches(&needle))
        .collect();

    let selected_id = selected.map(|coin| coin.id.as_str()).unwrap_or_default();
    let market = MarketData {
        chart: selected.and_then(|coin| coin.chart(range, snapshot.updated_at)),
        coins,
        selected,
        range,
        state_url: market_url(selected_id, range, &needle),
        day_url: market_url(selected_id, Range::Day, &needle),
        week_url: market_url(selected_id, Range::Week, &needle),
        query: needle,
        updated_at: snapshot.updated_at,
        refresh_failed: snapshot.refresh_failed,
        t: locale.strings(),
    };

    // Com htmx volta só o painel (a própria página repõe os pedaços que
    // mudaram); sem htmx, a página inteira — o fluxo clássico continua
    // funcionando, inclusive a seleção de moeda, que é um link comum.
    let html = if hx.0 {
        MarketFragment { market }.render()?
    } else {
        MarketPage {
            user,
            market,
            t: locale.strings(),
        }
        .render()?
    };

    Ok(Html(html))
}

/// Termo de busca normalizado: minúsculo, sem espaços nas pontas e limitado —
/// o campo é livre, o custo da varredura não pode ser.
fn search_needle(raw: Option<&str>) -> String {
    const MAX_CHARS: usize = 32;

    raw.unwrap_or_default()
        .trim()
        .to_lowercase()
        .chars()
        .take(MAX_CHARS)
        .collect()
}

/// Monta a URL da tela preservando o estado que o usuário construiu: moeda em
/// foco, período do gráfico e busca. É o que faz um link comum (sem htmx) e o
/// botão voltar do navegador levarem à MESMA tela.
fn market_url(coin: &str, range: Range, query: &str) -> String {
    let mut url = format!("/market?coin={}&range={}", query_escape(coin), range.tag());
    if !query.is_empty() {
        url.push_str("&q=");
        url.push_str(&query_escape(query));
    }
    url
}

/// Percent-encoding do que entra numa query string.
///
/// O askama já escapa HTML, o que impede o valor de escapar do atributo — mas
/// não impede um `&` de virar separador e partir a URL em dois parâmetros. O
/// termo de busca é texto livre do usuário; ele passa por aqui antes de virar
/// link.
fn query_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());

    for byte in value.bytes() {
        match byte {
            // Os "unreserved" da RFC 3986 seguem literais; todo o resto vai
            // como %XX, inclusive espaço, acento e separadores de query.
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                escaped.push(byte as char);
            }
            _ => escaped.push_str(&format!("%{byte:02X}")),
        }
    }

    escaped
}

/// Agregado em BRL na forma compacta que mesa de operação usa: "R$ 6,51 tri"
/// em vez de catorze dígitos.
///
/// Vale só para número informativo (capitalização, volume, oferta). Nada da
/// carteira passa por aqui: saldo, posição e resultado saem com todas as casas
/// que o `Decimal` guarda.
fn compact_brl(value: &Decimal, t: &Strings) -> String {
    if value.is_zero() {
        return NOT_PUBLISHED.to_string();
    }
    let (digits, unit) = compact_parts(value, t);
    format!("{}R$ {digits}{unit}", filters::ptbr_sign(value))
}

fn compact_units(value: &Decimal, t: &Strings) -> String {
    if value.is_zero() {
        return NOT_PUBLISHED.to_string();
    }
    let (digits, unit) = compact_parts(value, t);
    format!("{}{digits}{unit}", filters::ptbr_sign(value))
}

/// Zero, nestes agregados, é "a fonte não publicou" (ver `market.rs`) — e um
/// traço diz isso; "R$ 0,00" mentiria, parecendo medição.
const NOT_PUBLISHED: &str = "—";

fn compact_parts(value: &Decimal, t: &Strings) -> (String, String) {
    let scales = [
        (12u32, t.unit_trillion),
        (9, t.unit_billion),
        (6, t.unit_million),
        (3, t.unit_thousand),
    ];

    for (exponent, unit) in scales {
        let factor = Decimal::from(10u64.pow(exponent));
        if value.abs() >= factor {
            // Arredonda ANTES de formatar: a formatação com precisão trunca, e
            // um volume de 98,765 mi apareceria como 98,76 mi.
            return (
                filters::ptbr_digits(&(value / factor).round_dp(2), 2, false),
                format!(" {unit}"),
            );
        }
    }

    (filters::ptbr_digits(value, 2, false), String::new())
}

enum WalletAction {
    None,
    Deposit,
    Buy,
    Sell,
}

#[derive(Deserialize)]
struct PageQuery {
    page: Option<u32>,
}

#[instrument(skip_all)]
async fn assets_page(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    portfolio: PortfolioService,
    jar: CookieJar,
    hx: HxRequest,
    locale: Locale,
    Query(query): Query<PageQuery>,
) -> Result<(CookieJar, Html<String>), AppError> {
    render_wallet(RenderWallet {
        state,
        user,
        portfolio,
        jar,
        hx,
        locale,
        action: WalletAction::None,
        page: query.page,
    })
    .await
}

#[instrument(skip_all)]
async fn deposit_page(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    portfolio: PortfolioService,
    jar: CookieJar,
    hx: HxRequest,
    locale: Locale,
    Query(query): Query<PageQuery>,
) -> Result<(CookieJar, Html<String>), AppError> {
    render_wallet(RenderWallet {
        state,
        user,
        portfolio,
        jar,
        hx,
        locale,
        action: WalletAction::Deposit,
        page: query.page,
    })
    .await
}

#[instrument(skip_all)]
async fn buy_page(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    portfolio: PortfolioService,
    jar: CookieJar,
    hx: HxRequest,
    locale: Locale,
    Query(query): Query<PageQuery>,
) -> Result<(CookieJar, Html<String>), AppError> {
    render_wallet(RenderWallet {
        state,
        user,
        portfolio,
        jar,
        hx,
        locale,
        action: WalletAction::Buy,
        page: query.page,
    })
    .await
}

#[instrument(skip_all)]
async fn sell_page(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    portfolio: PortfolioService,
    jar: CookieJar,
    hx: HxRequest,
    locale: Locale,
    Query(query): Query<PageQuery>,
) -> Result<(CookieJar, Html<String>), AppError> {
    render_wallet(RenderWallet {
        state,
        user,
        portfolio,
        jar,
        hx,
        locale,
        action: WalletAction::Sell,
        page: query.page,
    })
    .await
}

/// Tudo que uma renderização da carteira precisa (os handlers acima só variam
/// a `action` e o resto vem dos extratores).
struct RenderWallet {
    state: AppState,
    user: User,
    portfolio: PortfolioService,
    jar: CookieJar,
    hx: HxRequest,
    locale: Locale,
    action: WalletAction,
    page: Option<u32>,
}

/// O handler só faz HTTP: garante o token CSRF, pede a visão pronta ao serviço
/// e renderiza o template. Toda a montagem (consultas, paginação) é do serviço.
/// Requisição parcial (htmx) recebe só o fragmento da carteira; navegação
/// normal (e restauração de histórico) recebe a página completa.
async fn render_wallet(request: RenderWallet) -> Result<(CookieJar, Html<String>), AppError> {
    let RenderWallet {
        state,
        user,
        portfolio,
        jar,
        hx,
        locale,
        action,
        page,
    } = request;

    let (jar, flash) = take_flash(jar);
    let (jar, csrf_token) = ensure_csrf_token(jar, state.config.cookie_secure);

    let view = portfolio.wallet_view(user.id(), page.unwrap_or(1)).await?;
    let wallet = WalletData::new(view, action, csrf_token, flash, locale);

    let html = if hx.0 {
        WalletFragment { wallet }.render()?
    } else {
        AssetsPage {
            user,
            wallet,
            t: locale.strings(),
        }
        .render()?
    };
    Ok((jar, Html(html)))
}

#[derive(Deserialize)]
struct AmountForm {
    amount: Decimal,
    csrf_token: String,
}

/// Como apresentar o desfecho de uma operação da carteira: a mensagem de
/// sucesso e, no erro de negócio, para qual formulário voltar (caminho + qual
/// seção de formulário re-abrir), para o banner aparecer no lugar certo e o
/// `autofocus` reposicionar o foco.
struct OperationFeedback {
    on_success: Flash,
    error_path: &'static str,
    error_action: WalletAction,
    locale: Locale,
}

/// Converte o desfecho de uma operação da carteira na resposta certa para cada
/// modo. Sucesso vira a mensagem dada, erro de negócio vira banner, erro
/// interno propaga.
///
/// - Clássico (sem JavaScript): flash em cookie + redirect — o padrão PRG de
///   sempre, em duas requisições.
/// - Parcial (htmx): o fragmento da carteira já atualizado volta NA MESMA
///   resposta, com o flash inline (nada de cookie: a mensagem não deve
///   sobreviver a um F5) e `HX-Push-Url` para a barra de endereço acompanhar.
///   Uma requisição só, sem recarregar a página.
async fn wallet_outcome(
    state: &AppState,
    user: User,
    portfolio: &PortfolioService,
    jar: CookieJar,
    hx: HxRequest,
    outcome: Result<(), AppError>,
    feedback: OperationFeedback,
) -> Result<Response, AppError> {
    let (flash, path, action) = match outcome {
        Ok(()) => (feedback.on_success, "/assets", WalletAction::None),
        Err(error) => (
            business_flash(error, feedback.locale.strings())?,
            feedback.error_path,
            feedback.error_action,
        ),
    };

    if hx.0 {
        let (jar, csrf_token) = ensure_csrf_token(jar, state.config.cookie_secure);
        let view = portfolio.wallet_view(user.id(), 1).await?;
        let fragment = WalletFragment {
            wallet: WalletData::new(view, action, csrf_token, Some(flash), feedback.locale),
        };
        Ok((jar, [("hx-push-url", path)], Html(fragment.render()?)).into_response())
    } else {
        Ok((
            set_flash(jar, &flash, state.config.cookie_secure),
            Redirect::to(path),
        )
            .into_response())
    }
}

#[instrument(skip_all)]
async fn deposit(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    portfolio: PortfolioService,
    jar: CookieJar,
    hx: HxRequest,
    locale: Locale,
    Form(form): Form<AmountForm>,
) -> Result<Response, AppError> {
    let outcome = async {
        verify_csrf(&jar, &form.csrf_token)?;
        portfolio.deposit(user.id(), form.amount).await
    }
    .await;

    wallet_outcome(
        &state,
        user,
        &portfolio,
        jar,
        hx,
        outcome,
        OperationFeedback {
            on_success: Flash::success(locale.strings().flash_deposit_done),
            error_path: "/deposit",
            error_action: WalletAction::Deposit,
            locale,
        },
    )
    .await
}

#[derive(Deserialize)]
struct TradeAssetForm {
    asset_id: i64,
    quantity: Decimal,
    csrf_token: String,
}

#[instrument(skip_all)]
async fn buy_asset(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    portfolio: PortfolioService,
    jar: CookieJar,
    hx: HxRequest,
    locale: Locale,
    Form(form): Form<TradeAssetForm>,
) -> Result<Response, AppError> {
    let outcome = async {
        verify_csrf(&jar, &form.csrf_token)?;
        portfolio.buy(user.id(), form.asset_id, form.quantity).await
    }
    .await;

    wallet_outcome(
        &state,
        user,
        &portfolio,
        jar,
        hx,
        outcome,
        OperationFeedback {
            on_success: Flash::success(locale.strings().flash_buy_done),
            error_path: "/buy",
            error_action: WalletAction::Buy,
            locale,
        },
    )
    .await
}

#[instrument(skip_all)]
async fn sell_asset(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    portfolio: PortfolioService,
    jar: CookieJar,
    hx: HxRequest,
    locale: Locale,
    Form(form): Form<TradeAssetForm>,
) -> Result<Response, AppError> {
    let outcome = async {
        verify_csrf(&jar, &form.csrf_token)?;
        portfolio
            .sell(user.id(), form.asset_id, form.quantity)
            .await
    }
    .await;

    wallet_outcome(
        &state,
        user,
        &portfolio,
        jar,
        hx,
        outcome,
        OperationFeedback {
            on_success: Flash::success(locale.strings().flash_sell_done),
            error_path: "/sell",
            error_action: WalletAction::Sell,
            locale,
        },
    )
    .await
}

/// O formulário de sincronizar cotações não tem campo de dado nenhum, mas ainda
/// é um POST que muda estado — então também carrega (e valida) o token CSRF.
#[derive(Deserialize)]
struct SyncQuotesForm {
    csrf_token: String,
}

#[instrument(skip_all)]
async fn sync_quotes(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    portfolio: PortfolioService,
    jar: CookieJar,
    hx: HxRequest,
    locale: Locale,
    Form(form): Form<SyncQuotesForm>,
) -> Result<Response, AppError> {
    let outcome = async {
        verify_csrf(&jar, &form.csrf_token)?;
        state
            .quote_sync
            .run(&Repository::from_state(&state), true)
            .await
            .map(|_| ())
    }
    .await;

    wallet_outcome(
        &state,
        user,
        &portfolio,
        jar,
        hx,
        outcome,
        OperationFeedback {
            on_success: Flash::success(locale.strings().flash_quotes_done),
            error_path: "/assets",
            error_action: WalletAction::None,
            locale,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use time::macros::datetime;

    /// Snapshot de mercado com uma moeda em alta, uma em baixa e uma parada.
    fn test_coins() -> Vec<Coin> {
        vec![
            Coin {
                id: "bitcoin".into(),
                rank: 1,
                symbol: "BTC".into(),
                name: "Bitcoin".into(),
                price: dec!(325611.00),
                change_24h: dec!(2.50),
                change_1h: dec!(0.40),
                change_7d: dec!(-1.20),
                market_cap: dec!(6512345678901),
                volume_24h: dec!(98765432.10),
                high_24h: dec!(330000),
                low_24h: dec!(320000),
                ath: dec!(400000),
                ath_change_pct: dec!(-18.60),
                circulating_supply: dec!(19800000),
                // Série horária de sete dias, como a fonte entrega.
                series: (0..168)
                    .map(|hour| 300_000.0 + hour as f64 * 100.0)
                    .collect(),
            },
            Coin {
                id: "ethereum".into(),
                rank: 2,
                symbol: "ETH".into(),
                name: "Ethereum".into(),
                price: dec!(9636.59),
                change_24h: dec!(-3.00),
                ..Coin::default()
            },
            Coin {
                id: "tether".into(),
                rank: 3,
                symbol: "USDT".into(),
                name: "Tether".into(),
                price: dec!(5.13),
                change_24h: dec!(0),
                ..Coin::default()
            },
            Coin {
                id: "tiny-coin".into(),
                rank: 4,
                symbol: "TINY".into(),
                name: "Tiny Coin".into(),
                price: dec!(0.00004125),
                change_24h: dec!(1),
                ..Coin::default()
            },
        ]
    }

    /// Monta o fragmento como o handler monta: selecionando pelo id, filtrando
    /// pela busca e derivando as URLs que preservam o estado da tela.
    fn test_market<'a>(coins: &'a [Coin], coin: Option<&str>, query: &str) -> MarketData<'a> {
        let selected = coin
            .and_then(|id| coins.iter().find(|coin| coin.id == id))
            .or_else(|| coins.first());
        let selected_id = selected.map(|coin| coin.id.as_str()).unwrap_or_default();

        MarketData {
            coins: coins
                .iter()
                .filter(|coin| query.is_empty() || coin.matches(query))
                .collect(),
            selected,
            chart: selected
                .and_then(|coin| coin.chart(Range::Week, Some(datetime!(2026-07-28 01:07 UTC)))),
            range: Range::Week,
            state_url: market_url(selected_id, Range::Week, query),
            day_url: market_url(selected_id, Range::Day, query),
            week_url: market_url(selected_id, Range::Week, query),
            query: query.to_string(),
            updated_at: Some(datetime!(2026-07-28 01:07 UTC)),
            refresh_failed: false,
            t: Locale::PtBr.strings(),
        }
    }

    /// O invariante de acessibilidade do painel: verde e vermelho medem ΔE
    /// ~4,6 sob deuteranopia, então **nenhuma variação pode ser comunicada só
    /// por cor**. Toda variação tem de trazer seta E sinal — este teste é o
    /// que trava isso contra uma refatoração distraída do template.
    #[test]
    fn market_dashboard_marks_direction_with_arrow_and_sign_not_only_colour() {
        let coins = test_coins();
        let html = MarketFragment {
            market: test_market(&coins, None, ""),
        }
        .render()
        .expect("render");

        // Alta: seta para cima e sinal de mais, além da classe de cor.
        assert!(html.contains("▲"), "falta a seta de alta");
        assert!(html.contains("+2,50%"), "falta o sinal e o valor da alta");
        assert!(html.contains("text-up"), "falta a cor semântica da alta");
        // Baixa: seta para baixo e sinal de menos.
        assert!(html.contains("▼"), "falta a seta de baixa");
        assert!(html.contains("−3,00%"), "falta o sinal e o valor da baixa");
        assert!(html.contains("text-down"), "falta a cor semântica da baixa");
        // Parada: sem seta, sem sinal.
        assert!(html.contains("0,00%"), "falta a variação nula");

        // O filtro já traz o símbolo da moeda; ele já duplicou ("R$ R$").
        assert!(html.contains("R$ 325.611,00"), "preço em pt-BR");
        assert!(!html.contains("R$ R$"), "símbolo da moeda repetido");
        assert!(
            html.contains("R$ 0,00004125"),
            "cotação pequena não pode virar R$ 0,00"
        );

        let css = include_str!("../../static/app.css");
        assert!(
            css.contains(".text-up{") && css.contains(".text-down{"),
            "as classes semânticas renderizadas precisam existir no CSS compilado"
        );
    }

    /// O painel abre na moeda de maior capitalização e mostra os indicadores
    /// da carteira digital: variações, capitalização, volume, faixa do dia e a
    /// série temporal.
    #[test]
    fn market_dashboard_shows_the_selected_coin_with_its_time_series() {
        let coins = test_coins();
        let html = MarketFragment {
            market: test_market(&coins, None, ""),
        }
        .render()
        .expect("render");

        assert!(html.contains("Bitcoin") && html.contains("BTC/BRL"));
        // Agregados grandes saem compactos, com o sufixo do idioma.
        assert!(html.contains("R$ 6,51 tri"), "capitalização compacta");
        assert!(html.contains("R$ 98,77 mi"), "volume compacto");
        assert!(html.contains("19,80 mi"), "oferta em circulação");
        // Faixa de negociação: marcador dentro do medidor, mínima e máxima.
        assert!(html.contains("R$ 320.000,00") && html.contains("R$ 330.000,00"));
        // 325.611 em [320.000, 330.000]: 56,11% da faixa, já em coordenada.
        assert!(html.contains(r#"x="335.4""#), "marcador da faixa do dia");
        // Série temporal: linha, eixo do tempo e a janela ativa marcada.
        assert!(html.contains("<path d=\"M10.00"), "linha do gráfico");
        assert!(html.contains("28/07"), "rótulo do eixo do tempo");
        assert!(
            html.contains(r#"href="/market?coin=bitcoin&#38;range=24h""#),
            "link da janela de 24 h"
        );

        // O painel se repõe sozinho preservando a moeda em foco, e as duas
        // regiões vivas da tela existem para o htmx trocar.
        assert!(html.contains(r#"hx-get="/market?coin=bitcoin&#38;range=7d""#));
        assert!(html.contains(r#"hx-trigger="every 60s""#));
        assert!(html.contains(r#"id="market-detail""#));
        assert!(html.contains(r#"id="market-list""#));
        assert!(html.contains(r#"id="market-state""#));
        assert!(html.contains(r##"hx-select-oob="#market-list,#market-state""##));
    }

    /// Selecionar uma moeda é um link comum: o estado inteiro da tela (moeda,
    /// período e busca) viaja na URL, então funciona sem JavaScript nenhum.
    #[test]
    fn market_dashboard_selects_by_id_and_keeps_the_state_in_every_link() {
        let coins = test_coins();
        let html = MarketFragment {
            market: test_market(&coins, Some("ethereum"), ""),
        }
        .render()
        .expect("render");

        assert!(html.contains("Ethereum"));
        assert!(
            html.contains(r#"aria-current="true" class="flex items-center"#),
            "a linha da moeda em foco precisa se anunciar como atual"
        );
        assert!(html.contains(r#"href="/market?coin=tether&#38;range=7d""#));

        // Id desconhecido não deixa a tela vazia: cai na primeira do ranking.
        let fallback = MarketFragment {
            market: test_market(&coins, Some("nao-existe"), ""),
        }
        .render()
        .expect("render");
        assert!(fallback.contains("Bitcoin"));
    }

    #[test]
    fn market_search_filters_the_side_list_without_losing_the_selection() {
        let coins = test_coins();
        let html = MarketFragment {
            market: test_market(&coins, Some("bitcoin"), "eth"),
        }
        .render()
        .expect("render");

        assert!(html.contains("Ethereum"));
        assert!(!html.contains("Tiny Coin"), "a busca filtra a lista");
        // O painel continua na moeda escolhida, e o termo volta para o campo.
        assert!(html.contains(r#"value="eth""#));
        assert!(html.contains("R$ 325.611,00"), "painel intacto");
        // O formulário de busca dispara para o caminho NU: quem manda moeda,
        // período e termo são os campos dele. Uma query aqui repetiria os
        // parâmetros na URL e o extrator recusaria a requisição.
        assert!(html.contains(r#"hx-get="/market" hx-trigger="submit"#));
        // Todo link carrega a busca junto, para o filtro sobreviver ao clique.
        assert!(html.contains(r#"href="/market?coin=ethereum&#38;range=7d&#38;q=eth""#));

        let vazio = MarketFragment {
            market: test_market(&coins, Some("bitcoin"), "zzz"),
        }
        .render()
        .expect("render");
        assert!(vazio.contains("nenhuma moeda encontrada"));
    }

    #[test]
    fn market_dashboard_shows_a_status_message_before_the_first_round() {
        let empty: Vec<Coin> = Vec::new();
        let html = MarketFragment {
            market: test_market(&empty, None, ""),
        }
        .render()
        .expect("render");

        assert!(
            html.contains(r#"role="status""#),
            "leitor de tela precisa saber"
        );
        assert!(html.contains("buscando as cotações"));
        assert!(!html.contains("id=\"market-list\""), "sem dados, sem lista");
        // Enquanto não há painel, a tela inteira se reconstrói na próxima
        // rodada — as regiões internas ainda não existem para trocar.
        assert!(html.contains(r##"hx-select="#market""##));

        let mut unavailable = test_market(&empty, None, "");
        unavailable.refresh_failed = true;
        let html = MarketFragment {
            market: unavailable,
        }
        .render()
        .expect("render");
        assert!(html.contains("mercado indisponível"));
        assert!(!html.contains("buscando as cotações"));
    }

    #[test]
    fn compact_scale_keeps_big_aggregates_readable() {
        let pt = Locale::PtBr.strings();

        assert_eq!(compact_brl(&dec!(6512345678901), pt), "R$ 6,51 tri");
        assert_eq!(compact_brl(&dec!(1500000000), pt), "R$ 1,50 bi");
        assert_eq!(compact_brl(&dec!(98765432.10), pt), "R$ 98,77 mi");
        assert_eq!(compact_brl(&dec!(1234.5), pt), "R$ 1,23 mil");
        assert_eq!(compact_brl(&dec!(999.99), pt), "R$ 999,99");
        assert_eq!(compact_units(&dec!(19800000), pt), "19,80 mi");
        assert_eq!(compact_brl(&dec!(-2500000), pt), "- R$ 2,50 mi");
        // Zero, nestes campos, é "não publicado" — e um traço diz isso melhor
        // do que um "R$ 0,00" que parece um dado real.
        assert_eq!(compact_brl(&Decimal::ZERO, pt), "—");

        // A abreviação é palavra, e palavra se traduz; o número segue a
        // convenção do dado (BRL), como o resto da interface.
        assert_eq!(
            compact_brl(&dec!(1500000000), Locale::En.strings()),
            "R$ 1,50 B"
        );
    }

    #[test]
    fn market_urls_percent_encode_free_text() {
        assert_eq!(
            market_url("bitcoin", Range::Week, ""),
            "/market?coin=bitcoin&range=7d"
        );
        // `&`, `=` e espaço não podem partir a query em parâmetros novos.
        assert_eq!(
            market_url("bitcoin", Range::Day, "a&b=c d"),
            "/market?coin=bitcoin&range=24h&q=a%26b%3Dc%20d"
        );
        assert_eq!(query_escape("moeda-é_1.0~"), "moeda-%C3%A9_1.0~");

        // O termo chega normalizado (minúsculo, aparado e limitado).
        assert_eq!(search_needle(Some("  BTC  ")), "btc");
        assert_eq!(search_needle(None), "");
        assert_eq!(search_needle(Some(&"x".repeat(100))).len(), 32);
    }

    #[test]
    fn htmx_marks_partial_requests_but_history_restore_wants_the_full_page() {
        let mut headers = HeaderMap::new();
        assert!(!is_partial_request(&headers), "navegação normal");

        headers.insert("hx-request", "true".parse().unwrap());
        assert!(is_partial_request(&headers), "requisição do htmx");

        // Voltar/avançar com o cache de histórico expirado: o htmx refaz o GET
        // mas precisa da página inteira para reconstruir o DOM.
        headers.insert("hx-history-restore-request", "true".parse().unwrap());
        assert!(!is_partial_request(&headers), "restauração de histórico");
    }

    #[test]
    fn unauthenticated_pages_redirect_the_whole_browser_for_classic_and_htmx_requests() {
        let classic = unauthenticated_page_response(&HeaderMap::new());
        assert_eq!(classic.status(), StatusCode::SEE_OTHER);
        assert_eq!(classic.headers().get(header::LOCATION).unwrap(), "/login");
        assert!(!classic.headers().contains_key("hx-redirect"));

        let mut htmx_headers = HeaderMap::new();
        htmx_headers.insert("hx-request", HeaderValue::from_static("true"));
        let htmx = unauthenticated_page_response(&htmx_headers);

        assert_eq!(htmx.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(htmx.headers().get("hx-redirect").unwrap(), "/login");
        assert!(!htmx.headers().contains_key(header::LOCATION));
    }

    #[test]
    fn language_switch_only_follows_local_absolute_paths() {
        assert_eq!(sanitized_next(Some("/assets")), "/assets");
        assert_eq!(sanitized_next(Some("/login")), "/login");
        // Protocolo-relativo, URL absoluta e lixo caem no fallback seguro.
        assert_eq!(sanitized_next(Some("//evil.example")), "/");
        assert_eq!(sanitized_next(Some("https://evil.example")), "/");
        assert_eq!(sanitized_next(Some("assets")), "/");
        assert_eq!(sanitized_next(None), "/");
    }

    fn test_wallet(locale: Locale) -> WalletData {
        WalletData {
            holdings: vec![],
            available_assets: vec![],
            transactions: vec![],
            summary: WalletSummary {
                balance: dec!(10),
                holdings_value: Decimal::ZERO,
                total_value: dec!(10),
                total_invested: Decimal::ZERO,
                total_delta: Decimal::ZERO,
            },
            action: WalletAction::None,
            csrf_token: "tok".to_string(),
            page: 1,
            has_prev: false,
            has_next: false,
            t: locale.strings(),
            flash: Some(Flash::success("depósito realizado.")),
            chart: EquityChart::empty(),
        }
    }

    /// O campo de depósito carrega `data-money-input` (o gancho da máscara em
    /// `money-input.js`) e continua um `<input type="number">` de verdade no
    /// HTML puro — a máscara é aditiva via JS, nunca uma reescrita do
    /// formulário que o servidor manda.
    #[test]
    fn deposit_amount_field_is_a_plain_number_input_hooked_for_the_mask() {
        let mut wallet = test_wallet(Locale::PtBr);
        wallet.action = WalletAction::Deposit;
        let fragment = WalletFragment { wallet }.render().expect("render");

        assert!(fragment.contains(r#"name="amount" type="number""#));
        assert!(fragment.contains("data-money-input"));
        assert!(fragment.contains(r#"min="0.01""#), "guarda nativa continua");
        assert!(fragment.contains("required"));
    }

    /// `money-input.js` depende de `window.htmx` já carregado (usa
    /// `htmx.onLoad` para reanexar a máscara depois de cada troca de
    /// fragmento) — por isso tem de vir DEPOIS do script do htmx no
    /// documento, e os dois com `defer` (que preserva a ordem do documento).
    #[test]
    fn money_input_script_loads_after_htmx_with_defer() {
        let full = AssetsPage {
            user: User::new(1, "breno".to_string(), "user".to_string()),
            wallet: test_wallet(Locale::PtBr),
            t: Locale::PtBr.strings(),
        }
        .render()
        .expect("render");

        let htmx_at = full.find("/static/htmx.js").expect("htmx.js referenciado");
        let mask_at = full
            .find("/static/money-input.js")
            .expect("money-input.js referenciado");
        assert!(
            htmx_at < mask_at,
            "money-input.js precisa vir depois do htmx"
        );
        for script in ["/static/htmx.js", "/static/money-input.js"] {
            let tag_start = full.find(script).expect("script presente");
            let tag = &full[full[..tag_start].rfind("<script").unwrap()..];
            let tag_end = tag.find('>').unwrap();
            assert!(tag[..tag_end].contains("defer"), "{script} sem defer");
        }
    }

    #[test]
    fn the_wallet_fragment_is_partial_html_embedded_by_the_full_page() {
        let fragment = WalletFragment {
            wallet: test_wallet(Locale::PtBr),
        }
        .render()
        .expect("fragment renders");

        // O fragmento é exatamente o alvo do swap (`outerHTML` de #wallet),
        // com o flash inline — e nada de esqueleto de página em volta.
        assert!(fragment.starts_with("<main id=\"wallet\">"));
        assert!(!fragment.contains("<!DOCTYPE"));
        assert!(fragment.contains("depósito realizado."));

        let full = AssetsPage {
            user: User::new(1, "breno".to_string(), "user".to_string()),
            wallet: test_wallet(Locale::PtBr),
            t: Locale::PtBr.strings(),
        }
        .render()
        .expect("full page renders");

        // A página completa embute o MESMO fragmento (id único do swap) dentro
        // do esqueleto, com o htmx carregado do próprio binário.
        assert!(full.contains("<!DOCTYPE html>"));
        assert!(full.contains("<main id=\"wallet\">"));
        assert!(full.contains("/static/htmx.js"));
        assert_eq!(full.matches("id=\"wallet\"").count(), 1);
    }

    /// A CSP fecha `script-src` e `style-src` em `'self'` (nada de
    /// `'unsafe-inline'`). Isso só se sustenta enquanto NENHUMA página emitir
    /// `<style>` ou `<script>` inline — um bloco desses passaria despercebido
    /// em revisão e o navegador simplesmente o ignoraria em produção, gerando
    /// um bug visual difícil de rastrear. O teste trava o invariante.
    #[test]
    fn pages_carry_no_inline_style_or_script() {
        let full = AssetsPage {
            user: User::new(1, "breno".to_string(), "user".to_string()),
            wallet: test_wallet(Locale::PtBr),
            t: Locale::PtBr.strings(),
        }
        .render()
        .expect("page renders");

        let login = LoginPage {
            is_register: false,
            csrf_token: "tok".to_string(),
            flash: None,
            t: Locale::PtBr.strings(),
        }
        .render()
        .expect("login renders");

        // O painel de mercado é a tela mais gráfica do produto (medidor,
        // série temporal, eixo) — e nada disso pode virar estilo inline.
        let coins = test_coins();
        let market = MarketPage {
            user: User::new(1, "breno".to_string(), "user".to_string()),
            market: test_market(&coins, None, ""),
            t: Locale::PtBr.strings(),
        }
        .render()
        .expect("market renders");

        for (name, html) in [("assets", &full), ("login", &login), ("market", &market)] {
            assert!(!html.contains("<style"), "{name}: <style> inline");
            // Todo `<script>` da página tem de ser externo (`src=`).
            for fragment in html.split("<script").skip(1) {
                let tag = fragment.split('>').next().unwrap_or_default();
                assert!(tag.contains("src="), "{name}: <script> sem src ({tag})");
            }
            assert!(html.contains("/static/app.css"), "{name}: css externo");
        }
    }

    /// O CSS e os scripts moram numa URL fixa e mudam a cada build. Se o
    /// navegador puder usar a cópia guardada sem perguntar, um rebuild deixa a
    /// tela com o HTML novo e o estilo velho — o layout de mercado empilhado
    /// que motivou esta política. A etiqueta é o que faz a pergunta caber num
    /// 304.
    #[tokio::test]
    async fn static_assets_revalidate_by_content_and_answer_304_when_unchanged() {
        let css = app_css(HeaderMap::new()).await;
        assert_eq!(css.status(), StatusCode::OK);

        let tag = css
            .headers()
            .get(header::ETAG)
            .expect("etag")
            .to_str()
            .expect("ascii")
            .to_string();
        assert!(tag.starts_with('"') && tag.ends_with('"'), "formato: {tag}");
        assert_eq!(
            css.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, no-cache",
            "cache sem revalidação serviria estilo de outro binário"
        );

        // Mesma etiqueta de volta: o corpo não desce outra vez.
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, tag.parse().unwrap());
        assert_eq!(
            app_css(headers.clone()).await.status(),
            StatusCode::NOT_MODIFIED
        );

        // Etiqueta de outra versão (e a marca de "fraca", que o navegador pode
        // acrescentar) não podem confundir a comparação.
        let mut stale = HeaderMap::new();
        stale.insert(header::IF_NONE_MATCH, "\"deadbeef\"".parse().unwrap());
        assert_eq!(app_css(stale).await.status(), StatusCode::OK);

        let mut weak = HeaderMap::new();
        weak.insert(
            header::IF_NONE_MATCH,
            format!("\"outra\", W/{tag}").parse().unwrap(),
        );
        assert_eq!(app_css(weak).await.status(), StatusCode::NOT_MODIFIED);

        // Cada asset tem a própria etiqueta: um rebuild que só mexe no CSS não
        // pode invalidar o htmx, nem o contrário.
        let js = htmx_js(HeaderMap::new()).await;
        assert_ne!(js.headers().get(header::ETAG).unwrap(), tag.as_str());
        assert_eq!(
            money_input_js(headers).await.status(),
            StatusCode::OK,
            "a etiqueta do CSS não vale para outro arquivo"
        );
    }

    #[test]
    fn the_wallet_page_renders_in_both_languages() {
        let render = |locale: Locale| {
            AssetsPage {
                user: User::new(1, "breno".to_string(), "user".to_string()),
                wallet: test_wallet(locale),
                t: locale.strings(),
            }
            .render()
            .expect("page renders")
        };

        let pt = render(Locale::PtBr);
        assert!(pt.contains("lang=\"pt-BR\""));
        assert!(pt.contains("posições"));
        assert!(pt.contains("patrimônio"));

        let en = render(Locale::En);
        assert!(en.contains("lang=\"en\""));
        assert!(en.contains("positions"));
        assert!(en.contains("net worth"));
        // O dinheiro continua na convenção do DADO (BRL), não da interface.
        assert!(en.contains("R$ 10,00"));
    }

    #[test]
    fn the_login_page_renders_in_both_languages() {
        let render = |locale: Locale, is_register: bool| {
            LoginPage {
                is_register,
                csrf_token: "tok".to_string(),
                flash: None,
                t: locale.strings(),
            }
            .render()
            .expect("login renders")
        };

        assert!(render(Locale::PtBr, false).contains("entre para acessar"));
        assert!(render(Locale::En, false).contains("sign in"));
        assert!(render(Locale::En, true).contains("create account"));
    }

    #[test]
    fn csv_export_formats_the_statement_in_ptbr_conventions() {
        let transactions = vec![
            Transaction {
                id: 2,
                kind: "buy".to_string(),
                asset_name: Some("bitcoin \"btc\"".to_string()),
                quantity: Some(dec!(0.50)),
                unit_value: Some(dec!(100.25)),
                cash_delta: dec!(-50.125),
                created_at: datetime!(2026-07-17 12:30 UTC),
            },
            Transaction {
                id: 1,
                kind: "deposit".to_string(),
                asset_name: None,
                quantity: None,
                unit_value: None,
                cash_delta: dec!(1000),
                created_at: datetime!(2026-07-17 12:00 UTC),
            },
        ];

        let csv = transactions_to_csv(&transactions);
        let lines: Vec<&str> = csv.lines().collect();

        assert_eq!(
            lines[0],
            "data;tipo;ativo;quantidade;preco_unitario;movimento_caixa"
        );
        // Decimais com vírgula, aspas internas dobradas, tipo em pt-BR.
        assert_eq!(
            lines[1],
            "2026-07-17 12:30;compra;\"bitcoin \"\"btc\"\"\";0,5;100,25;-50,125"
        );
        // Depósito não tem ativo/quantidade/preço.
        assert_eq!(lines[2], "2026-07-17 12:00;deposito;\"-\";-;-;1000");
    }
}

pub mod filters {
    use askama::Values;
    use rust_decimal::Decimal;
    use time::OffsetDateTime;

    #[askama::filter_fn]
    pub fn human_datetime(value: &OffsetDateTime, _: &dyn Values) -> askama::Result<String> {
        let format = time::format_description::parse("[year]-[month]-[day] [hour]:[minute]")
            .map_err(askama::Error::custom)?;

        value.format(&format).map_err(askama::Error::custom)
    }

    #[askama::filter_fn]
    pub fn money(value: &Decimal, _: &dyn Values) -> askama::Result<String> {
        Ok(format_brl(value, 2, false))
    }

    /// Cotação de mercado em BRL. Acima de R$ 1, centavos bastam; abaixo
    /// disso mantemos até oito casas e removemos zeros finais sem cair no
    /// enganoso "R$ 0,00" para criptoativos de preço muito baixo.
    #[askama::filter_fn]
    pub fn market_price(value: &Decimal, _: &dyn Values) -> askama::Result<String> {
        if value.abs() >= Decimal::ONE {
            Ok(format_brl(value, 2, false))
        } else {
            Ok(format_brl(value, 8, true))
        }
    }

    fn format_brl(value: &Decimal, decimal_places: usize, trim_fraction: bool) -> String {
        format!(
            "{}R$ {}",
            ptbr_sign(value),
            ptbr_digits(value, decimal_places, trim_fraction)
        )
    }

    /// O sinal sai separado dos dígitos porque o símbolo da moeda fica ENTRE
    /// os dois: "- R$ 1,00", como num extrato.
    pub(super) fn ptbr_sign(value: &Decimal) -> &'static str {
        if value.is_sign_negative() { "- " } else { "" }
    }

    /// Valor absoluto no padrão pt-BR: milhar com ponto, decimal com vírgula.
    pub(super) fn ptbr_digits(
        value: &Decimal,
        decimal_places: usize,
        trim_fraction: bool,
    ) -> String {
        let raw = format!("{:.*}", decimal_places, value.abs());
        let (integer, fraction) = raw.split_once('.').unwrap_or((raw.as_str(), ""));

        let mut grouped = String::new();
        for (index, character) in integer.chars().rev().enumerate() {
            if index > 0 && index % 3 == 0 {
                grouped.push('.');
            }
            grouped.push(character);
        }
        let integer = grouped.chars().rev().collect::<String>();
        let mut fraction = fraction.to_string();
        if trim_fraction {
            while fraction.len() > 2 && fraction.ends_with('0') {
                fraction.pop();
            }
        }

        format!("{integer},{fraction}")
    }

    #[askama::filter_fn]
    pub fn quantity(value: &Decimal, _: &dyn Values) -> askama::Result<String> {
        Ok(value.normalize().to_string())
    }

    #[askama::filter_fn]
    pub fn nonnegative(value: &Decimal, _: &dyn Values) -> askama::Result<bool> {
        Ok(*value >= Decimal::ZERO)
    }

    #[askama::filter_fn]
    pub fn positive(value: &Decimal, _: &dyn Values) -> askama::Result<bool> {
        Ok(*value > Decimal::ZERO)
    }

    #[askama::filter_fn]
    pub fn negative(value: &Decimal, _: &dyn Values) -> askama::Result<bool> {
        Ok(*value < Decimal::ZERO)
    }

    /// Percentual em valor ABSOLUTO, com vírgula decimal. O sinal fica por
    /// conta do template, que o emite junto da seta ▲/▼ — direção nunca é
    /// comunicada só pela cor (verde e vermelho são indistinguíveis para
    /// deuteranopia; medido: ΔE 4,6).
    #[askama::filter_fn]
    pub fn percent(value: &Decimal, _: &dyn Values) -> askama::Result<String> {
        Ok(format!("{:.2}", value.abs()).replace('.', ","))
    }
}
