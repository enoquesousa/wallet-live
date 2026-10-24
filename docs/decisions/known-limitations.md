# Limitações conhecidas

## Objetivo

Registrar o que o sistema **não faz** e o que ele **não garante** — consequências
conscientes de decisões de projeto, não defeitos a corrigir.

## Escopo

Coberto: limitações funcionais, de escala, de teste e de operação. Não coberto:
itens que deveriam ser corrigidos (ver [technical-debt.md](technical-debt.md)) e
análise de risco de segurança (ver
[../security/threat-model.md](../security/threat-model.md)).

> **A diferença entre este documento e o de débitos técnicos:** um débito é algo que
> **deveria** ser corrigido; uma limitação é uma consequência **aceita** de uma decisão
> deliberada. Esconder limitações para o projeto parecer mais maduro tornaria toda a
> documentação menos confiável.

---

## 1. Escopo do produto

O `wallet-live` é uma **simulação educacional**. Não movimenta dinheiro real, não
integra meio de pagamento e não custodia ativo de terceiro.

O que **não existe** como funcionalidade:

| Ausente | Consequência |
| --- | --- |
| **Saque** | Dinheiro entra por depósito e nunca sai. Não há `kind = 'withdraw'` |
| **Transferência entre usuários** | Nenhuma operação move valor entre contas |
| **Múltiplas moedas de denominação** | Tudo é BRL; não há coluna de moeda |
| **Venda a descoberto** | `CHECK (quantity >= 0)` impede |
| **Ordem limitada ou agendada** | Operações são a mercado, imediatas |
| **Estorno ou reversão** | `transactions` é imutável por desenho |
| **Preço histórico do catálogo** | `assets.unit_value` guarda só o valor atual |
| **Taxas, corretagem, impostos** | Não modelados |
| **Recuperação de senha** | **Senha esquecida é conta perdida** |
| **Troca de senha** | Não implementada |
| **Exclusão de conta** | Não implementada |
| **Autenticação multifator** | Não implementada |
| **Listagem de sessões ativas** | A tabela existe; nenhuma rota a expõe |
| **Notificações** | Nenhum canal (e-mail, push) |
| **Múltiplas carteiras por usuário** | Uma carteira por conta |

## 2. Escala e concorrência

### L-01 — O sistema presume **uma instância**

Três estados vivem na memória do processo:

| Estado | Consequência com N réplicas |
| --- | --- |
| `LoginThrottle` | Lockout **por instância** — N× mais tentativas toleradas |
| `QuoteSync` (`Mutex`) | A serialização das rodadas deixa de valer globalmente |
| `Market` (snapshot) | Cada réplica faz as suas chamadas — **N× o consumo da API externa** |

**Nenhum causa corrupção de dado**: as operações financeiras são protegidas por
transação e `FOR UPDATE` no banco. O impacto é em eficácia de defesa e consumo de API.

### L-02 — Snapshot de mercado não sobrevive ao restart

Deliberado ([ADR-0009](../adr/0009-snapshot-de-mercado-em-memoria.md)): é dado de
terceiro, volátil e informativo. Consequência: após cada deploy, a tela de mercado
fica em carregamento por até `MARKET_SYNC_SECONDS`.

### L-03 — Contadores de lockout zerados no restart

Um reinício libera todos os bloqueios de login em curso.

### L-04 — Sem cache de leitura

Cada carregamento da carteira executa 6 consultas (concorrentes, via `try_join!`).
Adequado à escala atual, **nunca medido sob carga**.

### L-05 — Sem limite global de requisições

Só o login tem lockout. Não há *rate limiting* geral — mitigável no proxy reverso.

## 3. Precisão e dados

### L-06 — `Decimal` tem 28 dígitos significativos

`NUMERIC` do Postgres é ilimitado; `rust_decimal::Decimal` não. O invariante
`MONEY_SCALE = 8` e os `ROUND` nos agregados mantêm as duas representações
compatíveis — mas **qualquer query nova que some ou multiplique dinheiro precisa do
`ROUND`**, e nada no compilador força isso.

### L-07 — `MONEY_SCALE = 8` é um teto

