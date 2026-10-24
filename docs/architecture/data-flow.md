# Fluxos e diagramas

## Objetivo

Representar graficamente os fluxos que atravessam mais de um componente, cada um
acompanhado da explicação textual correspondente. Nenhum diagrama aqui é a única
fonte da informação: o texto ao lado sustenta o que o desenho resume.

## Escopo

Coberto: contexto, componentes, sequência de uma operação financeira,
autenticação e renovação de sessão, ciclo de vida dos dados, tratamento de erro e
implantação. Não coberto: contratos de campo (ver [../api/](../api/)) e a ficha
individual de cada componente (ver
[component-architecture.md](component-architecture.md)).

---

![Diagrama animado da arquitetura: uma requisição entra pelo navegador, atravessa os
middlewares request_tracing, security_headers e refresh_session, chega às rotas, passa
por services/portfolio e repository e alcança as seis tabelas do PostgreSQL; em
paralelo, os jobs quotes e market consomem Coinbase e CoinGecko](../assets/arquitetura-fluxo.gif)

**Leitura.** O laço percorre em 7,7 s o que as seções seguintes detalham paradas: as
três camadas na ordem em que são aplicadas e os caminhos que contornam a camada de
serviço (§2), a compra até o `COMMIT` e as seis consultas concorrentes que montam a
`WalletView` (§3), e as duas integrações externas com destinos diferentes (§1) — âmbar
chega ao banco, violeta termina em memória. Como todo desenho deste documento, ele
**resume**: o texto de cada seção é o que sustenta a afirmação.

---

## 1. Diagrama de contexto

Quem interage com o sistema e por qual fronteira.

```mermaid
graph TB
    subgraph externos["Fora da fronteira de confiança"]
        U["Usuário final<br/>(navegador)"]
        OP["Integração máquina-a-máquina<br/>(credencial de serviço)"]
        CB["API Coinbase<br/>exchange-rates"]
        CG["API CoinGecko<br/>coins/markets"]
        OT["Backend OTLP<br/>(opcional)"]
    end

    subgraph confianca["Fronteira de confiança"]
        W["wallet<br/>binário único Rust"]
        DB[("PostgreSQL 18")]
    end

    U -->|"HTTPS · HTML/form · cookies"| W
    OP -->|"HTTPS · JSON · Authorization"| W
    W -->|"HTTPS saída · lastreia preço"| CB
    W -->|"HTTPS saída · informativo"| CG
    W -->|"OTLP/HTTP · só se configurado"| OT
    W <-->|"TCP · SQL"| DB
```

**Leitura.** O sistema tem **dois** consumidores de entrada e **três**
dependências de saída. A assimetria importa para segurança: as entradas são
superfícies de ataque (validação, autenticação, CSRF, lockout); as saídas são
riscos de disponibilidade e de integridade de dado de terceiro.

As duas integrações externas têm **naturezas diferentes**, e confundi-las seria o
erro mais custoso do sistema:

| | Coinbase | CoinGecko |
| --- | --- | --- |
| Papel | **Lastreia dinheiro**: define `assets.unit_value`, que é o preço de compra e venda | **Informativo**: alimenta só a tela de mercado |
| Formato do número | String com precisão arbitrária → `Decimal` sem passar por float | Número JSON → `f64` → `Decimal` com escala travada |
| Persistido? | Sim, em `assets` | Não, só em memória |
| Se ficar indisponível | Preços congelam no último valor válido; operações continuam funcionando | Tela de mercado mostra estado de carregamento; nada mais é afetado |

O backend OTLP é a única dependência **verdadeiramente opcional**: sem
`OTEL_EXPORTER_OTLP_ENDPOINT`, não há tentativa de conexão.

## 2. Diagrama de componentes

