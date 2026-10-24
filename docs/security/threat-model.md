# Modelo de ameaças

## Objetivo

Inventariar os ativos do sistema, as ameaças a cada um, o controle existente e o
risco residual — de forma que a decisão de aceitar ou mitigar cada risco seja
explícita.

## Escopo

Coberto: ativos, agentes de ameaça, matriz ameaça × controle, cenários de ataque
analisados e riscos aceitos. Não coberto: a descrição dos controles (ver
[security-architecture.md](security-architecture.md)) e o processo de divulgação de
vulnerabilidade (ver [SECURITY.md](../../SECURITY.md)).

> **Contexto que calibra todo este documento:** o `wallet-live` é uma **simulação
> educacional**. Não movimenta dinheiro real, não integra meio de pagamento e não
> custodia ativo de terceiro. As avaliações de impacto abaixo consideram esse escopo.
> Numa operação real com dinheiro de usuários, vários "Médio" viram "Crítico".

---

## 1. Ativos

| # | Ativo | Onde | Valor para um atacante |
| --- | --- | --- | --- |
| A1 | **`JWT_SECRET`** | Ambiente, memória | **Máximo** — permite forjar qualquer sessão, inclusive admin |
| A2 | **`ADMIN_SECRET_KEY`** | Ambiente, memória | **Máximo** — controla o preço que lastreia todas as operações |
| A3 | **`assets.unit_value`** | Banco | **Máximo** — alterar move o patrimônio de todos |
| A4 | Credenciais do banco | `DATABASE_URL` | Alto — acesso total aos dados |
| A5 | Hashes de senha | `users.password_hash` | Alto — alvo de quebra offline |
| A6 | Saldo e posições | `users.balance`, `holdings` | Alto — dado financeiro do usuário |
| A7 | Extrato | `transactions` | Alto — histórico financeiro completo |
| A8 | Refresh tokens | Cookie (claro), banco (hash) | Alto — sessão persistente |
| A9 | Access tokens | Cookie | Médio — TTL nominal de 10 min mais 30 s de tolerância |
| A10 | Tokens CSRF | Cookie | Médio — habilita forja de requisição |
| A11 | Disponibilidade do serviço | Processo | Médio |
| A12 | Integridade do catálogo | `assets` | Alto |
| A13 | Logs | Saída do processo | Médio — podem conter dado de erro |
| A14 | Snapshot de mercado | Memória | Baixo — dado público de terceiro |

## 2. Agentes de ameaça

| Agente | Capacidade | Motivação |
| --- | --- | --- |
| Visitante anônimo | Acesso às rotas públicas | Acesso indevido, negação de serviço |
| Usuário autenticado | Sessão válida com papel `user` | Acessar dado de outro usuário, elevar privilégio |
| Site malicioso de terceiro | Induz o navegador da vítima | CSRF, clickjacking, exfiltração |
| Atacante na rede | Observa ou altera tráfego | Interceptar sessão |
| Fonte externa comprometida | Controla a resposta da Coinbase/CoinGecko | Envenenar preço, derrubar o serviço |
| Operador com acesso ao host | Lê ambiente e memória | — (dentro da fronteira confiável) |
| Atacante com dump do banco | Leitura do banco | Quebrar senhas, ler extrato |

## 3. Matriz de ameaças

**Legenda de risco residual:** calculado após os controles existentes.
Prob. = probabilidade.

### 3.1 Segredos e credenciais

| Ativo | Ameaça | Vetor | Impacto | Prob. | Controle existente | Risco residual | Controle recomendado |
| --- | --- | --- | --- | --- | --- | --- | --- |
| A1 | Vazamento do `JWT_SECRET` | `.env` versionado, log, variável exposta | **Crítico** | Baixa | Obrigatório e validado; nunca logado; `.env` no `.gitignore` | **Médio** | Rotação com duas chaves; gestor de segredos |
| A1 | Segredo fraco | Valor curto ou previsível | **Crítico** | **Média** | **Nenhum** — só se exige "não vazio" | **Alto** | Exigir comprimento e entropia mínimos no boot |
| A2 | `ADMIN_SECRET_KEY` deixada como `change-me` | `.env.example` público | **Crítico** | **Média** | Aviso na documentação | **Alto** | Recusar valores conhecidos no boot |
| A2 | Força bruta na credencial de admin | Requisições repetidas | Alto | Baixa | Tempo constante; **sem limite de tentativas** | **Médio** | Rate limiting em `/api/*` |
| A4 | Vazamento de `DATABASE_URL` | Log de erro 5xx | Alto | Baixa | 5xx censurado na resposta; **vai inteiro para o log** | Médio | Sanitizar credencial no log |

