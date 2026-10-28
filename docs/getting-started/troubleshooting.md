# Solução de problemas

## Objetivo

Reunir os problemas conhecidos de instalação e desenvolvimento, com sintoma, causa e
correção verificada.

## Escopo

Coberto: build, ambiente de desenvolvimento, banco, integrações externas e interface.
Não coberto: incidentes em execução, que têm procedimento próprio em
[../operations/runbooks.md](../operations/runbooks.md).

---

## 1. Build e dependências

### `CRYPT_E_NO_REVOCATION_CHECK` ao baixar dependências (Windows)

**Sintoma.** `cargo build` falha ao baixar crates, com erro de verificação de
revogação de certificado.

**Causa.** O `schannel` do Windows não consegue verificar a revogação do certificado
em algumas redes.

**Correção.** Já aplicada no repositório — `.cargo/config.toml` desativa **apenas** a
checagem de revogação para downloads do cargo:

```toml
[http]
check-revoke = false
```

Não afeta o TLS do programa compilado.

### `os error 10061` no rust-analyzer, com o arquivo inteiro em vermelho

**Sintoma.** Todo `query_as!` acusa erro no editor, mesmo com o código correto.

**Causa.** As macros do `sqlx` tentam validar as queries contra um banco vivo, e não
há nenhum de pé.

**Correção.** Já aplicada — `.cargo/config.toml` define `SQLX_OFFLINE = "true"`, que
faz a compilação usar o cache `.sqlx/` versionado.

Se persistir: reinicie o servidor do rust-analyzer, ou verifique se
`SQLX_OFFLINE=false` está definido no ambiente (variáveis do ambiente **vencem** o
config do cargo).

### `jwt-simple` exigindo `cmake`

**Sintoma.** Falha de build reclamando de `cmake` ou BoringSSL.

**Causa.** O `jwt-simple` usa BoringSSL por padrão.

**Correção.** Já aplicada — o `Cargo.toml` fixa `default-features = false` com a
feature `pure-rust`.

### Falha de build em `reqwest` / OpenSSL (Linux)

**Sintoma.** Erro de linkagem sobre `openssl` ou `pkg-config`.

**Causa.** `reqwest` com `native-tls` precisa do OpenSSL para compilar.

**Correção.**

```bash
sudo apt-get install -y pkg-config libssl-dev
```

O `Dockerfile` já faz isso no estágio builder.

## 2. Banco de dados

### `Connection refused` no boot

**Sintoma.** O serviço aborta com erro de conexão.

**Causa.** Postgres não está de pé, ou `DATABASE_URL` aponta para o lugar errado.

**Correção.**

```bash
docker compose up -d db
```

```bash
docker compose ps
```

O container precisa aparecer como `healthy`. Confira também a porta em
`DATABASE_URL` (padrão `5432`) e, se houver outro Postgres na máquina, o conflito de
porta.

### Falha de migração no boot

**Sintoma.** O boot aborta com erro de migração.

**Causa.** Schema inconsistente, migração parcialmente aplicada, ou banco de uma
versão anterior do projeto.

**Correção em desenvolvimento** — recriar o banco do zero:

```bash
docker compose down -v && docker compose up -d db
```

> ⚠️ **`-v` apaga todos os dados.** Só faça isso em desenvolvimento.

Em produção, ver [../operations/runbooks.md](../operations/runbooks.md).

### CI falhando em `cargo sqlx prepare --check`

**Sintoma.** O job `test` falha dizendo que o cache está desatualizado.

**Causa.** Uma query foi adicionada ou alterada sem regenerar o cache `.sqlx/`.

**Correção.** Com o banco de pé:

```bash
cargo sqlx prepare
```

Commit os arquivos alterados em `.sqlx/`.

### Volume do Postgres não persistindo

**Sintoma.** Dados somem ao recriar o container.

**Causa.** **Postgres 18 mudou o ponto de mount** para `/var/lib/postgresql` — os
dados ficam numa subpasta versionada, não mais em `/var/lib/postgresql/data`.