```mermaid
graph TB
    REQ(["Requisição HTTP"])

    subgraph camadas["Pilha de camadas (de fora para dentro)"]
        RT["request_tracing<br/><i>span, request_id, histograma</i>"]
        SH["security_headers<br/><i>CSP, HSTS, no-store</i>"]
        RS["refresh_session<br/><i>rotaciona se preciso</i>"]
    end

    subgraph rotas["routes/"]
        FE["frontend<br/>16 rotas SSR"]
        API["api<br/>/api/v1 + alias /api"]
        PROBE["healthz · readyz · health"]
    end

    SVC["services::portfolio<br/><i>PortfolioService</i>"]
    REPO["repository<br/><i>todo o SQL</i>"]
    DB[("PostgreSQL")]

    subgraph transversal["Transversais"]
        AUTH["auth/*<br/>user · session · admin<br/>csrf · throttle"]
        I18N["i18n"]
        ERR["error::AppError"]
    end

    subgraph jobs["Jobs de segundo plano"]
        QJ["quotes<br/><i>Mutex</i>"]
        MJ["market<br/><i>RwLock</i>"]
    end

    REQ --> RT --> SH --> RS
    RS --> FE
    RS --> API
    RS --> PROBE
    FE --> SVC
    FE -.-> REPO
    API --> REPO
    SVC --> REPO
    REPO --> DB
    PROBE -.->|"SELECT 1"| DB
    FE --- AUTH
    API --- AUTH
    FE --- I18N
    QJ --> REPO
    FE -.->|"lê snapshot"| MJ
    QJ -.->|"lastreia preço"| DB
```

**Leitura.** As três camadas são aplicadas de fora para dentro, e a ordem é
funcional, não estética:

- `request_tracing` é a **mais externa** de propósito: assim até os logs dos
  middlewares internos saem correlacionados ao mesmo `request_id`, e a métrica de
  duração cobre a requisição inteira, cabeçalhos de segurança inclusos.
- `security_headers` vem antes do roteamento porque se aplica a **toda** resposta,
  inclusive erros e 404 — não é algo que cada handler precise lembrar de fazer.
- `refresh_session` roda antes do extrator `User` para que ele receba a sessão
  renovada. Ignora `/login`, `/register` e `/logout`, que controlam seus cookies.

As setas tracejadas marcam acessos que **contornam** a camada de serviço: o
`frontend` fala direto com o `Repository` nos casos que não envolvem a carteira
(login, logout, CSV, sincronização manual), e a sonda de readiness executa
`SELECT 1` sem passar por nada. Ambos são deliberados — não há visão de portfólio
a montar nesses caminhos.

Evidência: `src/app.rs` · `App::router`.

## 3. Sequência: compra de um ativo

O caminho mais completo do sistema — passa por CSRF, sessão, serviço, transação de
banco e resposta condicional a htmx.

```mermaid
sequenceDiagram
    autonumber
    participant B as Navegador
    participant T as request_tracing
    participant S as security_headers
    participant R as refresh_session
    participant H as buy_asset (handler)
    participant SV as PortfolioService
    participant RP as Repository
    participant DB as PostgreSQL

    B->>T: POST /buy (asset_id, quantity, csrf_token)
    T->>T: abre span, gera request_id
    T->>S: segue
    S->>R: segue
    R->>R: access token válido? sim → nada a fazer
    R->>H: extratores: SessionUser, PortfolioService, CookieJar, HxRequest
    H->>H: verify_csrf(jar, form.csrf_token)
    Note over H: divergente → 403 e nada é escrito
    H->>SV: buy_asset(user_id, asset_id, quantity)
    SV->>RP: buy_asset(...)
    RP->>DB: BEGIN
    RP->>DB: SELECT balance FROM users FOR UPDATE
    RP->>DB: SELECT unit_value FROM assets
    RP->>RP: total = ROUND(preço × quantidade, 8)
    alt saldo insuficiente, ativo sem cotação ou total arredonda a zero
        RP->>DB: ROLLBACK
        RP-->>SV: AppError (negócio)
        SV-->>H: repassa sem reinterpretar
        H->>H: flash de erro + reabre o formulário
        H-->>B: 303 (ou fragmento com banner, se htmx)
    else operação válida
        RP->>DB: UPDATE users SET balance = balance - total
        RP->>DB: INSERT/UPDATE holdings (custo médio ponderado)
        RP->>DB: INSERT INTO transactions (kind='buy', cash_delta<0)
        RP->>DB: COMMIT
        RP-->>SV: Ok(())
        SV-->>H: Ok(())
        H->>SV: wallet_view(user_id, page) — 6 consultas concorrentes
        SV-->>H: WalletView
        H-->>B: fragmento + flash inline + HX-Push-Url<br/>(ou 303 clássico sem htmx)
    end
    S->>S: aplica CSP, no-store
    T->>T: loga status e latência, devolve x-request-id
```

**Leitura.** Três propriedades que o diagrama torna visíveis:

1. **A verificação de CSRF acontece antes de qualquer escrita.** Um token
   divergente resulta em 403 com o saldo intacto — e o teste
   `forms_without_a_matching_csrf_token_are_refused` confere justamente que o
   saldo continua zerado, não apenas que o status é 403. Conferir só o status
   deixaria passar um refactor que redireciona bonito e credita de qualquer forma.

