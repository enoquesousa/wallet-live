# Arquitetura de componentes

## Objetivo

Ficha técnica de cada componente do sistema: responsabilidade, entradas, saídas,
dependências, o que persiste, como falha e como a falha é tratada. Serve como
referência de consulta pontual — "o que é o `QuoteSync` e o que acontece se ele
falhar?" — sem exigir a leitura da narrativa completa.

## Escopo

Coberto: os 18 componentes de `src/`, as regras de dependência entre camadas e os
pontos de extensão. Não coberto: a narrativa de como o sistema é montado (ver
[system-overview.md](system-overview.md)), os fluxos e diagramas (ver
[data-flow.md](data-flow.md)) e a justificativa de cada tecnologia (ver
[technology-decisions.md](technology-decisions.md)).

---

## 1. Mapa de camadas e regras de dependência

```text
┌──────────────────────────────────────────────────────────────┐
│  routes/          HTTP puro: form/JSON, CSRF, redirect,      │
│  (api, frontend,  flash, escolha fragmento vs página         │
│   flash)                                                     │
├──────────────────────────────────────────────────────────────┤
│  services/        Orquestração: consultas concorrentes,      │
│  (portfolio)      paginação, projeção de gráfico             │
├──────────────────────────────────────────────────────────────┤
│  repository.rs    TODO o SQL; validação na borda da escrita  │
├──────────────────────────────────────────────────────────────┤
│  PostgreSQL                                                   │
└──────────────────────────────────────────────────────────────┘

transversais (usados por qualquer camada acima do repository):
  models.rs · error.rs · config.rs · i18n.rs · auth/* · quotes.rs · market.rs
```

### Dependências permitidas

| De | Para | Permitido |
| --- | --- | --- |
| `routes/*` | `services/*`, `auth/*`, `i18n`, `models`, `error`, `repository` | Sim |
| `services/*` | `repository` (via trait `PortfolioRepository`), `models`, `error` | Sim |
| `repository` | `models`, `error`, `sqlx` | Sim |
| `quotes` | `repository`, `models`, `error` | Sim |
| `market` | `models`, `error` | Sim |
| Qualquer módulo | `error::AppError` | Sim |

### Dependências proibidas

| Regra | Por que | Como é sustentada |
| --- | --- | --- |
| `repository` não conhece HTTP | Um `StatusCode` no repository tornaria o SQL dependente do transporte; a mesma consulta serviria pior a um job em segundo plano | Verificável: `src/repository.rs` não importa nada de `axum::http` |
| `repository` não conhece `services` nem `routes` | A dependência é unidirecional para baixo | Verificável nos `use` do arquivo |
| `services/portfolio` não conhece `Repository` concreto | É genérico sobre `PortfolioRepository`, o que permite testar a orquestração sem Postgres | `PortfolioService<R: PortfolioRepository = Repository>` |
| `market` não escreve no banco | O snapshot é dado de terceiro, volátil e informativo; gravá-lo misturaria cotação externa com o catálogo que lastreia operações | `src/market.rs` não importa `Repository` |
| Nenhum módulo do núcleo financeiro usa `f64` | Ponto flutuante em dinheiro carrega ruído de arredondamento | `f64` aparece só em coordenadas de SVG (`services/portfolio.rs`, `market.rs`) |
| Nenhum template emite `<style>`/`<script>` inline | A CSP fecha `script-src`/`style-src` em `'self'` | Travado por teste: `pages_carry_no_inline_style_or_script` |

---

## 2. Fichas de componente

### 2.1 `App` / `AppState` — composição e boot

| Campo | Descrição |
| --- | --- |
| **Nome** | `App`, `AppState` |
| **Responsabilidade** | Instalar hooks de erro, ler `.env`, inicializar tracing, validar configuração, conectar ao banco, aplicar migrações, subir os dois jobs, montar o router e servir com desligamento gracioso |
| **Entradas** | Variáveis de ambiente do processo |
| **Saídas** | Servidor HTTP escutando em `BIND_ADDR`; `AppState` clonável injetado em todo handler |
| **Dependências** | `Config`, `PgPool`, `LoginThrottle`, `QuoteSync`, `Market`, `RequestMetrics` |
| **Persistência** | Nenhuma própria; detém a pool de conexões |
| **Falhas possíveis** | Segredo obrigatório ausente; `BIND_ADDR` malformado; banco inacessível; migração falha; porta ocupada |
| **Tratamento de erro** | **Todas fatais no boot** (`?` sobe até a `main`, que devolve `color_eyre::Result`). É deliberado: subir contra schema pela metade ou sem `JWT_SECRET` produziria 401 confusos em produção em vez de uma falha clara |
| **Evidência** | `src/app.rs` · `App::start`, `App::router`, `AppState::build`, `AppState::with_pool`, `shutdown_signal` |

