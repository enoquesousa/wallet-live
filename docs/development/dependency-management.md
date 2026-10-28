# Gestão de dependências

## Objetivo

Inventariar as dependências diretas relevantes, justificar cada uma, registrar a
política de atualização e o tratamento de vulnerabilidades conhecidas.

## Escopo

Coberto: as 30 dependências diretas, a árvore transitiva em números, auditoria e
política de atualização. Não coberto: a justificativa arquitetural de cada tecnologia
(ver [../architecture/technology-decisions.md](../architecture/technology-decisions.md))
e licenciamento do projeto (ver [../decisions/licensing.md](../decisions/licensing.md)).

---

## 1. Números

| Métrica | Valor |
| --- | ---: |
| Dependências diretas em `[dependencies]` | 26 |
| Dependências diretas em `[dev-dependencies]` | 4 |
| **Total de dependências diretas** | **30** |
| **Pacotes registrados no `Cargo.lock`** | **390** |
| Dependências JavaScript no build | **0** |
| Dependências de CDN em runtime | **0** |

> O inventário foi criado em 2026-07-31. As contagens, a entrada de `jwt-simple` e
> a política de auditoria foram revisadas em 2026-10-24. Recontar após alterações
> em `Cargo.toml` ou `Cargo.lock`; o lockfile também registra dependências opcionais
> que não participam da árvore ativa.

> **A ausência de cadeia npm é uma propriedade deliberada** — não um detalhe. O CSS é
> compilado por um executável standalone e o htmx é vendorado, então o build não herda
> a superfície de ataque do ecossistema JavaScript.

## 2. Dependências diretas de runtime

| Dependência | Versão | Finalidade | Justificativa | Removível? | Risco |
| --- | --- | --- | --- | --- | --- |
| `axum` | 0.8.9 | HTTP, roteamento, extratores | [ADR-0002](../adr/0002-axum-em-vez-de-rocket.md) | Não — é o framework | Quebra em versão menor |
| `axum-extra` | 0.10.1 | Cookies (`CookieJar`) | Cookies não estão no axum base | Não | Acompanha o axum |
| `tokio` | 1.52.3 | Runtime async, jobs, sincronização, sinais | Exigido por axum e sqlx | Não | Baixo — é o padrão de fato |
| `sqlx` | 0.9.0 | SQL verificado, migrações, testes | [ADR-0006](../adr/0006-sqlx-com-checagem-em-compilacao.md) | Não | Cache offline pode descolar |
| `askama` | 0.16.0 | Templates verificados em compilação | [ADR-0003](../adr/0003-ssr-com-askama-e-htmx.md) | Não | Baixo |
| `rust_decimal` | 1.36 | **Dinheiro exato** | [ADR-0004](../adr/0004-decimal-e-numeric-para-dinheiro.md) | **Não** — é o núcleo financeiro | Teto de 28 dígitos significativos |
| `serde` | 1.0.228 | Serialização | Onipresente no ecossistema | Não | Muito baixo |
| `serde_json` | 1.0.145 | JSON | API e payloads externos | Não | Muito baixo |
| `time` | 0.3.47 | Datas e durações | Integra com sqlx e cookies | Não | Baixo |
| `tracing` | 0.1.44 | Logs estruturados e spans | Base da observabilidade | Não | Muito baixo |
| `tracing-subscriber` | 0.3.23 | Formatação e filtro de log | `EnvFilter`, saída JSON | Não | Baixo |
| `thiserror` | 2.0.18 | Derivação de erro | `AppError` com 21 variantes | Sim, com custo | Muito baixo |
| `color-eyre` | 0.6.5 | Erro e backtrace no boot | Mensagem legível de falha de configuração | **Sim** — só usado no boot | Muito baixo |
| `reqwest` | 0.12.24 | Cliente HTTP | Duas integrações externas | Não | Traz `native-tls`/OpenSSL |
| `password-auth` | 1.0.0 | Hash argon2 | Negocia algoritmo; hash carrega os parâmetros | Não | Baixo |
| `jwt-simple` | 0.13.2 | JWT HS256 | `pure-rust` dispensa cmake/BoringSSL | Substituível | **Traz `rsa` com advisory** |
| `subtle` | 2.6.1 | Comparação em tempo constante | Segredo de admin e token CSRF | Não | Muito baixo |
| `sha2` | 0.10.8 | SHA-256 | Hash do refresh token | Não | Muito baixo |
| `rand` | 0.8.5 | Aleatoriedade do SO (`OsRng`) | Refresh token, CSRF, `request_id` | Não | Baixo |
| `base64` | 0.22.1 | Codificação | Token opaco e flash com acento | Não | Muito baixo |
| `dotenvy` | 0.15.7 | Carrega `.env` | Conveniência de desenvolvimento | **Sim** | Muito baixo |
| `utoipa` | 5.5.0 | OpenAPI gerada do código | Documentação que não descola | **Sim** | Baixo |
| `opentelemetry` | 0.32.0 | API de telemetria | [ADR-0012](../adr/0012-observabilidade-opt-in-via-otlp.md) | Sim | Versões acopladas |
| `opentelemetry-otlp` | 0.32.0 | Exportador OTLP | Idem | Sim | Idem |
| `opentelemetry_sdk` | 0.32.1 | SDK | Idem | Sim | Idem |
| `tracing-opentelemetry` | 0.33.0 | Ponte `tracing` ↔ OTel | Idem | Sim | Idem |