2. **A recusa é atômica.** Saldo insuficiente causa `ROLLBACK`, não uma escrita
   parcial. O `FOR UPDATE` no `SELECT` do saldo serializa compras concorrentes do
   mesmo usuário: sem ele, duas compras simultâneas poderiam ambas ler o saldo
   antigo e ambas passar a validação.

3. **A resposta é única, não um redirect seguido de GET.** Em requisição htmx, a
   mesma resposta traz o fragmento atualizado, o banner inline e o `HX-Push-Url` —
   uma requisição em vez de duas. Sem htmx (sem JavaScript, ou restauração de
   histórico), vale o PRG clássico.

Evidência: `src/routes/frontend.rs` · `buy_asset`, `render_wallet`;
`src/repository.rs` · `buy_asset`; `src/services/portfolio.rs` · `wallet_view`.

## 4. Fluxo de autenticação e renovação de sessão

```mermaid
sequenceDiagram
    autonumber
    participant B as Navegador
    participant M as refresh_session
    participant X as Extrator User
    participant RP as Repository
    participant DB as sessions

    Note over B: Caso 1 — access token ainda válido
    B->>M: requisição com cookie `token` válido
    M->>M: assinatura confere → não toca o banco
    M->>X: segue
    X->>X: reconstrói User das claims (stateless)

    Note over B: Caso 2 — access expirado, refresh válido
    B->>M: cookie `token` expirado + `refresh_token`
    M->>RP: rotate_session(hash_antigo, hash_novo, expiry)
    RP->>DB: BEGIN
    RP->>DB: UPDATE sessions SET revoked_at = NOW()<br/>WHERE token_hash = $1 AND revoked_at IS NULL<br/>AND expires_at > NOW() RETURNING user_id
    alt reivindicação bem-sucedida
        RP->>DB: INSERT nova sessão (hash_novo)
        RP->>DB: COMMIT
        RP-->>M: Some(UserIdentity)
        M->>M: insere User nas extensions
        M->>X: segue
        X->>X: lê User das extensions (sem tocar cookie)
        M-->>B: Set-Cookie: token + refresh_token renovados
    else sessão inexistente, revogada ou expirada
        RP->>DB: ROLLBACK
        RP-->>M: None
        M->>X: segue sem renovar
        X-->>B: 303 /login (ou HX-Redirect se htmx)
    end

    Note over B: Caso 3 — logout
    B->>RP: GET /logout
    RP->>DB: UPDATE sessions SET revoked_at = NOW()
    RP-->>B: remove cookies + redireciona
```

**Leitura.** O `UPDATE ... RETURNING` é a peça central e merece ser lido com
atenção: ele **reivindica** a sessão numa operação atômica. Se um token roubado e
o legítimo tentarem rotacionar ao mesmo tempo, o segundo a chegar encontra a
sessão já revogada (`revoked_at IS NULL` não bate mais) e recebe `None`. **Não há
janela de corrida em que os dois consigam rotacionar.**

Divisão de responsabilidade entre os dois tokens:

| | Access token | Refresh token |
| --- | --- | --- |
| Formato | JWT HS256 com claims (`id`, `username`, `role`) | 32 bytes aleatórios, **opaco** |
| Validade | `SESSION_TTL_MINUTES` (10 min) | `REFRESH_TTL_DAYS` (14 dias) |
| Validação | Assinatura e claims temporais, com tolerância de 30 s — **não toca o banco** | Consulta `sessions` por hash |
| No banco | Nada | Só a **hash SHA-256** |
| Revogável | Não, até expirar | **Sim**, de verdade |

O valor em claro do refresh token nunca toca o banco: um vazamento do banco não
vaza token utilizável.

Logout não executa rotação antes de revogar o refresh recebido. A remoção dos
cookies continua mesmo se a revogação no banco falhar. Um JWT já emitido não
consulta `sessions` e pode ser aceito até `exp + 30 s`, inclusive após o logout.

Evidência: `src/auth/session.rs` · `refresh_session`, `RefreshToken`;
`src/repository.rs` · `rotate_session`, `revoke_session`;
`migrations/20260716000001_create_sessions.up.sql`.

## 5. Ciclo de vida dos dados

