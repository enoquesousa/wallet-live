# Referência de configuração

## Objetivo

Documentar todas as variáveis de ambiente: obrigatoriedade, padrão, valores válidos,
efeito, risco de configuração incorreta, sensibilidade e onde cada uma é lida no
código.

## Escopo

Coberto: as 13 variáveis que o sistema lê. Não coberto: instalação (ver
[installation.md](installation.md)) e gestão de segredos em produção (ver
[../security/secrets-management.md](../security/secrets-management.md)).

---

## 1. Tabela geral

| Variável | Obrigatória | Padrão | Exemplo | Finalidade | Sensível |
| --- | :---: | --- | --- | --- | :---: |
| `DATABASE_URL` | **Sim** | — | `postgres://postgres:postgres@localhost:5432/postgres` | Conexão com o Postgres | **Sim** |
| `ADMIN_SECRET_KEY` | **Sim** | — | *(valor longo e aleatório)* | Credencial da API administrativa | **Sim** |
| `JWT_SECRET` | **Sim** | — | *(valor longo e aleatório)* | Chave de assinatura das sessões | **Sim** |
| `COOKIE_SECURE` | Não | `false` | `true` | Cookies `Secure` + HSTS | Não |
| `BIND_ADDR` | Não | `0.0.0.0:3000` | `127.0.0.1:8080` | Endereço de escuta | Não |
| `SESSION_TTL_MINUTES` | Não | `10` | `15` | Validade do token de acesso | Não |
| `REFRESH_TTL_DAYS` | Não | `14` | `7` | Validade do refresh token | Não |
| `QUOTES_SYNC_MINUTES` | Não | `10` | `0` (desliga) | Intervalo do job de cotações | Não |
| `MARKET_SYNC_SECONDS` | Não | `60` | `0` (desliga) | Intervalo do job de mercado | Não |
| `LOG_FORMAT` | Não | texto | `json` | Formato dos logs | Não |
| `RUST_LOG` | Não | `info` | `wallet=debug,info` | Nível de log | Não |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Não | *(desligado)* | `http://localhost:4318` | Destino de traces e métricas | Não |
| `OTEL_SERVICE_NAME` | Não | `wallet` | `wallet-prod` | Nome no backend de observabilidade | Não |

**Nenhuma exige reinicialização para mudar de valor** — porque **nenhuma é relida em
runtime**. Toda mudança exige reiniciar o processo.

## 2. Validação no boot (*fail-fast*)

As três obrigatórias são validadas em `Config::from_env`, **antes** de o serviço
abrir a porta. Ausente ou **vazia** derruba o boot com mensagem nomeando a variável.

> Valor em branco é rejeitado como ausente: "um segredo em branco é tão perigoso
> quanto um ausente."

O motivo de centralizar isso está registrado no código: antes, um `JWT_SECRET`
ausente só aparecia na primeira requisição, **disfarçado de `401 invalid
credentials`** — um erro de cliente para um problema de configuração nosso.

Ganho secundário: os segredos ficam em memória, num `Arc`. Validar um token ou
conferir a credencial de admin não relê variável de ambiente a cada chamada.

**`LOG_FORMAT` e as variáveis `OTEL_*` não passam pela `Config`.** São lidas
diretamente do ambiente, e a razão é de ordem: o logging precisa existir **antes** de
a configuração ser validada, para que os próprios erros de configuração já saiam no
formato certo.

Consequência: **não são validadas com o mesmo rigor**. Um valor inesperado em
`LOG_FORMAT` cai silenciosamente no formato de texto.

## 3. Detalhamento

### `DATABASE_URL` — **obrigatória**, sensível

Conexão com o PostgreSQL, no formato
`postgres://usuário:senha@host:porta/banco`.

| Aspecto | Detalhe |
| --- | --- |
| Validada | Presença e não-vazio no boot; a conexão é testada logo em seguida |
| Falha | Banco inacessível aborta o boot |
| Risco | **Contém a senha do banco.** Aparece em `ps`, em arquivos `.env` e em variáveis de container |
| Lida em | `src/config.rs` · `Config::from_env` → `AppState::build` |

> Erros 5xx são censurados na resposta HTTP, então uma falha de conexão não vaza a
> string. Mas ela **vai inteira para o log do servidor** — o log é um ativo sensível.

### `ADMIN_SECRET_KEY` — **obrigatória**, sensível

Credencial de serviço da API administrativa. Enviada **crua** no header
`Authorization` — sem `Bearer`, sem prefixo.

| Aspecto | Detalhe |
| --- | --- |
| Formato | Qualquer string não vazia. **Não há requisito de comprimento nem entropia** |
| Comparação | Tempo constante (`subtle::ConstantTimeEq`) |
| Risco de valor fraco | **Alto** — controla o preço que lastreia todas as operações |
| Rotação | **Não implementada** — trocar exige reiniciar o serviço |
| Lida em | `src/config.rs`; usada em `src/auth/admin.rs` |