`AppState::with_pool` existe separado de `build` para que a suíte de integração
monte o **mesmo** estado que a produção monta, a partir de uma pool que o
`#[sqlx::test]` já entrega migrada — sem exigir os segredos de ambiente.

### 2.2 `Config` — configuração validada uma vez

| Campo | Descrição |
| --- | --- |
| **Nome** | `Config` |
| **Responsabilidade** | Ler e validar o ambiente **uma única vez** no boot (*fail-fast*) |
| **Entradas** | 12 variáveis de ambiente (ver [../getting-started/configuration.md](../getting-started/configuration.md)) |
| **Saídas** | Struct imutável em `Arc`, compartilhada por todo o processo |
| **Dependências** | `dotenvy` (carrega `.env` se existir), `color_eyre` |
| **Persistência** | Nenhuma |
| **Falhas possíveis** | `DATABASE_URL`/`ADMIN_SECRET_KEY`/`JWT_SECRET` ausentes ou **vazios**; TTL igual a zero; `BIND_ADDR` não parseável |
| **Tratamento de erro** | Erro fatal com mensagem nomeando a variável. Segredo em branco é rejeitado como ausente — "um segredo em branco é tão perigoso quanto um ausente" |
| **Evidência** | `src/config.rs` · `Config::from_env`, `required`, `optional_positive`, `optional_non_negative` |

Nota de comportamento não óbvio: `COOKIE_SECURE` é comparado **literalmente** com
`"true"`. Qualquer outro valor — inclusive `"1"`, `"TRUE"`, `"yes"` — resulta em
`false`, silenciosamente. Ver o débito **DT-04** em
[../decisions/technical-debt.md](../decisions/technical-debt.md).

### 2.3 Middleware `request_tracing`

| Campo | Descrição |
| --- | --- |
| **Nome** | `request_tracing` (camada mais externa) |
| **Responsabilidade** | Abrir o span da requisição com `request_id`, medir latência, alimentar o histograma e devolver o id na resposta |
| **Entradas** | Requisição HTTP; header `x-request-id` (opcional) |
| **Saídas** | Resposta com `x-request-id`; um evento de log `request completed`; um ponto no histograma `http.server.request.duration` |
| **Dependências** | `tracing`, `opentelemetry`, `AppState.metrics` |
| **Persistência** | Nenhuma |
| **Falhas possíveis** | `request_id` externo malicioso (injeção de lixo em log); cardinalidade ilimitada de métrica |
| **Tratamento de erro** | Id externo é aceito só se não vazio, ≤ 64 caracteres e apenas alfanuméricos ASCII ou `-`; senão gera-se um local. A métrica é rotulada pelo **padrão de rota** (`MatchedPath`), não pela URL crua — um 404 em caminho aleatório vira `<unmatched>` em vez de uma série nova |
| **Evidência** | `src/app.rs` · `request_tracing`, `new_request_id`, `REQUEST_ID_HEADER` |

### 2.4 Middleware `security_headers`

| Campo | Descrição |
| --- | --- |
| **Nome** | `security_headers` |
| **Responsabilidade** | Aplicar cabeçalhos de segurança a **toda** resposta, inclusive erros e 404 |
| **Entradas** | Requisição (só o caminho, para detectar `/static/`) e a resposta do que vem depois |
| **Saídas** | Resposta com CSP, `nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store` (exceto assets) e HSTS condicional |
| **Dependências** | `AppState.config.cookie_secure` |
| **Persistência** | Nenhuma |
| **Falhas possíveis** | Nenhuma em runtime — os valores são `HeaderValue::from_static` |
| **Tratamento de erro** | Não aplicável |
| **Evidência** | `src/app.rs` · `security_headers`; testes `every_api_response_carries_the_security_headers`, `pages_carry_no_inline_style_or_script` |

CSP efetiva: `default-src 'self'; script-src 'self'; style-src 'self'; img-src
'self' data:; connect-src 'self'; frame-ancestors 'none'; form-action 'self';
base-uri 'self'; object-src 'none'`.

### 2.5 Middleware `refresh_session`