```mermaid
graph LR
    subgraph entrada["Origem"]
        F["Formulário<br/>do usuário"]
        A["API admin<br/>JSON"]
        CB["Coinbase<br/>string"]
        CG["CoinGecko<br/>f64"]
    end

    subgraph validacao["Validação na borda"]
        V1["validated_asset_name<br/>validated_unit_value"]
        V2["round_dp(MONEY_SCALE)"]
        V3["decimal_from_f64<br/>escala travada"]
    end

    subgraph persistido["Persistido (NUMERIC)"]
        AS[("assets")]
        US[("users.balance")]
        HO[("holdings")]
        TR[("transactions<br/>imutável")]
        SN[("portfolio_snapshots")]
        SE[("sessions")]
    end

    MEM["Snapshot em memória<br/><i>não persistido</i>"]

    subgraph leitura["Leitura"]
        RD["ROUND(..., 8)<br/>em todo agregado"]
        UI["Tela · CSV · JSON"]
    end

    F --> V2 --> US
    F --> V2 --> HO
    F --> V2 --> TR
    A --> V1 --> AS
    CB --> V2 --> AS
    CG --> V3 --> MEM
    AS --> RD
    US --> RD
    HO --> RD
    TR --> RD
    SN --> RD
    RD --> UI
    MEM --> UI
    AS -.->|"preço do momento"| SN
```

**Leitura.** Há **duas** barreiras de escala, e ambas são necessárias:

- **Na escrita**, todo valor monetário é arredondado para `MONEY_SCALE = 8` antes
  de chegar ao banco.
- **Na leitura**, todo agregado SQL que soma ou multiplica `NUMERIC` é envolvido
  em `ROUND(..., 8)`.

A segunda barreira parece redundante e não é: produtos e somas de `NUMERIC`
acumulam escala **sem limite**, então a leitura falharia com `value not
representable` mesmo com cada coluna individual dentro do invariante. Foi
exatamente esse o incidente de 2026-07-22 — `/assets` respondendo 500 para
qualquer conta com posições.

Repare no caminho da CoinGecko: ele termina em memória e **nunca** cruza para as
tabelas. É a separação que garante que cotação informativa não contamine o
catálogo que lastreia operações.

`transactions` é o único destino **imutável** — só recebe `INSERT`. A migração de
saneamento `normalize_money_scales` deliberadamente **não** tocou nessa tabela:
é histórico, e todos os seus valores foram gravados via `Decimal`, logo já são
representáveis na volta.

Detalhamento por campo em [../data/data-dictionary.md](../data/data-dictionary.md).

## 6. Fluxo de erro e recuperação

```mermaid
graph TB
    E["Erro em qualquer camada"] --> C{"AppError:<br/>4xx ou 5xx?"}

    C -->|"4xx — erro do cliente"| CL["Mensagem real preservada"]
    C -->|"5xx — erro nosso"| SV["tracing::error! com causa raiz"]

    SV --> CENS["Resposta: 'internal server error'<br/><i>nada de SQL, coluna ou conexão</i>"]

    CL --> D{"Origem da requisição?"}
    D -->|"API JSON"| J["{ error: mensagem }"]
    D -->|"Formulário web"| FL{"Erro de negócio?"}

    FL -->|"Sim"| BAN["Flash + reabre formulário<br/>com autofocus"]
    FL -->|"Não (CSRF, sessão)"| RED["403 ou 303 /login"]

    CENS --> J

    subgraph recuperacao["Recuperação automática"]
        R1["Cotação falhou<br/>→ warn, próxima rodada tenta"]
        R2["Mercado falhou<br/>→ mantém snapshot anterior"]
        R3["Access expirado<br/>→ rotação transparente"]
        R4["Banco fora<br/>→ readyz 503, orquestrador tira do LB"]
        R5["Migração falha<br/>→ boot aborta"]
    end
```

**Leitura.** A distinção 4xx/5xx é a decisão mais importante do tratamento de
erro, e ela é tomada num só lugar:

```rust
let error = if status.is_server_error() {
    tracing::error!(error = ?self, "internal error serving request");
    "internal server error".to_string()
} else {
    self.to_string()
};
```

Erros 4xx (senha errada, saldo insuficiente, CSRF divergente) devolvem a mensagem
real — não revelam nada sobre como o sistema funciona por dentro. Erros 5xx são
logados **inteiros**, com a causa raiz encadeada pelo `thiserror`, e o cliente
recebe só a mensagem genérica: nunca o texto de erro do SQL, nome de coluna ou
string de conexão.

