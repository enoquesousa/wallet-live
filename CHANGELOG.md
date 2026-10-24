# Changelog

Todas as mudanças relevantes deste projeto são registradas aqui.

O formato segue [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/), e o
projeto adota [Versionamento Semântico](https://semver.org/lang/pt-BR/).

> **Nota sobre este arquivo.** O changelog foi criado **retroativamente**, a partir do
> histórico de commits, dos comentários de migração e do código. As entradas anteriores
> a 2026-07-30 são reconstruções, não registros contemporâneos.
>
> **A versão nunca foi incrementada:** `Cargo.toml` marca `0.1.0` desde o primeiro
> commit, apesar de sucessivas mudanças substanciais, e não há tags no repositório.
> Por isso as mudanças abaixo estão agrupadas pelas **cinco fases de
> desenvolvimento** documentadas em
> [docs/decisions/roadmap.md](docs/decisions/roadmap.md), não por versões publicadas.
> Registrado como **DT-17** em
> [docs/decisions/technical-debt.md](docs/decisions/technical-debt.md).

---

## [Não publicado]

### Adicionado

- Documentação técnica completa em `docs/`, com 12 ADRs, referência de API, dicionário
  de dados, modelo de ameaças, runbooks e matriz de testes por risco.
- `CONTRIBUTING.md`, `SECURITY.md` e este `CHANGELOG.md`.
- Registro consolidado de débitos técnicos (24 itens) e limitações conhecidas.
- Análise de licenciamento e proveniência do código-base, distinguindo as
  contribuições autorais próprias dos componentes derivados do projeto didático da
  DIO.

### Alterado

- Documentação reorganizada em árvore temática. Renomeações preservando o histórico:
  `docs/ARQUITETURA.md` → `docs/architecture/system-overview.md`,
  `docs/TESTES.md` → `docs/testing/test-catalogue.md` (dividido, com a estratégia
  extraída para `test-strategy.md`),
  `docs/ROADMAP.md` → `docs/decisions/roadmap.md`,
  `docs/ENTREGA.md` → `docs/delivery/course-delivery.md`.
- Referências por número de linha substituídas por referências a arquivo e símbolo.

### Corrigido

- Renovação automática ignora `/login`, `/register` e `/logout`: o middleware
  não consome o refresh antes do logout nem sobrescreve os cookies de outra conta
  depois de login/cadastro. Cinco contratos HTTP com Postgres real cobrem esses
  caminhos e o limite de revogação do JWT stateless.
- Documentação de revogação distingue refresh e access: logout impede novas
  renovações quando a escrita no banco tem sucesso; JWTs já emitidos mantêm sua
  validade até a expiração com a tolerância de relógio.
- Corrigida a análise de licenciamento para registrar a origem derivada do projeto, a
  cláusula 10.1 do edital Santander Bootcamp 2026 e a necessidade de resolver a
  autorização do código-base antes da adoção de uma licença *open source*.
- Contagem de variantes de `AppError` na documentação de arquitetura: eram descritas
  17, o enum tem 21.

### Segurança

- Tolerância de relógio na validação do JWT definida em 30 segundos, em vez de
  herdar os 15 minutos de `jwt-simple` 0.12.16. Com TTL nominal de 10 minutos,
  uma cópia do access pode ser aceita até 10 minutos e 30 segundos desde a emissão.
- Documentados três riscos de prioridade alta ainda **não corrigidos no código**:
  `COOKIE_SECURE` com comparação literal que falha em silêncio (**DT-04**), segredos
  validados só por presença e não por qualidade (**DT-07**) e `DATABASE_URL` completa
  registrada em log de erro de conexão (**DT-23**).

---

## Fase 5 — Produto e interface — 2026-07-17 a 2026-07-29

### Adicionado

- **Painel de mercado** com as 100 maiores criptomoedas em BRL: cotação, variações de
  1 h/24 h/7 d, capitalização, volume, faixa do dia, máxima histórica e oferta, com
  gráfico temporal em 24 h ou 7 d. Fonte CoinGecko, servida de snapshot **em memória** —
  trocar de moeda ou de janela não custa chamada externa.
- **Alvo de biblioteca** (`src/lib.rs`), que habilitou a suíte de integração: 35 testes
  de contrato que atravessam o router de produção.
- **Interface bilíngue** (pt-BR/en) a partir de catálogo tipado — texto faltando num
  idioma é erro de compilação. Resolução: cookie > `Accept-Language` > pt-BR.
- **Operações sem recarregar a página** via fragmentos htmx, mantendo SSR e o fluxo
  clássico de redirect sem JavaScript.
- **Gráfico de evolução do patrimônio**, desenhado como SVG no servidor.
- **Exportação do extrato em CSV**, na convenção pt-BR de planilha.
- **Mensagens de feedback** (*flash messages*) acessíveis, de uso único.
- **Sincronização agendada de cotações**, com rodada no boot e intervalo configurável.
- Cobertura de mais pares de mercado (USD, EUR, BTC, ETH, SOL) numa única chamada.
- Navegação por teclado com foco gerenciado e *skip link*.
- Criação automática do catálogo mínimo na primeira sincronização bem-sucedida.

### Alterado

- **Interface reconstruída** sobre uma paleta validada por script (banda de
  luminosidade, croma, separação sob daltonismo, contraste) contra a superfície real.
- **Toda variação passou a sair com seta ▲/▼ e sinal**, não apenas cor: verde e
  vermelho medem ΔE ~4,6 sob deuteranopia, indistinguíveis para cerca de 8% dos homens.
- Tela de mercado deixou de ser uma tabela de 100 linhas e virou painel com moeda em
  foco e lista lateral com busca.

### Corrigido

- **Escala monetária limitada a 8 casas de ponta a ponta.** A sincronização gravava
  `preço = 1/taxa` sem arredondar, e a divisão de `Decimal` preenchia a mantissa
  inteira. Produtos e somas desses valores estouravam os 28 dígitos significativos do
  `Decimal`, e a **leitura** falhava com `value not representable` — `/assets` respondia
  **500 para qualquer conta com posições**. Correção em três camadas (arredondamento na
  escrita, `ROUND` nos agregados, migração de saneamento) e teste de regressão nomeado
  pelo incidente.
- Ativos sem cotação não podem mais ser negociados por zero.
- Operações cujo total arredonde a zero passaram a ser recusadas.

---

## Fase 4 — Operação — 2026-07-17 a 2026-07-21

### Adicionado

- **CI (GitHub Actions)** com quatro frentes independentes: `lint` (fmt, clippy com
  warnings como erro, frescor do CSS compilado), `test` (Postgres real em service
  container, `sqlx prepare --check`), `audit` (RustSec) e `docker` (build da imagem).
- **Dockerfile multi-stage**: binário único com templates e migrações embutidos,
  usuário sem privilégio (`uid 10001`), build offline via cache `.sqlx`.
- **Migrações aplicadas no boot** — o deploy dispensa passo manual.
- **Sondas separadas**: liveness (`/healthz`, sem tocar o banco) e readiness
  (`/readyz`, exige o banco).
- **Logs em JSON** (`LOG_FORMAT=json`) e `request_id` por requisição, propagado do
  header `x-request-id` quando bem-formado e devolvido na resposta.
- **Exportação OpenTelemetry** de traces e do histograma
  `http.server.request.duration`, via OTLP/HTTP, **opt-in** por variável de ambiente —
  ausente, zero overhead e zero tentativa de conexão.
- Coletor OTLP local (perfil `observability`) para verificar a exportação.
- Suporte a CA extra no build, para ambientes com inspeção TLS.
- Desligamento gracioso participando de `SIGTERM`.

### Segurança

- Advisory **RUSTSEC-2023-0071** (`rsa`, transitivo via `jwt-simple`) documentado e
  ignorado com justificativa verificável: a aplicação usa exclusivamente HS256, e o
  código RSA nunca é exercitado.

---

## Fase 3 — Arquitetura e API — 2026-07-17

### Adicionado

- **Versionamento de API**: caminho canônico `/api/v1`, com `/api` mantido como alias
  de compatibilidade servindo o **mesmo** router — divergência verificada byte a byte
  por teste.
- **Especificação OpenAPI gerada do código** (utoipa), servida em
  `/api/v1/openapi.json`.
- **Camada de serviço** (`PortfolioService`), genérica sobre o trait
  `PortfolioRepository` — o que permite testar a orquestração da carteira sem Postgres.
- **Paginação do extrato** com ordem estável.

### Alterado

- Handlers reduzidos a HTTP: o repositório passou a concentrar todo o SQL, e a camada
  de serviço, a orquestração.

### Decisões registradas

- **UUIDv7 avaliado e descartado**; `BIGSERIAL` mantido. Os ids só aparecem em
  superfícies autenticadas e toda leitura é filtrada pelo usuário da sessão, então
  enumeração não expõe dados de terceiros. Reavaliar se a API se tornar pública ou
  multi-tenant.

---

## Fase 2 — Segurança de sessão — 2026-07-17

### Adicionado

- **Refresh token opaco com rotação a cada uso e revogação no servidor.** Apenas a
  hash SHA-256 é gravada: um vazamento do banco não vaza tokens utilizáveis. A rotação
  usa `UPDATE ... RETURNING` atômico — não há janela de corrida em que dois tokens
  rotacionem.
- **Proteção CSRF** nos formulários (*double-submit cookie*, comparação em tempo
  constante).
- **Lockout progressivo de login** por usuário, com backoff exponencial de 30 s até o
  teto de 15 min, verificado **antes** da senha.
- **Papéis de usuário** (`user`/`admin`), permitindo que a autorização administrativa
  derive de uma sessão em vez de depender só do segredo compartilhado.
- **Cabeçalhos de segurança** em toda resposta: CSP, `nosniff`, `X-Frame-Options`,
  `Referrer-Policy` e HSTS condicional.
- TTLs de sessão configuráveis por ambiente.
- Logout com revogação real da sessão no servidor.

### Alterado

- **Tailwind saiu do Play CDN** para CSS pré-compilado em build-time pelo CLI
  standalone — sem Node e sem npm. O CDN era um compilador de 407 KB rodando no
  navegador que injetava `<style>` em runtime, o que obrigava a CSP a manter
  `style-src 'unsafe-inline'`.
- **CSP fechada em `'self'`**, sem `'unsafe-inline'`, com teste travando o invariante
  de que nenhuma página emite `<style>` ou `<script>` inline.
- htmx e CSS passaram a ser servidos do próprio binário — zero requisições a terceiros.

---

## Fase 1 — Fundações e integridade de dados — 2026-06-01 a 2026-06-15

### Adicionado

- Aplicação Axum + Tokio com PostgreSQL e migrações versionadas.
- Cadastro e login com hash argon2; a senha nunca é armazenada em texto.
- Sessão stateless com JWT assinado em cookie `HttpOnly` + `SameSite=Strict`.
- **Livro-razão transacional**: posições materializadas (`holdings`) e extrato imutável
  (`transactions`), com depósito, compra e venda atômicos (`FOR UPDATE`, reversão
  íntegra) e custo médio ponderado.
- **Guard-rails no schema**: `CHECK` de preço, quantidade e saldo não negativos, e
  tipos de transação restritos — o banco como última linha de defesa.
- Configuração *fail-fast*: segredos lidos e validados uma única vez no boot.
- Erro central (`AppError`) com status HTTP específicos e **censura de 5xx** — detalhe
  só no log do servidor.
- Comparação em tempo constante da credencial administrativa.
- Testes do núcleo financeiro em bancos efêmeros isolados (`#[sqlx::test]`) e snapshots
  do contrato JSON (`insta`).

### Alterado

- **Valores monetários migrados de `DOUBLE PRECISION` para `NUMERIC`**, mapeados a
  `rust_decimal::Decimal`. Ponto flutuante carrega ruído de arredondamento
  (0,1 + 0,2 ≠ 0,3) inaceitável para valor financeiro.
- **`owned_assets` reformulado em `holdings` + `transactions`.** O modelo append-only
  do material didático funciona para um produto que só compra; uma carteira real também
  vende, e a venda quebra o cálculo de custo médio nesse modelo. A migração preserva os
  dados existentes.

### Removido

- Tabela `owned_assets`, substituída pelo par acima.

---

## Convenções deste changelog

| Seção | Conteúdo |
| --- | --- |
| `Adicionado` | Funcionalidade nova |
| `Alterado` | Mudança em comportamento existente |
| `Corrigido` | Correção de defeito |
| `Removido` | Funcionalidade retirada |
| `Descontinuado` | Marcado para remoção futura |
| `Segurança` | Correção ou mudança com implicação de segurança |

Este arquivo **não é uma cópia do histórico de commits**. Commits `docs:`, `ci:` e
`refactor:` que não alteram o comportamento observável não aparecem aqui.

Processo de release em
[docs/development/commit-conventions.md](docs/development/commit-conventions.md).