**Correção.** Já aplicada no `docker-compose.yaml`. Se você mantém um compose próprio
vindo de uma versão anterior, ajuste o mount.

## 3. Docker atrás de proxy corporativo ou antivírus

### `unable to get local issuer certificate` durante o build

**Sintoma.** O `cargo` dentro do container falha ao baixar dependências.

**Causa.** Proxy corporativo ou antivírus com inspeção TLS (Norton, ZScaler)
reassina o tráfego HTTPS com um CA próprio, que o container não conhece.

**Correção.** Exporte o certificado raiz do interceptador em **PEM com extensão
`.crt`** para `docker/extra-ca/`. O `Dockerfile` roda `update-ca-certificates` sobre
esse diretório; sem certificados, o passo é um no-op.

```powershell
$cert = Get-ChildItem Cert:\CurrentUser\Root, Cert:\LocalMachine\Root |
    Where-Object { $_.Subject -match 'NomeDoInterceptador' } | Select-Object -First 1
Export-Certificate -Cert $cert -FilePath docker\extra-ca\interceptador.cer -Type CERT
```

```bash
openssl x509 -inform der -in docker/extra-ca/interceptador.cer -out docker/extra-ca/interceptador.crt
```

Os `.crt`/`.cer` desse diretório são ignorados pelo git de propósito — são
específicos de cada máquina. Ver `docker/extra-ca/README.md`.

## 4. Aplicação

### Serviço não sobe: "variável de ambiente obrigatória ausente ou vazia"

**Causa.** Falta `DATABASE_URL`, `ADMIN_SECRET_KEY` ou `JWT_SECRET` — ou alguma está
**em branco**, o que é rejeitado como ausente.

**Correção.** A mensagem nomeia a variável. Confira o `.env` e lembre que variáveis
já presentes no ambiente **vencem** o arquivo.

### Catálogo de ativos vazio

**Sintoma.** A carteira não oferece nada para comprar.

**Causas possíveis:**

| Causa | Verificação |
| --- | --- |
| Primeira sincronização ainda não rodou | Aguarde ou use o botão **atualizar cotações** |
| Sem acesso à internet | O log mostra `scheduled quotes sync failed` |
| `QUOTES_SYNC_MINUTES=0` | O job está desligado; o catálogo **não** é criado |

**Correção alternativa** — cadastrar um ativo manualmente pela API administrativa:

```bash
curl -X POST http://127.0.0.1:3000/api/v1/assets -H "Authorization: $ADMIN_SECRET_KEY" -H 'Content-Type: application/json' -d '{"name":"ouro","unit_value":"750.25"}'
```

### Tela de mercado sempre "buscando as cotações"

**Causas:**

| Causa | Verificação |
| --- | --- |
| `MARKET_SYNC_SECONDS=0` | O job está desligado. **Não há botão manual para esta tela** |
| Primeira rodada ainda não completou | Aguarde até `MARKET_SYNC_SECONDS` |
| Fonte indisponível | O log mostra o erro da rodada |
| Serviço recém-reiniciado | O snapshot vive em memória e é perdido no restart, por decisão ([ADR-0009](../adr/0009-snapshot-de-mercado-em-memoria.md)) |

### `429 too many failed attempts` no login

**Causa.** Lockout progressivo após 5 falhas: 30 s dobrando, teto de 15 min.

**Correção.** Aguardar. Em desenvolvimento, reiniciar o serviço zera o estado — ele
vive em memória.

### Interface sem estilo, ou estilo desatualizado

**Sintoma.** A página carrega sem CSS, ou uma classe nova não tem efeito.

**Causa.** `static/app.css` não foi recompilado depois de mudar os templates.

**Correção.**

```bash
./tools/tailwindcss -i styles/app.css -o static/app.css --minify
```

Use exatamente a **versão 4.3.3** — divergir da versão do CI produz `diff` espúrio.
Commit o `static/app.css` gerado.