| Campo | Descrição |
| --- | --- |
| **Nome** | `refresh_session` |
| **Responsabilidade** | Renovar a sessão sem novo login: access expirado + refresh válido ⇒ rotaciona e emite os dois cookies |
| **Entradas** | Cookies `token` e `refresh_token` |
| **Saídas** | `User` nas `extensions` da requisição; `Set-Cookie` de ambos os tokens na resposta |
| **Dependências** | `Repository::rotate_session`, `auth::session`, `Config` |
| **Persistência** | Sim — revoga a linha antiga e insere a nova em `sessions`, numa transação |
| **Falhas possíveis** | Sessão inexistente, revogada ou expirada; erro de banco; falha ao assinar o novo JWT |
| **Tratamento de erro** | **Nenhuma delas interrompe a requisição.** Todas caem no fluxo normal, e o extrator `User` de cada rota produz o 401 ou o redirecionamento habitual. É deliberado: um erro de renovação não deve virar 500 quando o caminho correto é pedir login |
| **Evidência** | `src/auth/session.rs` · `refresh_session`; `src/repository.rs` · `rotate_session` |

Roda **antes** dos extratores para que a rota receba o `User` renovado. Ignora
`/login`, `/register` e `/logout` em todos os métodos: esses handlers precisam
controlar seus cookies sem uma renovação posterior da conta anterior.

### 2.6 `routes::frontend` — SSR

| Campo | Descrição |
| --- | --- |
| **Nome** | `routes::frontend` |
| **Responsabilidade** | 16 rotas HTML: login/cadastro/logout, carteira, operações, mercado, CSV, assets estáticos, troca de idioma |
| **Entradas** | Formulários `application/x-www-form-urlencoded`, query strings, cookies, header `HX-Request` |
| **Saídas** | HTML (página completa ou fragmento), redirecionamentos 303, CSV, CSS/JS estático |
| **Dependências** | `PortfolioService`, `Repository`, `auth::*`, `i18n`, `market`, `routes::flash`, templates Askama |
| **Persistência** | Indireta, sempre via `PortfolioService`/`Repository` |
| **Falhas possíveis** | CSRF divergente; erro de negócio (saldo/posição insuficiente); sessão ausente; template mal formado; parâmetro de query inválido |
| **Tratamento de erro** | Erro de negócio vira **flash message** e redireciona ao formulário de origem com `autofocus` — nunca JSON cru na tela. Sessão ausente redireciona (`303`, ou `HX-Redirect` em requisição htmx). Erro interno sobe como `AppError` e é censurado |
| **Evidência** | `src/routes/frontend.rs` · `router`, `render_wallet`, `is_partial_request`, `authenticate_form`, `logout`, `set_language` |

Os handlers são deliberadamente finos: `render_wallet` garante o token CSRF, pede
a visão pronta ao serviço e decide entre página inteira e fragmento. Nenhuma regra
de negócio mora ali.

### 2.7 `routes::api` — API REST administrativa

| Campo | Descrição |
| --- | --- |
| **Nome** | `routes::api` |
| **Responsabilidade** | CRUD parcial do catálogo de ativos em JSON, e servir a especificação OpenAPI |
| **Entradas** | JSON (`CreateAssetRequest`, `UpdateAssetRequest`); credencial de admin |
| **Saídas** | `Asset` em JSON (dinheiro como **string**), spec OpenAPI |
| **Dependências** | `Admin` (extrator), `Repository`, `utoipa` |
| **Persistência** | Sim, via `Repository::create_asset`/`update_asset` |
| **Falhas possíveis** | Credencial inválida/ausente; JSON malformado; nome vazio; preço negativo; id inexistente |
| **Tratamento de erro** | `Admin` recusa antes de o handler rodar; validação no repository devolve `400`; id inexistente vira `404` explícito (`AppError::AssetDoesNotExist`), não `200` silencioso |
| **Evidência** | `src/routes/api.rs` · `router`, `list_assets`, `create_asset`, `update_asset`, `ApiDoc` |

### 2.8 `routes::flash` — mensagens de uso único