> **Duas linhas de probabilidade "Média" e risco "Alto"** — segredo fraco e
> `change-me` esquecido — são as mais acionáveis deste modelo. Ambas têm a mesma
> causa: o boot valida **presença**, não **qualidade**.

### 3.2 Sessão e autenticação

| Ativo | Ameaça | Vetor | Impacto | Prob. | Controle existente | Risco residual | Controle recomendado |
| --- | --- | --- | --- | --- | --- | --- | --- |
| A5 | Quebra offline de senha | Dump do banco | Alto | Baixa | argon2 | **Baixo** | — |
| A5 | Força bruta online | Login repetido | Médio | Média | Lockout 5 tentativas, backoff até 15 min, **antes** da senha | **Baixo** | Estado compartilhado entre réplicas |
| A5 | Enumeração de contas | Mensagem de erro distinta | Baixo | Média | **Mesma mensagem** para credencial inválida e conta inexistente | **Baixo** | — |
| A8 | Roubo de refresh token | XSS, acesso ao dispositivo | Alto | Baixa | `HttpOnly`, `SameSite=Strict`, CSP fechada, **rotação a cada uso** | **Baixo** | Vincular a impressão de dispositivo |
| A8 | Replay de token queimado | Reuso após rotação | Médio | Baixa | `UPDATE ... RETURNING` atômico — **sem janela de corrida** | **Baixo** | — |
| A8 | Vazamento pelo dump do banco | Leitura de `sessions` | Médio | Baixa | **Só a hash SHA-256 é gravada** | **Baixo** | — |
| A9 | Access token não revogável | Roubo antes de expirar | Médio | Baixa | TTL nominal de 10 min e tolerância de 30 s | **Médio** | Reduzir TTL; lista de revogação |
| A9 | Interceptação em HTTP claro | Rede hostil, `COOKIE_SECURE` mal configurado | **Alto** | **Média** | `Secure` condicional — **comparação literal com `"true"`** | **Alto** | Corrigir o parsing (DT-04); forçar HTTPS |
| A6 | Privilégio revogado ainda ativo | `role` nas claims | Médio | Baixa | Expiração do JWT mais 30 s de tolerância; revogar refresh só impede renovar | **Baixo** | Consultar papel no banco |

> **A linha de `COOKIE_SECURE` é o risco mais concreto do modelo.** A falha é
> silenciosa: um ambiente configurado com `COOKIE_SECURE=TRUE` (maiúsculas) roda
> normalmente, com os cookies de sessão trafegando sem a flag `Secure`.

### 3.3 Autorização e isolamento

| Ativo | Ameaça | Vetor | Impacto | Prob. | Controle existente | Risco residual | Controle recomendado |
| --- | --- | --- | --- | --- | --- | --- | --- |
| A6, A7 | Acesso ao dado de outro usuário | Manipular `asset_id` ou id em URL | Alto | Baixa | **Toda leitura filtra por `user_id` da sessão** | **Baixo** | — |
| A3 | Escrita não autorizada no catálogo | Chamar `POST`/`PATCH` sem credencial | **Crítico** | Baixa | Extrator `Admin`; teste de contrato | **Baixo** | — |
| A3 | Autorização acidental por header residual | `Authorization` de outra finalidade | Médio | Baixa | **Precedência**: sessão sem papel admin nega imediatamente | **Baixo** | — |
| A3 | Elevação de privilégio | Alterar o próprio papel | **Crítico** | Baixa | `role` só muda por `UPDATE` manual; **nenhuma rota expõe** | **Baixo** | — |
| A12 | Alteração indevida sem rastro | Admin legítimo ou credencial roubada | Alto | Baixa | **Nenhum log de auditoria** | **Médio** | Tabela de auditoria com autor e valor anterior |

### 3.4 Entrada e injeção

| Ativo | Ameaça | Vetor | Impacto | Prob. | Controle existente | Risco residual | Controle recomendado |
| --- | --- | --- | --- | --- | --- | --- | --- |
| A6 | Injeção de SQL | Qualquer entrada | **Crítico** | **Muito baixa** | **Toda query é parametrizada**; consultas dinâmicas não interpolam entrada | **Muito baixo** | — |
| A9, A10 | XSS | Campo de texto refletido | Alto | Baixa | Askama escapa por padrão; **CSP sem `unsafe-inline`**; teste trava o invariante | **Baixo** | — |
| A6 | CSRF | Site malicioso induz o navegador | Alto | Baixa | *Double-submit* + `SameSite=Strict`; teste verifica **o saldo**, não só o status | **Baixo** | — |
| A11 | Clickjacking | Iframe | Baixo | Baixa | `X-Frame-Options: DENY`, `frame-ancestors 'none'` | **Muito baixo** | — |
| — | Open redirect | `/lang/{code}?next=` | Médio | Baixa | **Só caminho local absoluto**; dois testes | **Baixo** | — |
| A13 | Injeção em log | Header `x-request-id` malicioso | Baixo | Baixa | Aceita ≤ 64 caracteres, só alfanuméricos e `-` | **Muito baixo** | — |
| A6 | Escala monetária excessiva | `INSERT` manual, caminho novo sem `round_dp` | Alto | Baixa | `round_dp` na escrita, `ROUND` na leitura, teste de regressão | **Médio** | `NUMERIC(38, 8)` no schema |