> O `.env.example` traz `change-me`. **Trocar não é opcional** — o valor de exemplo é
> público neste repositório.

Gerar um valor adequado:

```bash
openssl rand -base64 48
```

### `JWT_SECRET` — **obrigatória**, sensível

Chave HMAC que assina e valida os tokens de sessão (HS256).

| Aspecto | Detalhe |
| --- | --- |
| Formato | Qualquer string não vazia. **Sem requisito de comprimento** |
| Risco de vazamento | **Crítico** — permite forjar qualquer sessão, inclusive de admin |
| Efeito de trocar | Invalida JWTs da chave antiga; refresh válido continua podendo renovar com a chave nova. Encerrar todas as sessões exige também revogar os refresh tokens em `sessions` |
| Rotação | **Não implementada** — não há suporte a duas chaves simultâneas |
| Lida em | `src/config.rs`; usada em `src/auth/user.rs` |

Como HS256 é simétrico, a mesma chave assina e valida: quem a possui pode emitir
tokens ([ADR-0007](../adr/0007-sessao-jwt-curto-com-refresh-rotativo.md)).

### `COOKIE_SECURE` — padrão `false`

Marca `token`, `refresh_token`, `csrf` e `flash` como `Secure`, **e** liga o HSTS.

> ⚠️ **Armadilha verificada.** A comparação é **literal com a string `"true"`**:
>
> ```rust
> cookie_secure: std::env::var("COOKIE_SECURE")
>     .map(|value| value == "true")
>     .unwrap_or(false),
> ```
>
> Qualquer outro valor resulta em `false`, **silenciosamente**. `TRUE`, `True`, `1`,
> `yes`, `on` e ` true ` (com espaço) **não** ativam a flag.
>
> Consequência: cookies de sessão trafegando em HTTP claro num ambiente que se
> acredita protegido. Registrado como **DT-04** em
> [../decisions/technical-debt.md](../decisions/technical-debt.md).

**Use exatamente `COOKIE_SECURE=true` em produção**, e confirme pela resposta:

```bash
curl -sI https://seu-host/login | grep -i 'strict-transport-security'
```

Se o cabeçalho HSTS estiver presente, a flag pegou.

| Valor | Cookies `Secure` | HSTS |
| --- | :---: | :---: |
| `true` | Sim | `max-age=63072000; includeSubDomains` |
| Qualquer outro / ausente | Não | Não |

### `BIND_ADDR` — padrão `0.0.0.0:3000`

Endereço e porta de escuta. **Validado no boot**: valor não parseável como socket
aborta com mensagem explícita.

| Valor | Efeito |
| --- | --- |
| `0.0.0.0:3000` | Aceita conexões de qualquer interface |
| `127.0.0.1:3000` | **Só local** — use atrás de proxy reverso |

O `Dockerfile` fixa `0.0.0.0:3000`, necessário para o container receber tráfego.

### `SESSION_TTL_MINUTES` — padrão `10`

Validade do JWT de acesso **e** do `Max-Age` do cookie: navegador e assinatura
expiram juntos.

**Zero é rejeitado** no boot — um TTL de zero significaria sessões que já nascem
expiradas.

| Valor | Efeito |
| --- | --- |
| Menor | Janela menor para token roubado; mais rotações (uma escrita no banco cada) |
| Maior | Menos escrita; **revogação de privilégio demora mais** — o `role` viaja nas claims |

### `REFRESH_TTL_DAYS` — padrão `14`

Validade do refresh token e da linha em `sessions`. **Zero é rejeitado.**

Define por quanto tempo o usuário permanece conectado sem novo login. Aumentar
amplia a janela de um refresh token roubado que nunca seja rotacionado.

### `QUOTES_SYNC_MINUTES` — padrão `10`

Intervalo do job que atualiza `assets.unit_value` **e** grava os snapshots de
patrimônio. **Zero desliga o job.**

| Valor | Efeito |
| --- | --- |
| `0` | **Preços nunca são atualizados** automaticamente. O botão manual continua funcionando. Numa instalação vazia, o catálogo **não é criado** |
| Menor | Preços mais frescos; mais chamadas externas e **mais linhas em `portfolio_snapshots`** |
| `10` (padrão) | 144 snapshots por usuário por dia |

> Esta variável é a que mais afeta o **crescimento do banco**: ela determina a taxa de
> `portfolio_snapshots`, que não tem expurgo (**DT-03**).

### `MARKET_SYNC_SECONDS` — padrão `60`

Intervalo do job que atualiza o snapshot de mercado **em memória**. **Zero desliga.**

O padrão acompanha o cache da fonte (~60 s): buscar mais rápido não traz número novo,
só gasta requisição do limite gratuito.

Com `0`, a tela de mercado fica permanentemente no estado "buscando as cotações" —
ela não tem caminho de atualização manual.