| Campo | Descrição |
| --- | --- |
| **Nome** | `Flash`, `set_flash`, `take_flash`, `business_flash` |
| **Responsabilidade** | Transportar o resultado de uma operação do POST para o GET seguinte |
| **Entradas** | `Flash::success`/`Flash::error`; cookie `flash` |
| **Saídas** | Banner acessível (`role=alert`/`role=status`) no HTML |
| **Dependências** | `i18n::Strings` (para traduzir erros de negócio), `base64` |
| **Persistência** | Cookie `flash`, `HttpOnly`, `SameSite=Strict`, `Max-Age` de 1 minuto |
| **Falhas possíveis** | Valor de cookie malformado; acento corrompido no transporte |
| **Tratamento de erro** | Valor malformado é **descartado silenciosamente** (a tela aparece sem banner, o que é melhor que um erro). O texto viaja em base64 justamente para acentos sobreviverem ao cookie |
| **Evidência** | `src/routes/flash.rs` · `Flash`, `set_flash`, `take_flash`; teste `flash_roundtrips_through_the_cookie_including_accents` |

Só erros de **negócio** viram flash. Erro interno segue o caminho do 500 —
`business_errors_become_messages_and_internal_errors_do_not` trava a distinção.

### 2.9 `services::portfolio` — orquestração da carteira

| Campo | Descrição |
| --- | --- |
| **Nome** | `PortfolioService<R: PortfolioRepository = Repository>` |
| **Responsabilidade** | Montar a `WalletView` completa numa chamada, paginar o extrato, projetar o gráfico de patrimônio, repassar operações |
| **Entradas** | `user_id`, número de página, parâmetros de operação |
| **Saídas** | `WalletView` (resumo, posições, ativos negociáveis, extrato paginado, `EquityChart`) |
| **Dependências** | Apenas o trait `PortfolioRepository` — nunca o `Repository` concreto |
| **Persistência** | Nenhuma própria |
| **Falhas possíveis** | Qualquer falha de repositório; série de gráfico curta ou constante |
| **Tratamento de erro** | Erro do repositório é **repassado sem reinterpretação** (três testes travam isso: `deposit_result_flows_through_unchanged` e os dois equivalentes de compra/venda) — uma camada que "melhora" o erro esconde a causa. Série com menos de 2 pontos devolve `EquityChart::empty()`, e o template omite o gráfico; série constante vira reta no meio, sem divisão por zero |
| **Evidência** | `src/services/portfolio.rs` · `PortfolioService`, `PortfolioRepository`, `WalletView`, `EquityChart`, `equity_chart` |

As seis consultas independentes rodam **concorrentes** com `tokio::try_join!`: o
tempo total é o da mais lenta, não a soma. Constantes:
`TRANSACTIONS_PAGE_SIZE = 25`, `CHART_POINTS = 60`.

### 2.10 `repository` — todo o acesso ao banco

| Campo | Descrição |
| --- | --- |
| **Nome** | `Repository` |
| **Responsabilidade** | **Todo** o SQL do sistema (24 métodos públicos) e a validação de entrada na borda da escrita |
| **Entradas** | Tipos de `models`; `Decimal` para valores monetários |
| **Saídas** | Tipos de `models`; `sqlx::Result` ou `Result<_, AppError>` |
| **Dependências** | `PgPool`, `models`, `error` |
| **Persistência** | 6 tabelas: `users`, `assets`, `holdings`, `transactions`, `sessions`, `portfolio_snapshots` |
| **Falhas possíveis** | Saldo insuficiente; posição insuficiente; ativo inexistente; nome vazio; preço negativo; escala acima de `MONEY_SCALE`; total que arredonda a zero; violação de `UNIQUE`; falha de conexão |
| **Tratamento de erro** | Erros de **negócio** são variantes tipadas de `AppError` (`InsufficientBalance`, `InsufficientHoldings`, `TradeTooSmall`…) e produzem 4xx. Erro de banco vira `AppError::Database` ⇒ 500 censurado. Violação de `UNIQUE` em `username` é traduzida para `UsernameTaken` em vez de virar 500 genérico. Operações monetárias rodam em transação com `FOR UPDATE`; qualquer recusa **reverte tudo** |
| **Evidência** | `src/repository.rs` · `deposit`, `buy_asset`, `sell_asset`, `rotate_session`, `revoke_session`, `wallet_summary`, `list_holdings`, `record_portfolio_snapshots`, `ensure_market_asset`, `update_known_asset_prices`, `validated_asset_name`, `validated_unit_value` |

Invariante de escala em duas pontas: **escrita** arredonda para `MONEY_SCALE`;
**leitura** envolve todo agregado em `ROUND(..., 8)`, porque produtos e somas de
`NUMERIC` acumulam escala sem limite e estourariam os 28 dígitos significativos
do `Decimal` mesmo com cada coluna dentro do invariante.