### Dependências de desenvolvimento

| Dependência | Versão | Finalidade |
| --- | --- | --- |
| `insta` | 1.47.2 | Snapshot testing do contrato JSON |
| `rust_decimal_macros` | 1.36 | Macro `dec!` nos testes |
| `tower` | 0.5.3 | `oneshot` — empurra requisição pelo router completo **sem abrir socket** |
| `http-body-util` | 0.1.3 | Coleta o corpo da resposta nos testes |

> `tower` e `http-body-util` **já estavam na árvore** como dependências transitivas do
> axum. Declará-las explicitamente torna a intenção visível — está comentado no
> `Cargo.toml`.

## 3. Features selecionadas, e por quê

Várias dependências usam `default-features = false` deliberadamente:

| Dependência | Configuração | Motivo |
| --- | --- | --- |
| `jwt-simple` | `default-features = false`, `pure-rust` | **Dispensa BoringSSL e cmake** — atrito de build no Windows |
| `reqwest` | `default-features = false`, `json`, `native-tls` | Reduz a superfície ao necessário |
| `sqlx` | `macros`, `postgres`, `runtime-tokio`, `migrate`, `time`, `rust_decimal` | Só o backend usado; `rust_decimal` é o que integra `NUMERIC` ↔ `Decimal` |
| `tokio` | `rt-multi-thread`, `macros`, `net`, `sync`, `signal` | Sem features não usadas |
| `opentelemetry-otlp` | `http-proto`, `reqwest-blocking-client`, `trace`, `metrics` | Só OTLP/HTTP |
| `utoipa` | `decimal` | Suporte a `Decimal` no schema |
| `tracing-subscriber` | `env-filter`, `json` | `RUST_LOG` e `LOG_FORMAT=json` |

## 4. Auditoria de vulnerabilidades

```bash
cargo audit --ignore RUSTSEC-2026-0235
```

Executado no CI (job `audit`) a cada push em `master` e a cada pull request.
O comando lê `.cargo/audit.toml`: o `--ignore` da CI **se soma** à exceção RSA
nesse arquivo. Portanto, a política efetiva contém os dois IDs abaixo.

### Exceção RSA

**RUSTSEC-2023-0071 — "Marvin Attack" no crate `rsa`.**