### 3.5 Terceiros e disponibilidade

| Ativo | Ameaça | Vetor | Impacto | Prob. | Controle existente | Risco residual | Controle recomendado |
| --- | --- | --- | --- | --- | --- | --- | --- |
| A3 | **Preço envenenado pela fonte** | Coinbase comprometida ou sequestrada | **Crítico** | **Muito baixa** | Escala validada; taxa ≤ 0 recusada. **Nenhuma validação de plausibilidade** | **Médio** | Recusar variação absurda entre rodadas |
| A11 | Pânico por payload malformado | Resposta inesperada | Alto | Baixa | Parse tipado; **erro em vez de pânico**, travado por teste | **Baixo** | — |
| A11 | Fonte indisponível | Rede | Baixo | Média | Timeout de 15 s; rodada falha é logada; preços anteriores seguem válidos | **Muito baixo** | — |
| A11 | Negação de serviço por volume | Requisições em massa | Médio | Média | **Nenhum limite global** | **Médio** | Rate limiting no proxy reverso |
| A11 | Exaustão de memória no lockout | Usernames inventados em massa | Baixo | Baixa | Varredura acima de 4096 entradas | **Baixo** | — |
| A11 | Exaustão de disco | Crescimento de `sessions` e `portfolio_snapshots` | Médio | **Média** | **Nenhum expurgo** | **Médio** | Job de limpeza (DT-02, DT-03) |
| A11 | DoS no hash de senha | Senha de tamanho enorme | Baixo | Baixa | Teto de 128 caracteres | **Muito baixo** | — |

### 3.6 Cadeia de suprimentos

| Ativo | Ameaça | Vetor | Impacto | Prob. | Controle existente | Risco residual | Controle recomendado |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Todos | Crate com vulnerabilidade | Dependência | Alto | Média | `cargo audit` em push e PR; `Cargo.lock` versionado | **Baixo** | Execução agendada |
| Todos | RUSTSEC-2023-0071 (`rsa`) | `jwt-simple` → `superboring` | Baixo **neste uso** | — | `ignore` documentado: só HS256 é usado, código RSA nunca exercitado | **Baixo** | Reavaliar quando houver fix |
| Todos | **htmx vendorado vulnerável** | `static/htmx.js` | Médio | Baixa | **Nenhum** — `cargo audit` não alcança JavaScript | **Médio** | Verificação de versão do htmx |
| Todos | Comprometimento de crate | Publicação maliciosa | Alto | Muito baixa | `Cargo.lock`; **zero dependências npm** | **Baixo** | — |
| Todos | CLI do Tailwind adulterado | Download do GitHub Releases no CI | Médio | Muito baixa | **Sem verificação de checksum** | **Baixo** | Fixar hash do binário |

## 4. Cenários analisados

### C1 — Atacante rouba um refresh token

**Caminho:** obtém o cookie por acesso ao dispositivo ou XSS (que a CSP torna
improvável).

**O que consegue:** renovar a sessão e agir como o usuário até a próxima rotação
legítima.

**O que o impede:** a **rotação a cada uso**. Quando o usuário legítimo renovar, o
`UPDATE ... RETURNING` encontra a sessão já reivindicada, e um dos dois é derrubado.
O ataque não é silencioso — a vítima é deslogada, o que é um sinal.

**Risco residual: Baixo.** Não há alerta automático nem listagem de sessões ativas
para a vítima investigar.

### C2 — Atacante obtém a `ADMIN_SECRET_KEY`

**O que consegue:** alterar `assets.unit_value` de qualquer ativo. Como o preço
lastreia compra e venda, isso permite manipular o patrimônio de todos os usuários —
por exemplo, zerar um preço para inviabilizar vendas, ou inflá-lo antes de uma venda
própria.

**O que o impede:** nada, uma vez de posse da credencial.

**O que limita:** o `CHECK (unit_value >= 0)` impede preço negativo; a escala é
travada. Mas um preço absurdo **positivo** é aceito.