### CI falhando em "css compilado em dia com os templates"

Mesma causa e correção do item anterior: recompile e commit.

### Operação sem recarregar a página não funciona

**Sintoma.** Cada operação recarrega a página inteira.

**Isto não é necessariamente um defeito.** O sistema funciona **integralmente** sem
JavaScript — o htmx é *progressive enhancement*
([ADR-0003](../adr/0003-ssr-com-askama-e-htmx.md)). Se o JS estiver desativado ou o
htmx não carregar, o fluxo clássico de redirect assume.

Para confirmar que o htmx está carregando, verifique no console do navegador se
`/static/htmx.js` retornou `200`.

### Sessão caindo com frequência

**Causas possíveis:**

| Causa | Correção |
| --- | --- |
| `SESSION_TTL_MINUTES` muito baixo | Aumentar — mas o refresh deveria renovar transparentemente |
| `REFRESH_TTL_DAYS` expirado | Novo login |
| **Duas abas renovando ao mesmo tempo** | Comportamento esperado: a rotação é atômica e uma das abas perde a reivindicação |
| `JWT_SECRET` alterado | JWTs antigos deixam de validar; refresh válido renova nas rotas permitidas. Sem refresh válido, é preciso entrar de novo |
| Serviço reiniciado com segredo diferente | Idem |

### Cookies não marcados como `Secure` em produção

**Sintoma.** Cookies trafegando sem a flag, mesmo com `COOKIE_SECURE` definida.

**Causa.** A comparação é **literal com `"true"`**. `TRUE`, `1`, `yes` e ` true `
resultam em `false`, silenciosamente.

**Correção.** Use exatamente `COOKIE_SECURE=true` e confirme:

```bash
curl -sI https://seu-host/login | grep -i 'strict-transport-security'
```

O cabeçalho HSTS presente confirma que a flag pegou. Ver **DT-04** em
[../decisions/technical-debt.md](../decisions/technical-debt.md).

## 5. Testes

### Maior parte dos testes falhando com erro de conexão

**Causa.** Os testes de banco usam `#[sqlx::test]`, que precisa de um Postgres vivo.

**Correção.**

```bash
docker compose up -d db
```

Ou rode só o que não toca banco:

```bash
cargo test --test payload_market --test payload_quotes
```

### Snapshot `insta` falhando

**Causa.** O formato JSON de resposta da API mudou.

**Correção.** Se a mudança foi intencional:

```bash
cargo insta review
```

Se **não** foi, é quebra de contrato para consumidores — investigue antes de aceitar.

### `cargo clippy` falhando com warnings

**Causa.** O CI trata warnings como erro (`-D warnings`).

**Correção.** Corrija os apontamentos. `cargo clippy --fix` resolve parte deles
automaticamente.

## 6. Onde procurar mais

| Situação | Documento |
| --- | --- |
| Incidente com o serviço em execução | [../operations/runbooks.md](../operations/runbooks.md) |
| Efeito de cada variável | [configuration.md](configuration.md) |
| Como interpretar falha de teste | [../testing/test-strategy.md](../testing/test-strategy.md) §7 |
| Limitações conhecidas (não são defeitos) | [../decisions/known-limitations.md](../decisions/known-limitations.md) |
| Débitos técnicos registrados | [../decisions/technical-debt.md](../decisions/technical-debt.md) |

## 7. Evidências

```text
- .cargo/config.toml         (check-revoke, SQLX_OFFLINE)
- Cargo.toml                 (jwt-simple pure-rust)
- Dockerfile                 (pkg-config, libssl-dev, update-ca-certificates)
- docker/extra-ca/README.md
- docker-compose.yaml        (mount /var/lib/postgresql do Postgres 18)
- src/config.rs              · required (rejeita valor vazio)
- src/auth/throttle.rs       · LoginThrottle
- .github/workflows/ci.yml   (checagem de frescor do CSS)
```