Suficiente para cripto (BTC tem 8 casas). Um ativo com precisão maior exigiria revisar
o invariante, as migrações e todos os `ROUND`.

### L-08 — A série do mercado usa `f64`

Exceção deliberada à regra de "dinheiro nunca é ponto flutuante": é **coordenada de
desenho**, não dinheiro. O campo é `pub(crate)` e só o projetor do gráfico o lê.

### L-09 — Operações financeiras não são idempotentes

Dois `POST /deposit` idênticos creditam duas vezes. Não há chave de idempotência.

### L-10 — `holdings` e `transactions` podem divergir

A consistência é garantida por transação, que é forte — mas depende de disciplina no
código, não de constraint. **Nenhuma reconciliação verifica a correspondência.**

## 4. Interface

### L-11 — Nenhum teste executa JavaScript

O htmx e a máscara monetária são verificados pelo **HTML que o servidor emite**
(atributos `hx-*`, ordem dos `<script>`, `defer`), não pelo comportamento no DOM. **Um
erro de runtime no htmx passaria pela suíte.**

Mitigação estrutural: o sistema funciona **integralmente sem JavaScript** — o htmx é
*progressive enhancement* ([ADR-0003](../adr/0003-ssr-com-askama-e-htmx.md)).

### L-12 — Layout não é verificado

Os testes conferem que as classes renderizadas existem no CSS compilado, não que o
resultado visual está correto. O painel de mercado empilhado abaixo de 1024 px é
comportamento correto do breakpoint — e nenhum teste distingue "correto" de
"indesejado".

### L-13 — Sem compatibilidade verificada entre navegadores

### L-14 — Dois idiomas apenas

pt-BR e inglês. Acrescentar um terceiro é barato (uma `const Strings`), mas os
formatos de moeda e data **não** acompanham o idioma: seguem a convenção do **dado**
(BRL, pt-BR). A tela em inglês mostra `R$ 10,00`, deliberadamente.

## 5. Integrações externas

### L-15 — Dependência de duas APIs públicas sem contrato

Coinbase e CoinGecko são públicas, sem chave, sem SLA. Podem mudar de formato,
introduzir limite ou sair do ar sem aviso.

Mitigação: 12 testes de contrato contra payloads reais versionados detectam mudança de
formato em CI.

### L-16 — Sem validação de plausibilidade das cotações

Formato e escala são validados; taxa `<= 0` é recusada. **Um preço absurdo mas válido é
aceito** — uma taxa que produza preço 1000× maior passa.

### L-17 — Chamadas de rede não são testadas

`fetch_brl_rates` e `market::fetch` não têm teste; o parse que elas alimentam tem.
Escolha deliberada: teste que bate em API de terceiro mede a internet, não o código.

### L-18 — Sem histórico próprio de cotação

A série temporal exibida é a que a CoinGecko entrega. Se a fonte parar de mandar
`sparkline`, o gráfico desaparece.

## 6. Operação

### L-19 — Não há ambiente de produção

O projeto é reproduzível localmente e preparado para container. Publicar é decisão
posterior de infraestrutura.

### L-20 — A aplicação não termina TLS

Fala HTTP puro; precisa de proxy reverso. `COOKIE_SECURE=true` alinha o comportamento
dos cookies.

### L-21 — Migração falha derruba o serviço

Deliberado: "melhor não subir do que subir contra um schema pela metade". Consequência:
deploy com migração ruim causa indisponibilidade, não degradação.

### L-22 — Rollback de binário não reverte migração

Os 11 arquivos `.down.sql` existem e **nunca foram testados**. Três reversões são
destrutivas ou impossíveis — ver [../data/migrations.md](../data/migrations.md) §3.

### L-23 — Jobs não participam do shutdown gracioso

Requisições em voo são drenadas; uma rodada de cotações em andamento é **abortada**.
Como a rodada é transacional, não há estado parcial persistido — a rodada simplesmente
não acontece.

### L-24 — Sem criptografia em repouso

Saldo, posições e extrato ficam em texto no Postgres. Só a senha é hasheada. **O
arquivo de backup, quando existir, será igualmente um ativo sensível.**

### L-25 — Sem alertas

O sistema emite sinais; nada define o que é anormal.

## 7. Contrato da API