| Aspecto | Detalhe |
| --- | --- |
| Caminho | `jwt-simple` → `superboring` → `rsa` |
| Natureza | Canal lateral de temporização em operações RSA |
| Correção upstream | **Não existe** |
| Alcançável aqui? | **Não** — a aplicação assina e valida **exclusivamente com HS256** (HMAC simétrico); o código RSA nunca é exercitado |
| Onde está registrado | `.cargo/audit.toml`, com a justificativa completa |

> **Condição de reavaliação:** havendo correção upstream, ou se qualquer algoritmo RSA
> passar a ser usado. **Não há periodicidade ou próxima data de reavaliação definida** — registrado como débito.

### Exceção de dependência opcional

**RUSTSEC-2026-0235 — `rkyv` 0.7.46.** O pacote permanece no lockfile como
dependência opcional de `rust_decimal`, mas não participa da árvore ativa da
aplicação. A exceção está no comando do job `audit` em `.github/workflows/ci.yml`.
A verificação abaixo não lista dependentes na configuração atual, inclusive com
as features da aplicação habilitadas:

```bash
cargo tree --locked --offline --all-features -i rkyv@0.7.46
```

Reavaliar a exceção se uma mudança de dependência ou feature ativar `rkyv`, ou se
uma atualização permitir removê-lo do lockfile. `cargo audit` sem a flag continua
lendo a exceção RSA do arquivo, mas também reporta o advisory de `rkyv`.

### Atualização criptográfica de 2026-10-24

`Cargo.toml` declara `jwt-simple` 0.13.2 com `pure-rust`; `Cargo.lock` resolve
`crypto-bigint` 0.7.7 e `ctutils` 0.4.5. A árvore ainda contém
`jwt-simple` → `superboring` 0.1.15 → `rsa` 0.9.10: essa atualização não elimina
a exceção RSA. As duas exceções existentes permanecem delimitadas pelos IDs acima.

### Limites da auditoria

| Limite | Consequência |
| --- | --- |
| Só roda em push e PR | Advisory publicado em período sem commits passa despercebido |
| **Não alcança o htmx vendorado** | `static/htmx.js` (2.0.8) é JavaScript; o `cargo audit` só vê crates |
| Não alcança o CLI do Tailwind | Binário baixado no CI, sem verificação de checksum |
| Não valida licenças | Exigiria `cargo-deny` ou `cargo-license` |

## 5. Licenças das dependências

> **Requer validação.** Não foi possível verificar as licenças de forma automatizada
> neste ambiente: as dependências não estão em cache local e `cargo metadata --offline`
> falha. **Nenhuma informação de licença é afirmada aqui sem verificação.**

O que se pode dizer com base no que é verificável:

- O ecossistema Rust é predominantemente **MIT OR Apache-2.0**, e as dependências
  acima são crates amplamente usados desse ecossistema.
- **htmx 2.0.8** (vendorado em `static/htmx.js`) é distribuído sob **0BSD** —
  licença permissiva que não exige atribuição. O arquivo minificado **não** contém
  cabeçalho de licença.
- **Tailwind CSS** é MIT; o CLI é usado apenas em build-time e não é redistribuído.

Para verificar antes de qualquer decisão de licenciamento:

```bash
cargo install cargo-license && cargo license --tsv > licencas.tsv
```

Ou, com verificação de política:

```bash
cargo install cargo-deny && cargo deny check licenses
```

Esta verificação é **pré-requisito** para a decisão de licenciamento do projeto — ver
[../decisions/licensing.md](../decisions/licensing.md).

## 6. Política de atualização

| Tipo | Frequência | Procedimento |
| --- | --- | --- |
| **Correção de segurança** | **Imediata** | Atualizar, rodar a suíte, verificar `cargo audit` |
| Patch (`x.y.Z`) | Quando conveniente | `cargo update` |
| Minor (`x.Y.z`) | Avaliar caso a caso | Ler o changelog; suíte completa |
| Major (`X.y.z`) | Deliberada | Avaliar impacto; pode exigir refactor |

```bash
cargo update
```