### 2.11 `auth::user` — identidade e senha

| Campo | Descrição |
| --- | --- |
| **Nome** | `User`, `UnauthenticatedUser` |
| **Responsabilidade** | Autenticar por senha, cadastrar, emitir e validar o JWT de acesso, e ser o extrator que protege rotas |
| **Entradas** | Formulário (username/senha); cookie `token` |
| **Saídas** | `User` (id, username, role) — campos privados; JWT HS256 assinado |
| **Dependências** | `jwt-simple` (HS256), `password-auth` (argon2), `Repository`, `Config` |
| **Persistência** | Indireta: lê/insere em `users` via `Repository` |
| **Falhas possíveis** | Credencial inválida; usuário inexistente; username em uso; registro fora dos limites (username 3–32, senha 8–128); token fabricado/expirado; hash armazenada ilegível |
| **Tratamento de erro** | Erros normais viram variantes de `AppError` (401/400/404). **Exceção deliberada:** hash de senha que não parseia causa `panic!` — significa que o registro entrou por outra via e continuar seria operar em estado inconsistente |
| **Evidência** | `src/auth/user.rs` · `User::auth_token`, `User::from_auth_token`, `UnauthenticatedUser::authenticate`, `register`, `valid_registration`, `TOKEN_COOKIE` |

Os campos de `User` são privados de propósito: a única forma de obter um `User` é
passando por um fluxo de autenticação, então **ter um `User` em mãos é prova de
que o fluxo foi cumprido**. O `role` vem das claims assinadas — consequência: um
rebaixamento de admin passa a valer em um novo token. Os JWTs já emitidos mantêm
o papel até `exp + 30 s` (até 10 min e 30 s por padrão); revogar o refresh não
antecipa esse limite.

### 2.12 `auth::session` — refresh token

| Campo | Descrição |
| --- | --- |
| **Nome** | `RefreshToken`, `hash_token`, `access_cookie`, `refresh_cookie` |
| **Responsabilidade** | Gerar o refresh token opaco, derivar sua hash e construir os cookies de sessão |
| **Entradas** | Aleatoriedade do SO (32 bytes); `Config` |
| **Saídas** | Token base64 url-safe (só em memória e no cookie); SHA-256 para o banco; `Cookie` configurados |
| **Dependências** | `rand::rngs::OsRng`, `sha2`, `base64`, `axum-extra` |
| **Persistência** | Só a **hash** vai para `sessions.token_hash`; o valor em claro nunca toca o banco |
| **Falhas possíveis** | Falha de assinatura do JWT ao montar o cookie de acesso |
| **Tratamento de erro** | `access_cookie` devolve `Result`; o middleware trata como "não renovar" e segue |
| **Evidência** | `src/auth/session.rs` · `RefreshToken::generate`, `hash`, `hash_token`, `access_cookie`, `refresh_cookie`, `session_expiry` |

Cookies: `HttpOnly`, `SameSite=Strict`, `Secure` conforme `COOKIE_SECURE`,
`Path=/`, `Max-Age` alinhado ao TTL correspondente.

### 2.13 `auth::admin` — autorização administrativa

| Campo | Descrição |
| --- | --- |
| **Nome** | `Admin` (extrator) |
| **Responsabilidade** | Autorizar as escritas do catálogo por dois caminhos alternativos |
| **Entradas** | Cookie de sessão **ou** header `Authorization` |
| **Saídas** | `Admin` (unit struct — carrega apenas a prova de autorização) |
| **Dependências** | `User`, `Config.admin_secret_key`, `subtle::ConstantTimeEq` |
| **Persistência** | Nenhuma — o `role` vem das claims assinadas, sem consulta extra |
| **Falhas possíveis** | Sessão sem papel admin; header ausente; header divergente; header não-ASCII |
| **Tratamento de erro** | Sessão válida **sem** papel admin retorna erro **imediatamente**, sem cair para o header: "ele claramente está usando a sessão, negar já". Evita que um usuário comum autenticado ganhe autorização por acidente ao mandar um `Authorization` de outra finalidade. A comparação do segredo é em **tempo constante** |
| **Evidência** | `src/auth/admin.rs` · `Admin::from_request_parts`; teste `writing_to_the_catalogue_requires_the_admin_credential` |

### 2.14 `auth::csrf`