### L-26 — Erro sem código estruturado

A resposta tem um único campo `error` com texto em inglês. Um cliente não consegue
distinguir programaticamente duas causas do mesmo status sem comparar strings — que
**não** são parte estável do contrato.

### L-27 — Sem `Retry-After`

Respostas `429` e `503` não indicam quanto esperar. Os valores estão documentados em
[../api/errors.md](../api/errors.md) §7.

### L-28 — A spec OpenAPI cobre só `/api/v1/assets`

As rotas HTML e as sondas não são descritas — não têm consumidor programático.

### L-29 — Alias `/api` sem destino definido

Quando existir um `/api/v2`, o comportamento do alias precisará ser decidido. Ver
[ADR-0011](../adr/0011-versionamento-da-api-por-caminho.md).

## 8. Testes

Lacunas permanentes, com os IDs de [../testing/test-matrix.md](../testing/test-matrix.md) §11:

| ID | Não coberto |
| --- | --- |
| N-01 | Reversão de migração |
| N-02 | Query nova de agregado sem `ROUND` |
| N-03 | Validação de configuração (`config.rs`) |
| N-04 | Comportamento de JavaScript no DOM |
| N-05 | Reconciliação `holdings` × `transactions` |
| N-06 | Corrida nas primitivas de concorrência dos jobs |
| N-07 | Que a imagem Docker sobe e serve |
| N-08 | Layout e responsividade |
| N-09 | Carga, latência, throughput |
| N-10 | Perda prolongada de conectividade |

E a cobertura instrumentada **não é medida** — ver
[../testing/coverage.md](../testing/coverage.md).

## 9. Segurança

Limitações estruturais, com os IDs de
[../security/security-architecture.md](../security/security-architecture.md) §4:

| ID | Limitação |
| --- | --- |
| RR-1 | Access token não revogável por até 10 min e 30 s com o TTL padrão |
| RR-2 | Revogação de privilégio não é imediata (o `role` viaja nas claims) |
| RR-4 | `JWT_SECRET` sem rotação — trocar invalida JWTs antigos, mas refresh válido pode renovar |
| RR-5 | `ADMIN_SECRET_KEY` compartilhada, sem escopo nem auditoria |
| RR-8 | Sem trilha de auditoria de alteração de preço |
| RR-12 | Sem limite global de requisições |

## 10. Como estas limitações foram escolhidas

Cada uma é consequência de uma decisão com custo e benefício registrados:

| Decisão | Ganho | Limitação aceita |
| --- | --- | --- |
| Snapshot em memória ([ADR-0009](../adr/0009-snapshot-de-mercado-em-memoria.md)) | Consumo constante de API; troca de moeda gratuita | L-02, L-18 |
| JWT stateless ([ADR-0007](../adr/0007-sessao-jwt-curto-com-refresh-rotativo.md)) | Validação sem tocar o banco | RR-1, RR-2 |
| htmx sem SPA ([ADR-0003](../adr/0003-ssr-com-askama-e-htmx.md)) | Zero build JS; CSP fechada | L-11, L-12 |
| Migração no boot ([ADR-0006](../adr/0006-sqlx-com-checagem-em-compilacao.md)) | Deploy sem passo manual | L-21, L-22 |
| `Decimal` ([ADR-0004](../adr/0004-decimal-e-numeric-para-dinheiro.md)) | Aritmética exata | L-06, L-07 |
| Payload real, sem testar rede | Suíte estável, sem falso alarme | L-17 |
| Instância única | Simplicidade operacional | L-01, L-03 |

**Nenhuma destas limitações é acidental.** Cada uma é o preço de um ganho que o
projeto quis ter.

## 11. Evidências

```text
- src/auth/throttle.rs   (L-01, L-03)
- src/market.rs          (L-02, L-08, L-16, L-18)
- src/models.rs          · MONEY_SCALE (L-06, L-07)
- src/repository.rs      (L-04, L-10)
- src/app.rs             · shutdown_signal (L-23), AppState::build (L-21)
- src/error.rs           (L-26)
- migrations/            (L-22, L-24)
- docs/testing/test-matrix.md §11
- docs/security/security-architecture.md §4
```