**Risco residual: Médio.** Não há auditoria de quem alterou, nem validação de
plausibilidade entre rodadas. É o cenário que mais justifica os dois controles
recomendados na §3.3 e §3.5.

### C3 — Dump completo do banco

**O que o atacante obtém:** hashes argon2 (custosas de quebrar), hashes SHA-256 de
refresh tokens (**inúteis** — o valor em claro nunca foi gravado), extrato e saldos em
texto.

**O que **não** obtém:** senhas, tokens utilizáveis, `JWT_SECRET`, `ADMIN_SECRET_KEY`.

**Risco residual: Médio** — o histórico financeiro completo dos usuários fica exposto,
porque **não há criptografia em repouso**.

### C4 — Fonte de cotação comprometida

**Caminho:** a Coinbase é comprometida, ou o tráfego é interceptado, e responde taxas
manipuladas.

**O que acontece:** `assets.unit_value` recebe preços falsos, e todas as operações
subsequentes usam esses preços.

**O que o impede:** validação de **formato** e escala; taxa ≤ 0 é recusada. **Não há
validação de plausibilidade** — uma taxa que produza preço 1000× maior é aceita.

**Risco residual: Médio.** O controle recomendado — recusar variação absurda entre
rodadas — é simples e ainda não existe.

### C5 — Usuário tenta acessar dado de outro usuário

**Caminho:** manipular `asset_id` em formulário, ou tentar ids sequenciais.

**O que acontece:** nada. **Toda leitura filtra por `user_id` da sessão**, e não há
rota que aceite um id de usuário como parâmetro.

**Risco residual: Baixo.** É a razão pela qual `BIGSERIAL` foi mantido em vez de UUID
— decisão avaliada e registrada.

## 5. Riscos aceitos

Decisões conscientes, com a justificativa registrada:

| # | Risco aceito | Justificativa | Reavaliar se |
| --- | --- | --- | --- |
| RA-1 | Catálogo e preços públicos | Não é dado de usuário | A API se tornar comercial |
| RA-2 | Sem criptografia em repouso | Simulação educacional, sem dinheiro real | Houver dado real de usuário |
| RA-3 | Access token não revogável até `exp + 30 s` | Custo de consultar o banco a cada requisição | Requisito de revogação imediata |
| RA-4 | Lockout em memória | Instância única | Houver réplicas |
| RA-5 | RUSTSEC-2023-0071 ignorado | Só HS256 é usado; código RSA nunca exercitado | Houver fix, ou uso de RSA |
| RA-6 | Sem rate limiting global | Escopo educacional; mitigável no proxy | Exposição pública real |
| RA-7 | `BIGSERIAL` em vez de UUID | Ids só em superfície autenticada, leitura filtrada | API pública ou multi-tenant |
| RA-8 | Sem MFA | Escopo educacional | Operação real |

## 6. Ações prioritárias

Ordenadas por (impacto × probabilidade) ÷ esforço:

| # | Ação | Risco tratado | Esforço |
| --- | --- | --- | --- |
| 1 | **Corrigir o parsing de `COOKIE_SECURE`** (aceitar `true`/`1`/`yes`, sem distinção de caixa) | §3.2 — o único risco Alto com falha silenciosa | **Baixo** |
| 2 | **Exigir comprimento/entropia mínimos** em `JWT_SECRET` e `ADMIN_SECRET_KEY` no boot | §3.1 — dois riscos Altos | Baixo |
| 3 | **Recusar valores de exemplo conhecidos** (`change-me`) no boot | §3.1 | Baixo |
| 4 | Validação de plausibilidade nas cotações | C4 | Médio |
| 5 | Auditoria de alteração de preço | C2 | Médio |
| 6 | Expurgo de `sessions` e `portfolio_snapshots` | §3.5 | Médio |
| 7 | `NUMERIC(38, 8)` no schema | §3.4 | Médio |
| 8 | Verificação de versão do htmx vendorado | §3.6 | Baixo |
| 9 | Rate limiting no proxy reverso | §3.5 | Fora da aplicação |

**As três primeiras são de baixo esforço e tratam os únicos riscos residuais
classificados como Alto.**

## 7. Evidências

```text
- src/config.rs          · Config::from_env (valida presença, NÃO qualidade)
- src/auth/*             (todos os controles de sessão e autorização)
- src/app.rs             · security_headers
- src/error.rs           · IntoResponse (censura de 5xx)
- src/repository.rs      (queries parametrizadas; filtro por user_id)
- src/quotes.rs          · brl_price (recusa taxa ≤ 0; sem plausibilidade)
- .cargo/audit.toml      (RUSTSEC-2023-0071)
- migrations/20260716000000_financial_guardrails.up.sql
- migrations/20260716000001_create_sessions.up.sql
```
