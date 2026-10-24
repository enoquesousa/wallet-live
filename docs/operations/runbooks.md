# Runbooks de incidente

## Objetivo

Fornecer procedimentos de diagnóstico e recuperação para os incidentes previsíveis
deste sistema, com sintoma, causas, comandos, ação corretiva e critério de
escalonamento.

## Escopo

Coberto: 12 incidentes previsíveis a partir da arquitetura real. Não coberto:
problemas de instalação e desenvolvimento (ver
[../getting-started/troubleshooting.md](../getting-started/troubleshooting.md)) e o
significado de cada sinal (ver [observability.md](observability.md)).

> **Contexto:** não há ambiente de produção, plantão nem SLA. "Escalonar" significa,
> na prática, registrar uma issue com as evidências coletadas. Os procedimentos são
> escritos para serem válidos quando houver operação real.

---

## Estrutura

Cada runbook segue: **sintoma → impacto → causas → verificações → ação corretiva →
validação → rollback → evidências → escalonamento**.

## Índice

| # | Incidente | Gravidade |
| --- | --- | --- |
| [R-01](#r-01--o-serviço-não-inicia) | O serviço não inicia | **Crítica** |
| [R-02](#r-02--readyz-retorna-503) | `/readyz` retorna 503 | **Crítica** |
| [R-03](#r-03--porta-indisponível) | Porta indisponível | Alta |
| [R-04](#r-04--migração-falhou-no-boot) | Migração falhou no boot | **Crítica** |
| [R-05](#r-05--preços-congelados) | Preços congelados | **Alta** |
| [R-06](#r-06--500-na-tela-da-carteira) | 500 na tela da carteira | **Crítica** |
| [R-07](#r-07--tela-de-mercado-vazia) | Tela de mercado vazia | Baixa |
| [R-08](#r-08--usuários-não-conseguem-entrar) | Usuários não conseguem entrar | Alta |
| [R-09](#r-09--cookies-sem-a-flag-secure) | Cookies sem a flag `Secure` | **Crítica** |
| [R-10](#r-10--disco-crescendo-sem-parar) | Disco crescendo sem parar | Média |
| [R-11](#r-11--alto-consumo-de-cpu-ou-memória) | Alto consumo de CPU ou memória | Média |
| [R-12](#r-12--suspeita-de-preço-adulterado) | Suspeita de preço adulterado | **Crítica** |

---

## R-01 — O serviço não inicia

**Sintoma.** O processo termina logo após iniciar. O container reinicia em laço.

**Impacto.** Indisponibilidade total.

**Causas possíveis.** O boot tem quatro pontos que abortam deliberadamente:

| Causa | Mensagem característica |
| --- | --- |
| Segredo obrigatório ausente ou vazio | `variável de ambiente obrigatória ausente ou vazia: X` |
| `BIND_ADDR` malformado | `BIND_ADDR não é um endereço de socket válido` |
| TTL configurado como zero | `X deve ser um inteiro positivo` |
| Banco inacessível | Erro de conexão |
| Migração falhou | Ver [R-04](#r-04--migração-falhou-no-boot) |

**Verificações.**

```bash
docker compose logs --tail 50 app
```

```bash
docker compose exec app env | grep -E 'DATABASE_URL|BIND_ADDR|SESSION_TTL|REFRESH_TTL' | sed 's/:\/\/.*@/:\/\/***@/'
```

> O `sed` mascara a credencial da `DATABASE_URL` antes de exibir. **Nunca** imprima
> `ADMIN_SECRET_KEY` nem `JWT_SECRET` em terminal compartilhado.

**Ação corretiva.** A mensagem nomeia exatamente a variável. Corrija e reinicie.

Se a variável parece definida mas o serviço reclama: confira se ela está **vazia** —
valor em branco é rejeitado como ausente, por decisão.

**Validação.**

```bash
curl -fsS http://localhost:3000/healthz && curl -fsS http://localhost:3000/readyz
```

**Rollback.** Não aplicável — a falha é de configuração, não de versão.

**Evidências a coletar.** Log completo do boot; lista de variáveis definidas (sem
valores sensíveis).

**Escalonamento.** Se a mensagem não indicar a causa, ou se o boot falhar sem
mensagem.

---

## R-02 — `/readyz` retorna 503

**Sintoma.** `/healthz` responde `200`, `/readyz` responde `503`. O orquestrador tira
a instância do balanceador.

**Impacto.** Serviço vivo mas incapaz de atender — todas as telas dependem do banco.

**Causas.** Postgres fora do ar, rede entre app e banco, pool esgotado, credencial
alterada, disco do banco cheio.

**Verificações.**

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://localhost:3000/healthz http://localhost:3000/readyz
```

```bash
docker compose ps db
```

```bash
docker compose exec db pg_isready -U postgres
```

```bash
docker compose logs --tail 50 db
```

**Ação corretiva.**

| Causa | Ação |
| --- | --- |
| Banco parado | `docker compose up -d db` |
| Banco não saudável | Ver o log do banco; possível disco cheio |
| Rede | Verificar conectividade e DNS entre os containers |
| Credencial alterada | Corrigir `DATABASE_URL` e reiniciar o app |

> **Não reinicie o app como primeira ação.** A liveness separada existe exatamente
> para isso: reiniciar não conserta um Postgres fora do ar, e o reinício perde o
> snapshot de mercado e os contadores de lockout.

**Validação.** `/readyz` volta a `200` e a instância retorna ao balanceador.

**Evidências.** Status das duas sondas; `docker compose ps`; log do banco.

**Escalonamento.** Se o banco não voltar, ou se houver suspeita de perda de dado.

---

## R-03 — Porta indisponível

**Sintoma.** O boot falha com erro de bind.

**Impacto.** Indisponibilidade total.

**Causas.** Outro processo na porta; instância anterior não encerrada; `BIND_ADDR`
apontando para interface inexistente.

**Verificações.**

```bash
docker compose ps
```

```powershell
Get-NetTCPConnection -LocalPort 3000 -ErrorAction SilentlyContinue | Select-Object OwningProcess, State
```

```bash
ss -ltnp | grep 3000
```

**Ação corretiva.** Encerrar o processo anterior, ou mudar `BIND_ADDR`.

> **Em container, `BIND_ADDR` precisa ser `0.0.0.0:porta`.** Com `127.0.0.1`, o
> processo sobe mas o container não recebe tráfego externo — um sintoma que parece
> problema de rede e é de configuração.

**Validação.** `/healthz` responde.

**Escalonamento.** Raramente necessário.

---

## R-04 — Migração falhou no boot

**Sintoma.** O boot aborta com erro de migração. **É o incidente mais delicado deste
sistema.**

**Impacto.** Indisponibilidade total, com risco de schema em estado intermediário.

**Causas.** SQL inválido na migração nova; migração incompatível com o dado existente
(ex.: `CHECK` que o dado viola); migração parcialmente aplicada; rollback de binário
contra schema mais novo.

**Verificações.**

```bash
docker compose logs app | grep -A 20 -i migrat
```

```bash
docker compose exec db psql -U postgres -c "SELECT version, description, success, applied_on FROM _sqlx_migrations ORDER BY version DESC LIMIT 10;"
```

A coluna `success` indica se cada migração completou.

**Ação corretiva.**

| Situação | Ação |
| --- | --- |
| SQL inválido | Corrigir a migração, rebuildar, reimplantar |
| `CHECK` violado por dado existente | **Sanear o dado primeiro**, como fez a migração `normalize_money_scales` |
| Binário antigo contra schema novo | Reimplantar a versão nova; ver o aviso abaixo |
| Linha com `success = false` | Investigar antes de qualquer nova tentativa |

> ⚠️ **Rollback do binário não reverte migração.** Voltar a versão anterior **não**
> desfaz o que já foi aplicado. Se a migração nova é incompatível com o binário
> antigo, o rollback não resolve.

Reverter uma migração exige executar o `.down.sql` correspondente **manualmente** — e
esses arquivos **nunca foram testados**. Consulte
[../data/migrations.md](../data/migrations.md) §3, que documenta quais reversões
perdem dado. As reversões de `money_to_numeric` e `holdings_and_transactions` são
**destrutivas**.

**Validação.**

```bash
docker compose exec db psql -U postgres -c "SELECT version, success FROM _sqlx_migrations ORDER BY version DESC LIMIT 3;"
```

Todas com `success = true`, e o serviço sobe.

**Evidências.** Log completo do boot; conteúdo de `_sqlx_migrations`; **backup do
banco antes de qualquer intervenção manual**.

**Escalonamento.** **Sempre**, se houver migração parcialmente aplicada. Não improvise
SQL manual num schema em estado intermediário sem backup.

---

## R-05 — Preços congelados

**Sintoma.** `assets.unit_value` não muda há muito tempo. Os usuários operam a preços
desatualizados.

**Impacto.** **Alto** — este é o preço que lastreia compra e venda. Operações ocorrem
a valores que não refletem o mercado.

**Causas.** Fonte indisponível; `QUOTES_SYNC_MINUTES=0`; falha de rede ou DNS;
timeout de 15 s excedido; mudança de formato na resposta.

**Verificações.**

```bash
docker compose logs app | grep -i 'quotes sync' | tail -20
```

Interpretação das mensagens:

| Mensagem | Significado |
| --- | --- |
| `scheduled quotes sync` com `assets_updated` | Funcionando |
| `scheduled quotes sync failed` | Rodada falhou — o campo `error` diz por quê |
| `scheduled quotes sync disabled` | **O job está desligado** (`QUOTES_SYNC_MINUTES=0`) |

```bash
curl -s http://localhost:3000/api/v1/assets
```

Testar a fonte diretamente:

```bash
curl -sS -A "wallet/0.1.0" "https://api.coinbase.com/v2/exchange-rates?currency=BRL" | head -c 300
```

**Ação corretiva.**

| Causa | Ação |
| --- | --- |
| Job desligado | Definir `QUOTES_SYNC_MINUTES` > 0 e reiniciar |
| Fonte indisponível | Aguardar; a próxima rodada tenta de novo automaticamente |
| Rede/DNS | Corrigir a conectividade do container |
| **Formato mudou** | Rodar `cargo test --test payload_quotes`; se passar com o payload versionado mas a produção falha, **recapturar** o payload e verificar o que mudou |

Forçar uma rodada manual: usar o botão **atualizar cotações** na interface
(autenticado). O cooldown é de 30 s.

**Validação.** Log mostra `scheduled quotes sync` com `assets_updated` > 0, e
`GET /api/v1/assets` traz preços coerentes.

> **Não há alerta para este incidente.** Ele só é detectável lendo log — a ausência de
> métrica de negócio é a lacuna mais relevante da observabilidade atual.

**Evidências.** Log das rodadas; saída de `/api/v1/assets`; resposta da fonte.

**Escalonamento.** Se a fonte mudou de formato — exige correção de código.

---

## R-06 — 500 na tela da carteira

**Sintoma.** `GET /assets` responde `500` para contas **com posições**; contas vazias
funcionam.

**Impacto.** **Crítico** — usuários com posição não acessam a carteira.

> **Este incidente já aconteceu**, em 2026-07-22, e a assinatura é característica: o
> padrão "só quebra para quem tem posições" aponta diretamente para um estouro em
> agregado monetário.

**Causa provável.** Valor gravado com escala acima de `MONEY_SCALE`. Produtos e somas
de `NUMERIC` acumulam escala sem limite e estouram os 28 dígitos significativos do
`Decimal` na leitura — `value not representable`.

Origens possíveis: caminho de escrita novo sem `round_dp`; `INSERT` manual; query nova
de agregado **sem `ROUND(..., 8)`**.

**Verificações.**

```bash
docker compose logs app | grep -i 'not representable' | tail -5
```

Procurar valores fora do invariante:

```bash
docker compose exec db psql -U postgres -c "SELECT 'assets' t, id, scale(unit_value) s FROM assets WHERE scale(unit_value) > 8 UNION ALL SELECT 'holdings', asset_id, scale(avg_cost) FROM holdings WHERE scale(avg_cost) > 8 UNION ALL SELECT 'users', id, scale(balance) FROM users WHERE scale(balance) > 8;"
```

**Ação corretiva.**

Se houver valores fora do invariante, aplicar o mesmo saneamento da migração de 2026-07-22:

```bash
docker compose exec db psql -U postgres -c "UPDATE assets SET unit_value = ROUND(unit_value, 8) WHERE scale(unit_value) > 8; UPDATE holdings SET avg_cost = ROUND(avg_cost, 8) WHERE scale(avg_cost) > 8; UPDATE users SET balance = ROUND(balance, 8) WHERE scale(balance) > 8; UPDATE portfolio_snapshots SET total_value = ROUND(total_value, 8) WHERE scale(total_value) > 8;"
```

> **Não toque em `transactions`.** É histórico imutável, e seus valores foram gravados
> via `Decimal` — já são representáveis. Foi a decisão explícita da migração original.

Se **não** houver valores fora do invariante, a causa é outra: provavelmente uma query
de agregado nova sem `ROUND`. Procure no código a consulta que a tela executa.

**Validação.**

```bash
curl -s -o /dev/null -w '%{http_code}\n' -b 'token=<sessão de teste>' http://localhost:3000/assets
```

Deve responder `200`. E a suíte de regressão deve passar:

```bash
cargo test legacy_high_scale_money_still_renders_the_wallet
```

**Evidências.** Log com a mensagem de erro; resultado das consultas de escala;
identificação do caminho de escrita responsável.

**Escalonamento.** **Sempre** — indica regressão na disciplina de escala, e a causa
raiz (qual caminho gravou fora do invariante) precisa ser corrigida no código, não só
saneada no dado.

---

## R-07 — Tela de mercado vazia

**Sintoma.** `/market` mostra permanentemente "buscando as cotações".

**Impacto.** **Baixo.** A tela é informativa e **não** afeta operações financeiras.

**Causas.** `MARKET_SYNC_SECONDS=0`; primeira rodada ainda não completou; serviço
recém-reiniciado (o snapshot vive em memória, por decisão); fonte indisponível;
**403 por falta de `User-Agent`**.

**Verificações.**

```bash
docker compose logs app | grep -i market | tail -20
```

```bash
curl -sS -A "wallet/0.1.0" -o /dev/null -w '%{http_code}\n' "https://api.coingecko.com/api/v3/coins/markets?vs_currency=brl&per_page=1&page=1"
```

Comparar sem `User-Agent`:

```bash
curl -sS -o /dev/null -w '%{http_code}\n' "https://api.coingecko.com/api/v3/coins/markets?vs_currency=brl&per_page=1&page=1"
```

> Se o primeiro der `200` e o segundo `403`, está confirmado o comportamento conhecido
> da fonte: **ela recusa requisição sem `User-Agent`**. O serviço já envia um.

**Ação corretiva.** Aguardar até `MARKET_SYNC_SECONDS`; ligar o job se estiver em `0`;
verificar conectividade. **Não há botão manual para esta tela.**

**Validação.** `/market` passa a listar moedas.

**Escalonamento.** Só se a fonte mudou de formato.

---

## R-08 — Usuários não conseguem entrar

**Sintoma.** Login falhando para múltiplos usuários.

**Impacto.** Alto.

**Causas.**

| Causa | Sinal |
| --- | --- |
| Lockout ativo | `429 too many failed attempts` |
| `JWT_SECRET` alterado | Sessões existentes invalidadas |
| CSRF falhando | `403` ou banner de token inválido |
| Banco inacessível | Ver [R-02](#r-02--readyz-retorna-503) |
| Relógio do servidor errado | Tokens nascendo expirados |

**Verificações.**

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://localhost:3000/login
```

```bash
date -u && docker compose exec db psql -U postgres -c "SELECT NOW();"
```

> Diferença relevante de relógio entre app e banco causa expiração inconsistente de
> sessão — é uma causa fácil de esquecer.

**Ação corretiva.**

| Causa | Ação |
| --- | --- |
| Lockout | **Aguardar** (até 15 min). Reiniciar o serviço zera os contadores, que vivem em memória |
| `JWT_SECRET` trocado | JWT antigo não valida; refresh válido renova nas rotas permitidas. Sem refresh válido, é preciso entrar de novo |
| Relógio | Sincronizar NTP |

Revogar todas as sessões, se necessário:

```bash
docker compose exec db psql -U postgres -c "UPDATE sessions SET revoked_at = NOW() WHERE revoked_at IS NULL;"
```

**Validação.** Login bem-sucedido abre `/assets`.

**Escalonamento.** Se não houver causa identificável.

---

## R-09 — Cookies sem a flag `Secure`

**Sintoma.** Cookies de sessão trafegando sem `Secure`, mesmo com `COOKIE_SECURE`
definida.

**Impacto.** **Crítico** — cookies de sessão podem trafegar em HTTP claro e ser
interceptados.

**Causa.** A comparação é **literal com `"true"`**. `TRUE`, `True`, `1`, `yes`, `on` e
` true ` resultam em `false`, **silenciosamente**.

**Verificações.**

```bash
curl -sI https://seu-host/login | grep -iE 'strict-transport-security|set-cookie'
```

| Observação | Conclusão |
| --- | --- |
| `strict-transport-security` presente | A flag pegou |
| **Ausente** | `COOKIE_SECURE` **não** está ativa |

**Ação corretiva.** Definir exatamente `COOKIE_SECURE=true` (minúsculas, sem espaços)
e reiniciar.

**Validação.** O cabeçalho HSTS aparece, e os `Set-Cookie` incluem `Secure`.

**Evidências.** Cabeçalhos da resposta; valor da variável.

**Escalonamento.** Se houve tráfego em HTTP claro, considere as sessões daquele
período comprometidas e revogue-as (comando em [R-08](#r-08--usuários-não-conseguem-entrar)).

> Este incidente tem causa conhecida e correção de baixo esforço, registrada como
> **DT-04** e como ação prioritária no
> [modelo de ameaças](../security/threat-model.md) §6.

---

## R-10 — Disco crescendo sem parar

**Sintoma.** O volume do Postgres cresce continuamente, sem relação com a atividade
dos usuários.

**Impacto.** Médio, evoluindo para crítico — disco cheio derruba o banco.

**Causa.** Duas tabelas crescem **sem expurgo**, e uma delas cresce com o **relógio**,
não com o uso:

| Tabela | Cresce com | Taxa |
| --- | --- | --- |
| `portfolio_snapshots` | **Tempo × usuários** | **144 linhas/usuário/dia** com o padrão de 10 min |
| `sessions` | Renovações | ~6 linhas/hora por sessão ativa |

**Verificações.**

```bash
docker compose exec db psql -U postgres -c "SELECT relname, pg_size_pretty(pg_total_relation_size(relid)) size, n_live_tup FROM pg_stat_user_tables ORDER BY pg_total_relation_size(relid) DESC;"
```

```bash
docker compose exec db psql -U postgres -c "SELECT count(*) total, count(*) FILTER (WHERE revoked_at IS NOT NULL OR expires_at < NOW()) mortas FROM sessions;"
```

**Ação corretiva.**

Remover sessões mortas — seguro, pois já não autenticam:

```bash
docker compose exec db psql -U postgres -c "DELETE FROM sessions WHERE revoked_at IS NOT NULL OR expires_at < NOW();"
```

Reduzir snapshots antigos — o gráfico lê apenas os **últimos 60 pontos** por usuário:

```bash
docker compose exec db psql -U postgres -c "DELETE FROM portfolio_snapshots WHERE captured_at < NOW() - INTERVAL '90 days';"
```

> ⚠️ **Faça backup antes.** Este `DELETE` remove histórico de patrimônio de forma
> irreversível. Ajuste o intervalo conforme a necessidade de retenção.

Mitigação preventiva: aumentar `QUOTES_SYNC_MINUTES` reduz a taxa de crescimento
proporcionalmente.

**Validação.** Tamanho das tabelas estabiliza.

**Escalonamento.** Se o disco já estiver crítico.

> Registrado como **DT-02** e **DT-03**. A correção definitiva é um job de expurgo,
> que **não existe**.

---

## R-11 — Alto consumo de CPU ou memória

**Sintoma.** Uso elevado e sustentado.

**Impacto.** Médio.

**Causas prováveis, em ordem:**

| Causa | Sinal |
| --- | --- |
| Muitas requisições simultâneas | Latência alta na métrica |
| Hash argon2 sob volume de login | Picos de CPU correlacionados a `/login` |
| Snapshot de mercado | ~100 moedas × ~167 amostras, constante |
| Mapa de lockout | Até 4096 entradas |
| Pool de conexões saturado | Latência alta com CPU baixa |

**Verificações.**

```bash
docker stats --no-stream
```

```bash
docker compose logs app | grep 'request completed' | tail -50
```

**Ação corretiva.**

> **argon2 é deliberadamente custoso** — é o que torna a hash resistente a força
> bruta. Consumo de CPU em picos de login é comportamento esperado, não defeito.

| Causa | Ação |
| --- | --- |
| Volume de requisições | Rate limiting no proxy reverso — **não há limite global na aplicação** |
| Ataque de login em massa | O lockout já mitiga; considerar bloqueio por IP no proxy |
| Pool saturado | Avaliar o dimensionamento do Postgres |

> **Não há teste de carga nem baseline de desempenho.** Não existe referência do que é
> "normal" para este sistema.

**Escalonamento.** Se o consumo não corresponder ao volume observado.

---

## R-12 — Suspeita de preço adulterado

**Sintoma.** Um ativo com preço implausível; usuários relatando valores estranhos.

**Impacto.** **Crítico** — o preço lastreia compra e venda; um valor adulterado move o
patrimônio de todos.

**Causas.** `ADMIN_SECRET_KEY` comprometida; `JWT_SECRET` comprometido (permite forjar
sessão de admin); escrita administrativa indevida; fonte de cotação comprometida.

**Verificações.**

```bash
docker compose exec db psql -U postgres -c "SELECT id, name, unit_value FROM assets ORDER BY id;"
```

Comparar com a cotação de mercado. Procurar escritas no log:

```bash
docker compose logs app | grep -E 'PATCH|POST' | grep assets | tail -30
```

> ⚠️ **Não há trilha de auditoria.** O log registra que houve requisição, **não quem
> autorizou nem qual era o valor anterior**. Esta é uma lacuna conhecida —
> **RR-8** em [../security/security-architecture.md](../security/security-architecture.md).

**Ação corretiva.**

1. **Corrigir o preço imediatamente** — via API administrativa ou `UPDATE` direto.
2. **Trocar `ADMIN_SECRET_KEY` e `JWT_SECRET`**, e reiniciar.
3. **Revogar todas as sessões:**

   ```bash
   docker compose exec db psql -U postgres -c "UPDATE sessions SET revoked_at = NOW() WHERE revoked_at IS NULL;"
   ```

4. **Auditar as operações do período:**

   ```bash
   docker compose exec db psql -U postgres -c "SELECT t.id, t.user_id, t.kind, a.name, t.quantity, t.unit_value, t.cash_delta, t.created_at FROM transactions t LEFT JOIN assets a ON a.id = t.asset_id WHERE t.created_at > NOW() - INTERVAL '24 hours' ORDER BY t.created_at DESC;"
   ```

   `transactions` é imutável e registra o preço **no momento da operação** — é a
   evidência de quais operações usaram o preço adulterado.

5. Avaliar reparação das operações afetadas. **Não há mecanismo de estorno**: o
   livro-razão é imutável por desenho.

**Validação.** Preços coerentes; credenciais trocadas; sessões revogadas.

**Evidências.** Valores antes e depois; `transactions` do período; log das requisições.

**Escalonamento.** **Sempre.** É o cenário C2 do
[modelo de ameaças](../security/threat-model.md).

---

## Comandos de diagnóstico frequentes

```bash
docker compose ps
```

```bash
docker compose logs -f --tail 100 app
```

```bash
curl -s -o /dev/null -w 'healthz=%{http_code}\n' http://localhost:3000/healthz && curl -s -o /dev/null -w 'readyz=%{http_code}\n' http://localhost:3000/readyz
```

```bash
docker compose exec db psql -U postgres -c "SELECT count(*) FROM users;"
```

```bash
docker compose logs app | grep '<request_id>'
```

## Incidentes **não** cobertos

Cenários possíveis sem procedimento definido, por falta de infraestrutura
correspondente:

| Cenário | Por que não há procedimento |
| --- | --- |
| **Restauração após corrupção do banco** | **Não há backup implementado** — ver [backup-and-recovery.md](backup-and-recovery.md) |
| Certificado TLS expirado | TLS é terminado fora da aplicação |
| Failover de banco | Instância única, sem réplica |
| Degradação sob carga | Sem baseline de desempenho |
| Perda prolongada de conectividade | Comportamento não exercitado |

## Evidências

```text
- src/app.rs             · App::start, liveness, readiness, request_tracing
- src/config.rs          · Config::from_env (mensagens de erro do boot)
- src/quotes.rs          · spawn_scheduled_sync (eventos de log)
- src/market.rs          · USER_AGENT (o 403 conhecido)
- src/repository.rs      · wallet_summary (ROUND nos agregados)
- migrations/20260722000000_normalize_money_scales.up.sql  (o saneamento de R-06)
- docs/security/threat-model.md  (cenários C1–C5)
```