| Campo | Descrição |
| --- | --- |
| **Nome** | `ensure_csrf_token`, `verify_csrf` |
| **Responsabilidade** | Proteção CSRF no padrão *double-submit cookie* |
| **Entradas** | `CookieJar`; token submetido no campo oculto do formulário |
| **Saídas** | Jar com o cookie `csrf` e o token para embutir no HTML |
| **Dependências** | `rand::rngs::OsRng`, `base64`, `subtle` |
| **Persistência** | Cookie `csrf` (`HttpOnly`, `SameSite=Strict`, sem `Max-Age` — cookie de sessão do navegador) |
| **Falhas possíveis** | Cookie ausente; token divergente; token vazio |
| **Tratamento de erro** | Qualquer um dos três ⇒ `AppError::CsrfMismatch` ⇒ **403**. Ausência de cookie é recusa, não permissão — é o que faz o *double-submit* valer |
| **Evidência** | `src/auth/csrf.rs` · `ensure_csrf_token`, `verify_csrf`, `random_token`; 3 testes de unidade + `forms_without_a_matching_csrf_token_are_refused` |

O token **não rotaciona por página**: reutilizar o existente evita que duas abas
abertas invalidem uma à outra. É defesa em profundidade — o `SameSite=Strict` já
bloqueia a maior parte do CSRF em navegadores modernos.

### 2.15 `auth::throttle` — lockout de login

| Campo | Descrição |
| --- | --- |
| **Nome** | `LoginThrottle` |
| **Responsabilidade** | Mitigar força bruta contando falhas consecutivas **por usuário** e impondo backoff exponencial |
| **Entradas** | Username (normalizado com `trim().to_lowercase()`) |
| **Saídas** | `Ok(())` ou `AppError::TooManyAttempts` (**429**) |
| **Dependências** | `tokio::sync::Mutex<HashMap<String, Entry>>` |
| **Persistência** | **Em memória apenas** — reinicia com o processo |
| **Falhas possíveis** | Crescimento ilimitado do mapa por usernames inventados; perda do estado no restart; ineficácia com múltiplas réplicas |
| **Tratamento de erro** | Acima de `PRUNE_THRESHOLD = 4096` entradas, as vencidas são varridas ao registrar nova falha. Falhas mais antigas que `FORGET_AFTER` (1 h) são perdoadas |
| **Evidência** | `src/auth/throttle.rs` · `LoginThrottle::ensure_allowed`, `record_failure`, `record_success`, `lock_duration`, `normalize`; 4 testes |

Parâmetros: 5 tentativas livres, primeiro bloqueio de 30 s dobrando a cada falha,
teto de 15 min. A checagem roda **antes** de conferir a senha — durante o bloqueio
nem a senha correta passa, o que remove o lucro do ataque.

**Limitação estrutural, documentada e não resolvida:** com mais de uma réplica, o
lockout é por instância. Ver **DT-01** em
[../decisions/technical-debt.md](../decisions/technical-debt.md).

### 2.16 `quotes` — cotações que lastreiam operações

| Campo | Descrição |
| --- | --- |
| **Nome** | `QuoteSync`, `sync_quotes_round`, `brl_price`, `parse_brl_rates` |
| **Responsabilidade** | Atualizar `assets.unit_value` com taxas de câmbio reais e registrar o snapshot de patrimônio de todos os usuários |
| **Entradas** | `GET api.coinbase.com/v2/exchange-rates?currency=BRL` |
| **Saídas** | Preços atualizados no catálogo; linhas em `portfolio_snapshots`; contagem de ativos atualizados |
| **Dependências** | `reqwest` (timeout de 15 s), `Repository`, `MARKET_PAIRS` |
| **Persistência** | Sim — `assets`, `portfolio_snapshots` (via `Repository`) |
| **Falhas possíveis** | Fonte indisponível; timeout; corpo malformado; taxa que não cabe no `Decimal`; par ausente na resposta; taxa ≤ 0 |
| **Tratamento de erro** | Falha de rodada é **logada como `warn` e a próxima tenta de novo** — cotação atrasada não derruba o serviço. Par ausente é pulado sem afetar os outros. Taxa não invertível vira `None`. Corpo malformado vira `AppError::Payload` ⇒ 502 |
| **Evidência** | `src/quotes.rs` · `QuoteSync::run`, `spawn_scheduled_sync`, `sync_market_quotes`, `fetch_brl_rates`, `parse_brl_rates`, `brl_price`, `MARKET_PAIRS` |