### `LOG_FORMAT` — padrão texto

| Valor | Efeito |
| --- | --- |
| `json` (ignora maiúsculas) | Uma linha JSON por evento — para CloudWatch, Loki, etc. |
| Qualquer outro / ausente | Formato legível de terminal |

Lida **fora** da `Config`, diretamente em `init_tracing`. Valor inválido não gera
erro — cai no formato de texto silenciosamente.

O `docker-compose.yaml` define `LOG_FORMAT: json` no serviço `app`.

### `RUST_LOG` — padrão `info`

Filtro do `tracing-subscriber` (sintaxe `EnvFilter`).

| Exemplo | Efeito |
| --- | --- |
| `info` | Padrão |
| `wallet=debug,info` | Debug só do próprio crate |
| `wallet=trace,sqlx=debug` | Inclui as queries executadas |
| `warn` | Só avisos e erros |

> **`sqlx=debug` registra as queries executadas, com parâmetros.** Em produção, isso
> pode colocar valores monetários e identificadores no log. Use com critério.

### `OTEL_EXPORTER_OTLP_ENDPOINT` — padrão desligado

Endpoint OTLP/HTTP para traces e métricas. **Ausente: nenhuma exportação e nenhuma
tentativa de conexão.**

Falha ao montar o exportador **não derruba o boot** — é reportada em `stderr` e o
serviço segue. Observabilidade é infraestrutura auxiliar
([ADR-0012](../adr/0012-observabilidade-opt-in-via-otlp.md)).

### `OTEL_SERVICE_NAME` — padrão `wallet`

Nome do serviço no backend. Sem ele, a maioria dos backends chama tudo de
`unknown_service`. Só tem efeito se o endpoint estiver definido.

## 4. Arquivo `.env`

Carregado por `dotenvy` no boot, se existir. **Não existir não é fatal** — em
produção as variáveis vêm do ambiente real.

```bash
cp .env.example .env
```

```powershell
Copy-Item .env.example .env
```

`.env` está no `.gitignore`. **Nunca versione o arquivo preenchido.**

Precedência: variáveis já presentes no ambiente **vencem** o `.env`.

## 5. Perfis de configuração

### Desenvolvimento local

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres
ADMIN_SECRET_KEY=<valor local>
JWT_SECRET=<valor local>
COOKIE_SECURE=false
```

### Produção atrás de HTTPS

```bash
DATABASE_URL=<conexão gerenciada>
ADMIN_SECRET_KEY=<48+ bytes aleatórios>
JWT_SECRET=<48+ bytes aleatórios>
COOKIE_SECURE=true          # exatamente "true"
LOG_FORMAT=json
BIND_ADDR=0.0.0.0:3000
OTEL_EXPORTER_OTLP_ENDPOINT=<coletor>
OTEL_SERVICE_NAME=wallet-prod
```

### Testes / CI

O CI define apenas as três obrigatórias, com valores fixos — o boot valida
**presença**, não valor:

```yaml
DATABASE_URL: postgres://postgres:postgres@localhost:5432/postgres
ADMIN_SECRET_KEY: ci-admin-secret
JWT_SECRET: ci-jwt-secret
```

## 6. Riscos de configuração incorreta

| Erro | Sintoma | Gravidade |
| --- | --- | --- |
| `COOKIE_SECURE` com valor ≠ `"true"` em produção | **Nenhum** — falha silenciosa | **Crítica** |
| `JWT_SECRET` fraco ou compartilhado | Nenhum até ser explorado | **Crítica** |
| `ADMIN_SECRET_KEY` deixado como `change-me` | Nenhum até ser explorado | **Crítica** |
| `.env` versionado por engano | Segredos no histórico do git | **Crítica** |
| `RUST_LOG=sqlx=debug` em produção | Queries com parâmetros no log | Alta |
| `QUOTES_SYNC_MINUTES=0` | Preços congelados; catálogo vazio numa instalação nova | Alta |
| `BIND_ADDR=127.0.0.1` em container | Container não recebe tráfego | Média |
| `MARKET_SYNC_SECONDS=0` | Tela de mercado permanentemente vazia | Baixa |
| `SESSION_TTL_MINUTES` muito alto | Revogação de privilégio demora | Média |

> As três primeiras têm a mesma característica perigosa: **não produzem sintoma**. Um
> serviço mal configurado assim funciona normalmente até ser explorado.

## 7. Evidências

```text
- src/config.rs   · Config, from_env, required, optional_positive, optional_non_negative
- src/app.rs      · init_tracing (LOG_FORMAT), init_otel (OTEL_*), App::start
- .env.example    (as 13 variáveis, com comentários)
- docker-compose.yaml       (serviço app)
- Dockerfile                (ENV BIND_ADDR=0.0.0.0:3000)
- .github/workflows/ci.yml  (as três obrigatórias no job test)
```