As cinco recuperações automáticas têm graus deliberadamente diferentes de
severidade:

| Falha | Comportamento | Por que este e não outro |
| --- | --- | --- |
| Rodada de cotação | `warn` e segue; próxima tenta | Cotação atrasada não justifica recusar operações — os preços anteriores continuam válidos |
| Rodada de mercado | Mantém o snapshot anterior | Dado informativo defasado é melhor que tela quebrada |
| Access token expirado | Rotação transparente | O usuário não deveria perceber |
| Banco indisponível | `/readyz` → 503 | O orquestrador tira do balanceador **sem reiniciar** — reiniciar o app não conserta um Postgres fora do ar |
| Migração falha no boot | Processo aborta | "Melhor não subir do que subir contra um schema pela metade" |
| Exportador OTLP malformado | `eprintln!` e segue sem exportar | Observabilidade é infraestrutura auxiliar; não vale recusar servir requisições financeiras por causa dela |

Procedimentos operacionais correspondentes em
[../operations/runbooks.md](../operations/runbooks.md).

## 7. Fluxo de build e implantação

```mermaid
graph TB
    subgraph dev["Desenvolvimento"]
        SRC["Código-fonte"]
        SQLXC[".sqlx/ versionado<br/>31 queries"]
        CSS["styles/app.css<br/>→ static/app.css"]
    end

    subgraph ci["CI (4 jobs independentes)"]
        L["lint<br/>fmt · clippy -D warnings<br/>frescor do CSS"]
        T["test<br/>Postgres 18 em service container<br/>sqlx prepare --check · cargo test"]
        AU["audit<br/>RustSec"]
        DK["docker<br/>docker build ."]
    end

    subgraph img["Imagem"]
        B1["builder: rust:1.95-slim<br/>SQLX_OFFLINE=true"]
        B2["runtime: debian:bookworm-slim<br/>só o binário · uid 10001"]
    end

    subgraph run["Execução"]
        BOOT["1 · Config::from_env (fail-fast)"]
        MIG["2 · sqlx::migrate! no boot"]
        JOBS["3 · spawn dos 2 jobs"]
        SERVE["4 · serve com graceful shutdown"]
    end

    SRC --> L & T & AU & DK
    SQLXC --> L
    SQLXC --> B1
    CSS --> L
    DK --> B1 --> B2 --> BOOT --> MIG --> JOBS --> SERVE
    SERVE -->|"SIGTERM / Ctrl+C"| DRAIN["drena requisições em voo<br/>flush de spans e métricas"]
```

**Leitura.** Duas propriedades do build merecem destaque:

**O binário nasce sem nunca ter falado com um banco.** O cache `.sqlx/` versionado
(31 arquivos de query) permite que `SQLX_OFFLINE=true` valide as queries em tempo
de compilação sem conexão. O preço é que o cache pode descolar do schema — por
isso o CI roda `cargo sqlx prepare --check`.

**O CSS compilado é versionado pelo mesmo motivo, e sofre do mesmo risco.** O
binário embute `static/app.css` via `include_str!`; se alguém usar uma classe nova
sem recompilar, nada no build de Rust perceberia e o estilo simplesmente faltaria
em produção. O job `lint` recompila e faz `diff` para provar o frescor.

A imagem final leva **só o binário** — nenhuma toolchain, nenhum código-fonte,
nenhuma dependência de build — e roda como usuário sem privilégio (`uid 10001`).
Templates e migrações já vão embutidos no executável (`#[derive(Template)]` do
Askama, `sqlx::migrate!()`).

A ordem do boot é fail-fast em cada etapa: configuração inválida, banco
inacessível ou migração falha abortam **antes** de a porta abrir. O desligamento é
gracioso em SIGTERM (Docker/Kubernetes) e Ctrl+C, e o `Drop` do `OtelGuard` escoa
os spans e métricas ainda em buffer.

Procedimento completo em [../operations/deployment.md](../operations/deployment.md).

## 8. Evidências

```text
- src/app.rs             · App::router, App::start, security_headers, request_tracing
- src/auth/session.rs    · refresh_session
- src/repository.rs      · buy_asset, rotate_session, wallet_summary
- src/services/portfolio.rs · wallet_view (tokio::try_join!)
- src/error.rs           · IntoResponse for AppError
- Dockerfile             · estágios builder e runtime
- .github/workflows/ci.yml · jobs lint, test, audit, docker
- migrations/            · 11 pares up/down
```