Concorrência: `Mutex<Option<Instant>>` **adquirido durante a rodada inteira** —
duas chamadas simultâneas (botão manual + job agendado) nunca disparam duas
requisições nem gravam dois snapshots. Chamadas manuais têm cooldown de 30 s
(`MANUAL_SYNC_COOLDOWN`) e recebem `QuoteSyncTooSoon` (429) dentro dele.

Pares suportados: USD, EUR, BTC, ETH, SOL — cada um com aliases que casam no
catálogo. Além deles, `real` e `brl` são injetados com preço fixo `1` (a moeda de
denominação não precisa de cotação).

O preço é o **inverso** da taxa (BRL→USD = 0,2 ⇒ 1 USD = 5 BRL), sempre
arredondado para `MONEY_SCALE`. O arredondamento não é cosmético: foi a causa
raiz do incidente de 2026-07-22.

### 2.17 `market` — snapshot informativo

| Campo | Descrição |
| --- | --- |
| **Nome** | `Market`, `Coin`, `PriceChart`, `Range`, `parse_markets` |
| **Responsabilidade** | Manter em memória o snapshot das 100 maiores criptomoedas em BRL, com série temporal, e projetar gráfico e medidor de faixa |
| **Entradas** | `GET api.coingecko.com/api/v3/coins/markets` (BRL, 100 moedas, `sparkline=true`, variações de 1 h/24 h/7 d) |
| **Saídas** | `Snapshot` lido pelos handlers; caminhos SVG já projetados no `viewBox` |
| **Dependências** | `reqwest` (timeout de 15 s, `User-Agent` obrigatório) |
| **Persistência** | **Nenhuma** — vive só em memória, perdido no restart, por decisão |
| **Falhas possíveis** | 403 sem `User-Agent`; fonte indisponível; campo `null` em tipo numérico; `roi` polimórfico; preço `NaN`/`inf`; moeda sem preço; série ausente ou com < 2 pontos |
| **Tratamento de erro** | Moeda **sem preço** ou com preço não finito é **descartada** (preço é o único campo indispensável); qualquer outro campo ausente vira **zero neutro** e a moeda permanece na lista — "uma linha útil pelo preço é melhor que uma linha a menos". Antes da primeira rodada, o snapshot vazio faz a tela mostrar `role="status"` em vez de quebrar. Falha de rodada é logada e a próxima tenta de novo |
| **Evidência** | `src/market.rs` · `Market`, `Coin`, `MarketRow::into_coin`, `parse_markets`, `spawn_scheduled_refresh`, `trading_range_x`, `decimal_from_f64`, `MARKETS_URL`, `USER_AGENT` |

Concorrência: `RwLock<Snapshot>`, não `Mutex` — toda requisição HTTP **lê**, só o
job **escreve** (uma vez por minuto). `RwLock` permite leituras concorrentes sem
fila, o encaixe certo quando leitura domina.

**Este feed não move dinheiro.** A CoinGecko devolve número JSON, que o
`serde_json` decodifica como `f64` — precisão suficiente para exibir, insuficiente
para contabilizar. Escalas são travadas na fronteira: `MONEY_SCALE` (8) para
preços, `CHANGE_SCALE` (2) para variações, `AGGREGATE_SCALE` (2) para agregados.

Detalhe de integração descoberto na prática: **a CoinGecko responde 403 a
requisição sem `User-Agent`**, e o `reqwest` não manda nenhum por padrão.

### 2.18 `i18n` — catálogo tipado de idiomas

| Campo | Descrição |
| --- | --- |
| **Nome** | `Locale`, `Strings`, `lang_cookie` |
| **Responsabilidade** | Resolver o idioma da requisição e fornecer o catálogo de textos |
| **Entradas** | Cookie `lang`; header `Accept-Language` |
| **Saídas** | `Locale` (`PtBr`/`En`) e `&'static Strings` |
| **Dependências** | Nenhuma externa |
| **Persistência** | Cookie `lang` |
| **Falhas possíveis** | Tag desconhecida; header malformado; texto faltando num idioma |
| **Tratamento de erro** | O extrator **nunca falha** — na pior hipótese cai em pt-BR. Texto faltando num idioma é **erro de compilação**, não uma chave ausente descoberta em produção |
| **Evidência** | `src/i18n.rs` · `Locale::from_tag`, `resolve`, `preferred_from_accept_language`, `Strings`, `PT_BR`, `EN`; 4 testes |