```bash
cargo update -p <crate>
```

### Regras

1. **`Cargo.lock` é versionado** — builds reprodutíveis. Não remova.
2. **Após qualquer atualização:** `cargo test`, `cargo clippy -D warnings`,
   `cargo audit`.
3. **Os quatro crates OTel sobem juntos** — versões incompatíveis entre si não
   compilam.
4. **`axum` e `axum-extra` sobem juntos.**
5. **`sqlx`:** após atualizar, `cargo sqlx prepare` e conferir o cache.
6. **`rust_decimal`:** atualização exige atenção especial — é o núcleo financeiro.
   Rodar a suíte de escala inteira.

### Não há automação

Não existe Dependabot, Renovate nem verificação agendada. Atualizações são manuais.
Registrado como débito técnico.

## 7. Impacto de indisponibilidade ou abandono

| Dependência | Se abandonada | Substituto |
| --- | --- | --- |
| `rust_decimal` | **Alto** — é o núcleo financeiro | `bigdecimal` (resolve o teto de 28 dígitos, perde integração direta) |
| `axum` | Alto — refactor de todas as rotas | `actix-web`, `warp` |
| `sqlx` | Alto — todo o acesso a dados | `tokio-postgres`, SeaORM |
| `askama` | Médio — reescrever templates | `maud`, `tera` |
| `jwt-simple` | **Baixo** — API pequena e isolada | `jsonwebtoken` |
| `password-auth` | Baixo | `argon2` direto |
| `utoipa` | Baixo — só a spec | Spec escrita à mão |
| `color-eyre`, `dotenvy` | Muito baixo — removíveis | — |
| Crates OTel | Baixo — observabilidade é opt-in | — |

**`jwt-simple` é o candidato mais provável a substituição**, por causa do advisory
transitivo — e a troca é barata, porque o uso está confinado a duas funções em
`src/auth/user.rs`.

## 8. Dependências não-Rust

| Item | Versão | Onde | Verificado? |
| --- | --- | --- | :---: |
| **htmx** | 2.0.8 | `static/htmx.js`, vendorado | **Não** — fora do `cargo audit` |
| **Tailwind CLI** | 4.3.3 | Baixado no CI; `tools/` local | **Não** — sem checksum |
| PostgreSQL | 18 | Compose e CI | — |
| `debian:bookworm-slim` | — | Imagem de runtime | — |
| `rust:1.95-slim` | — | Imagem de build | — |

> **Nenhuma das duas primeiras tem verificação automatizada.** A versão do htmx só é
> descobrível inspecionando o arquivo minificado (`version:"2.0.8"`), e o CLI do
> Tailwind é baixado do GitHub Releases sem conferência de hash.

## 9. Débitos registrados

| # | Débito | Impacto |
| --- | --- | --- |
| 1 | Versão do htmx não registrada em local verificável | Vulnerabilidade passaria despercebida |
| 2 | Sem checksum do CLI do Tailwind | Binário adulterado no CI |
| 3 | RUSTSEC-2023-0071 sem data de reavaliação | Ignorado indefinidamente |
| 4 | `cargo audit` só em push/PR | Advisory novo demora a aparecer |
| 5 | Sem verificação de licenças | Bloqueia a decisão de licenciamento |
| 6 | Sem automação de atualização | Dependências envelhecem em silêncio |

Consolidados em [../decisions/technical-debt.md](../decisions/technical-debt.md).

## 10. Evidências

```text
- Cargo.toml               (26 em [dependencies] + 4 em [dev-dependencies], com features)
- Cargo.lock               (390 pacotes; inclui dependências opcionais)
- .cargo/audit.toml        (RUSTSEC-2023-0071, com justificativa)
- .github/workflows/ci.yml (job audit com exceção RUSTSEC-2026-0235; Tailwind v4.3.3)
- static/htmx.js           (2.0.8, vendorado)
- Dockerfile               (rust:1.95-slim, debian:bookworm-slim)
```