Ordem de resolução: cookie explícito → `Accept-Language` → pt-BR. Moeda, datas e
CSV seguem a convenção do **dado** (BRL, pt-BR), não da interface — a tela em
inglês mostra `R$ 10,00`, porque o valor é brasileiro independentemente do idioma
de quem olha.

### 2.19 `models` e `error` — tipos transversais

| Campo | Descrição |
| --- | --- |
| **Nome** | `models` (`Asset`, `UserRecord`, `UserIdentity`, `WalletSummary`, `Holding`, `Transaction`, `PortfolioSnapshot`, `MONEY_SCALE`, `ROLE_ADMIN`) e `error` (`AppError`) |
| **Responsabilidade** | Os tipos que atravessam camadas e o erro único do sistema |
| **Entradas / Saídas** | Não aplicável — são tipos, não componentes ativos |
| **Dependências** | `rust_decimal`, `serde`, `time`, `utoipa`, `thiserror`, `sqlx` |
| **Persistência** | Nenhuma própria; espelham as tabelas |
| **Falhas possíveis** | `percent_of` com base zero |
| **Tratamento de erro** | `percent_of` devolve `None` em base zero em vez de estourar a divisão — e é exatamente o estado da carteira recém-criada, onde a interface mostra o valor absoluto e **omite** o percentual em vez de exibir "0%" ou "∞" |
| **Evidência** | `src/models.rs`, `src/error.rs` |

`AppError` tem **21 variantes**, cada uma mapeada para um status HTTP específico.
A peça central é a censura de 5xx: erro de servidor é logado **inteiro**, com a
causa raiz, e o cliente recebe só `"internal server error"`. Ver
[../api/errors.md](../api/errors.md) para a tabela completa.

---

## 3. Pontos de extensão

Lugares onde o sistema foi construído para receber acréscimo sem reescrita:

| Ponto | Como estender | Custo |
| --- | --- | --- |
| Novo idioma | Adicionar uma `const Strings` e um braço em `Locale` | Baixo — o compilador aponta cada texto faltante |
| Novo par de cotação | Uma linha em `MARKET_PAIRS` com código e aliases | Baixo — a chamada externa já traz todas as taxas |
| Nova versão de API | `.nest("/api/v2", ...)` em `App::router`; o v1 permanece | Baixo |
| Novo dublê de repositório | Implementar `PortfolioRepository` | Baixo — foi para isso que o trait existe |
| Nova rota protegida | Declarar `User` ou `Admin` na assinatura do handler | Baixo — a proteção fica visível na assinatura |
| Novo backend de observabilidade | Apontar `OTEL_EXPORTER_OTLP_ENDPOINT` | Nenhum código |
| Nova tabela | Migração `up`/`down` + método no `Repository` + `cargo sqlx prepare` | Médio |
| Novo tipo de transação | Migração alterando o `CHECK` de `kind`, mais tradução em `i18n` | Médio — o `CHECK` é intencionalmente restritivo |

## 4. Limitações arquiteturais

Consequências estruturais das decisões, não defeitos a corrigir pontualmente:

1. **Instância única presumida.** `LoginThrottle` e `QuoteSync` guardam estado em
   memória do processo. Com réplicas, o lockout passa a ser por instância e a
   serialização da sincronização deixa de valer globalmente.
2. **Snapshot de mercado não sobrevive ao restart.** Deliberado, mas significa
   que a tela de mercado fica em estado de carregamento por até
   `MARKET_SYNC_SECONDS` após cada deploy.
3. **`role` nas claims do JWT.** Revogação de privilégio não é instantânea:
   depende da expiração do access token mais 30 s de tolerância; revogar o
   refresh não invalida o JWT já emitido.
4. **`Decimal` tem 28 dígitos significativos**, enquanto `NUMERIC` é ilimitado. O
   invariante `MONEY_SCALE` e os `ROUND` nas leituras são o que mantém as duas
   representações compatíveis — e precisam ser respeitados por **qualquer** query
   nova que some ou multiplique dinheiro.
5. **Sem cache de leitura.** Cada carregamento da carteira executa 6 consultas
   (concorrentes). É adequado à escala atual e não foi medido sob carga.

## 5. Evidências

```text
- src/lib.rs, src/app.rs, src/config.rs, src/models.rs, src/error.rs
- src/auth/{user,session,admin,csrf,throttle}.rs
- src/routes/{api,frontend,flash}.rs
- src/services/portfolio.rs
- src/repository.rs
- src/quotes.rs, src/market.rs, src/i18n.rs
```
