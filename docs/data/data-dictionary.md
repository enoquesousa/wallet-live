# Dicionário de dados

## Objetivo

Descrever cada campo persistido: tipo, obrigatoriedade, origem do valor, quem
consome, sensibilidade e retenção. Responde, por campo, às perguntas "por que este
dado é coletado?", "de onde vem?" e "quem pode vê-lo?".

## Escopo

Coberto: os 28 campos das 6 tabelas, mais os dados que vivem fora do banco (cookies,
memória, log). Não coberto: restrições e índices (ver
[database-schema.md](database-schema.md)) e políticas de retenção agregadas (ver
[data-model.md](data-model.md)).

---

## Legenda

| Coluna | Significado |
| --- | --- |
| **Obr.** | Obrigatório (`NOT NULL`) |
| **Sens.** | Sensibilidade: **Alta** (credencial/financeiro), Média (identificação), Baixa |
| **Visível a** | Quem pode ler o valor pela interface ou API |

---

## 1. `users`

| Campo | Tipo | Obr. | Sens. | Origem | Visível a |
| --- | --- | :---: | --- | --- | --- |
| `id` | `BIGSERIAL` | Sim | Baixa | Gerado pelo banco | Ninguém (não é exposto) |
| `username` | `TEXT` | Sim | Média | Formulário de cadastro, trimado | O próprio usuário |
| `password_hash` | `TEXT` | Sim | **Alta** | `password_auth::generate_hash` | **Ninguém** |
| `balance` | `NUMERIC` | Sim | **Alta** | Depósitos e operações | O próprio usuário |
| `role` | `TEXT` | Sim | **Alta** | `DEFAULT 'user'`; promoção manual | O próprio usuário (via claims) |

**`password_hash`** — por que existe: autenticar sem armazenar a senha. Como é usado:
comparado por `password_auth::verify_password`. **Nunca é serializado**: `UserRecord`
não deriva `Serialize`, então não há caminho para ele aparecer numa resposta.
Retenção: enquanto a conta existir. **Não há rotação nem expiração de senha.**

> Se a hash armazenada não parseia, o código faz `panic!` deliberado — significa que
> o registro entrou por outra via, e continuar seria operar em estado inconsistente.

**`balance`** — saldo em caixa disponível. Debitado em compras, creditado em
depósitos e vendas. **Sempre alterado dentro de transação com `FOR UPDATE`**, que
serializa operações concorrentes do mesmo usuário. Escala canônica de 8 casas.

**`role`** — viaja nas **claims assinadas do JWT**, então a autorização não custa uma
consulta. Consequência: um novo token usa o papel atualizado, mas JWTs já emitidos
mantêm o papel até `exp + 30 s` (até 10 min e 30 s por padrão). Revogar o refresh
não invalida esses JWTs.

**Campos que deliberadamente não existem:** e-mail, telefone, nome completo, CPF,
data de nascimento, endereço, data de criação da conta, último acesso. O sistema
coleta o **mínimo** para autenticar.

## 2. `assets`

| Campo | Tipo | Obr. | Sens. | Origem | Visível a |
| --- | --- | :---: | --- | --- | --- |
| `id` | `BIGSERIAL` | Sim | Baixa | Banco | Qualquer um (API pública) |
| `name` | `TEXT` | Sim | Baixa | API admin ou bootstrap de cotações | Qualquer um |
| `unit_value` | `NUMERIC` | Sim | **Alta** | Sincronização Coinbase ou API admin | Qualquer um |

**`name`** — por que existe: identificar o ativo **e** casar com a resposta da API de
câmbio. O casamento é por nome normalizado (`LOWER(TRIM(...))`), e é por isso que o
nome é trimado na escrita: `"  bitcoin  "` grava como `"bitcoin"`, porque espaço
invisível criaria um ativo duplicado que a sincronização nunca atualizaria.

**`unit_value`** — **o campo mais sensível do sistema.** É o preço que lastreia toda
compra e venda: quem o altera altera o patrimônio de todos os usuários.

| Aspecto | Detalhe |
| --- | --- |
| Origem primária | Inverso da taxa BRL→moeda da Coinbase, arredondado para 8 casas |
| Origem secundária | `POST`/`PATCH /api/v1/assets` (exige admin) |
| **Nunca vem de** | CoinGecko — aquele feed é informativo e não toca esta coluna |
| Frequência de mudança | A cada `QUOTES_SYNC_MINUTES` (padrão 10 min) |
| Valor `0` | Tratado como **sem cotação**: o ativo não pode ser negociado |
| Histórico | **Não preservado** — só o valor atual. O preço histórico vive em `transactions.unit_value` |

A tabela é **pública** via `GET /api/v1/assets`: preço de ativo não é informação
privada de usuário.

## 3. `holdings`

| Campo | Tipo | Obr. | Sens. | Origem | Visível a |
| --- | --- | :---: | --- | --- | --- |
| `user_id` | `BIGINT` | Sim | Média | Sessão | — |
| `asset_id` | `BIGINT` | Sim | Baixa | Formulário de operação | O próprio usuário |
| `quantity` | `NUMERIC` | Sim | **Alta** | Calculado na compra/venda | O próprio usuário |
| `avg_cost` | `NUMERIC` | Sim | **Alta** | Média ponderada na compra | O próprio usuário |

**`quantity`** — unidades atualmente possuídas. A linha é **apagada** quando chega a
zero, então na prática é sempre estritamente positiva.

**`avg_cost`** — custo médio **ponderado** das unidades que permanecem. É a base do
lucro/prejuízo exibido: `(preço atual − avg_cost) × quantity`.

Duas regras de cálculo que merecem destaque, ambas travadas por teste:

| Operação | Efeito em `avg_cost` |
| --- | --- |
| **Compra** | Recalcula: `(qtd_antiga × custo_antigo + qtd_nova × preço_novo) / qtd_total` |
| **Venda parcial** | **Não altera.** O custo médio se refere ao que foi pago pelas unidades restantes; recalcular na venda **inventaria lucro** |
| Venda total | Linha apagada |

Retenção: enquanto a posição existir. **Ao fechar a posição, o custo médio é perdido**
— só reconstituível a partir de `transactions`.

## 4. `transactions`

| Campo | Tipo | Obr. | Sens. | Origem | Visível a |
| --- | --- | :---: | --- | --- | --- |
| `id` | `BIGSERIAL` | Sim | Baixa | Banco | — |
| `user_id` | `BIGINT` | Sim | Média | Sessão | — |
| `kind` | `TEXT` | Sim | Média | Rota acionada | O próprio usuário |
| `asset_id` | `BIGINT` | **Não** | Baixa | Formulário | O próprio usuário |
| `quantity` | `NUMERIC` | **Não** | **Alta** | Formulário | O próprio usuário |
| `unit_value` | `NUMERIC` | **Não** | **Alta** | `assets.unit_value` no momento | O próprio usuário |
| `cash_delta` | `NUMERIC` | Sim | **Alta** | Calculado | O próprio usuário |
| `created_at` | `TIMESTAMPTZ` | Sim | Média | `NOW()` | O próprio usuário |

**Esta tabela é imutável.** Nenhum caminho de código faz `UPDATE` ou `DELETE`.

**`kind`** — restrito por `CHECK` a `'deposit'`, `'buy'`, `'sell'`. Traduzido para
exibição, com fallback: um tipo novo no banco não pode virar tela em branco.

**Os três campos nulos** — `asset_id`, `quantity` e `unit_value` são `NULL` em
depósito, porque depósito não envolve ativo. Usar zero seria gravar um valor falso.

**`unit_value`** — o preço **no momento da operação**, não o atual. Sem ele, o
histórico perderia sentido: reconstruir uma compra de ontem com o preço de hoje
mostraria um número que nunca existiu.

**`cash_delta`** — movimento de caixa **assinado**:

| `kind` | Sinal | Valor |
| --- | --- | --- |
| `deposit` | Positivo | A quantia depositada |
| `buy` | **Negativo** | `−(quantity × unit_value)`, arredondado |
| `sell` | Positivo | `quantity × unit_value`, arredondado |

O sinal permite conferir a soma do extrato contra o saldo. É a base do CSV e da
auditoria.

**Retenção: permanente.** É o único dado com retenção indefinida por decisão de
projeto — é livro-razão. A migração de saneamento de escala deliberadamente **não
tocou** nesta tabela: os valores foram todos gravados via `Decimal`, logo já são
representáveis, e reescrever histórico contradiria a natureza da tabela.

**Este é o dado mais sensível do sistema do ponto de vista de privacidade**: o
histórico financeiro completo do usuário, exportável em CSV.

## 5. `sessions`

| Campo | Tipo | Obr. | Sens. | Origem | Visível a |
| --- | --- | :---: | --- | --- | --- |
| `id` | `BIGSERIAL` | Sim | Baixa | Banco | Ninguém |
| `user_id` | `BIGINT` | Sim | Média | Login | Ninguém |
| `token_hash` | `BYTEA` | Sim | **Alta** | SHA-256 do refresh token | **Ninguém** |
| `expires_at` | `TIMESTAMPTZ` | Sim | Baixa | `now + REFRESH_TTL_DAYS` | Ninguém |
| `created_at` | `TIMESTAMPTZ` | Sim | Baixa | `NOW()` | Ninguém |
| `revoked_at` | `TIMESTAMPTZ` | **Não** | Baixa | Logout ou rotação | Ninguém |

**`token_hash`** — **o valor em claro nunca toca o banco.** Só o SHA-256 é gravado,
então um vazamento do banco não entrega tokens utilizáveis.

**`revoked_at`** — nulo enquanto a sessão está viva. É preenchido em duas situações:
logout e rotação. Sustenta o `UPDATE ... RETURNING` atômico que reivindica a sessão
sem janela de corrida.

**Nenhum campo desta tabela é exposto por qualquer rota.** Não há listagem de sessões
ativas.

**Retenção: indefinida, sem limpeza.** Linhas revogadas e expiradas permanecem para
sempre. Cada renovação cria uma linha nova (rotação), então uma sessão ativa gera
~6 linhas/hora. **DT-02.**

**Não registra** IP, User-Agent, dispositivo ou geolocalização — o que limita
auditoria de acesso, e é também uma escolha de minimização de dados.

## 6. `portfolio_snapshots`

| Campo | Tipo | Obr. | Sens. | Origem | Visível a |
| --- | --- | :---: | --- | --- | --- |
| `id` | `BIGSERIAL` | Sim | Baixa | Banco | — |
| `user_id` | `BIGINT` | Sim | Média | Todos os usuários, por rodada | — |
| `total_value` | `NUMERIC` | Sim | **Alta** | Caixa + posições ao preço do momento | O próprio usuário |
| `captured_at` | `TIMESTAMPTZ` | Sim | Baixa | `NOW()` | O próprio usuário |

**Por que é coletado:** desenhar o gráfico de evolução do patrimônio. Sem série
histórica, o gráfico não existe.

**Quando:** uma linha **por usuário** a cada rodada de cotações — que é exatamente
quando os preços, e portanto o patrimônio, mudam. Inclui usuários com carteira vazia.

**Volume:** com o padrão de 10 minutos, **144 linhas por usuário por dia** (~52.500
por ano). O gráfico lê os últimos `CHART_POINTS = 60` pontos; o resto nunca é
consultado. **DT-03.**

**Retenção: indefinida, sem expurgo.** É a tabela que mais cresce, e a única cujo
crescimento é proporcional ao **tempo × usuários**, não à atividade.

## 7. Dados fora do banco

### 7.1 Cookies (navegador do usuário)

| Cookie | Conteúdo | Sens. | Retenção |
| --- | --- | --- | --- |
| `token` | JWT com `id`, `username`, `role` | **Alta** | 10 min |
| `refresh_token` | 32 bytes aleatórios | **Alta** | 14 dias |
| `csrf` | 32 bytes aleatórios | Alta | Sessão do navegador |
| `flash` | Mensagem em base64 | Baixa | 1 minuto |
| `lang` | `pt-BR` ou `en` | Baixa | — |

> **O JWT é legível** por quem tem o cookie: é assinado, não criptografado. Carrega
> `id`, `username` e `role` — e nada além disso deve ser colocado lá.

### 7.2 Memória do processo (perdido no restart)

| Dado | Onde | Conteúdo |
| --- | --- | --- |
| Snapshot de mercado | `Market` (`RwLock`) | 100 moedas, ~167 amostras de série cada |
| Contadores de lockout | `LoginThrottle` (`Mutex`) | Username normalizado + contagem + instante |
| Cooldown de cotações | `QuoteSync` (`Mutex`) | Instante da última rodada |
| Configuração | `Config` (`Arc`) | **Inclui `JWT_SECRET` e `ADMIN_SECRET_KEY`** |

> `LoginThrottle` armazena o **username normalizado** de tentativas falhas — um dado
> de identificação que sobrevive em memória por até 1 hora. Não é persistido nem
> registrado em log.

### 7.3 Logs

Um evento por requisição (`request completed`) com `request_id`, método, caminho,
status e latência.

**Não devem aparecer em log:** senha, valor de formulário, token, hash, segredo,
`DATABASE_URL`. A proteção principal é `#[instrument(skip_all)]` nos handlers, que
impede os argumentos de entrarem no span.

> **Não há verificação automática** de que dado sensível não vaza para o log. É
> disciplina de código sustentada por convenção. Registrado em
> [../security/threat-model.md](../security/threat-model.md).

Erros 5xx são logados **inteiros**, com causa raiz — e é justamente aí que uma
mensagem de erro do SQL pode conter valores de parâmetros. O log é, por isso, um
ativo sensível.

## 8. Resumo de retenção

| Dado | Retenção | Expurgo automático | Observação |
| --- | --- | :---: | --- |
| `users` | Enquanto a conta existir | Não | **Não há exclusão de conta** |
| `assets` | Permanente | Não | — |
| `holdings` | Enquanto a posição existir | Sim, ao zerar | Único expurgo automático do sistema |
| `transactions` | **Permanente** | Não | Por decisão — é livro-razão |
| `sessions` | **Indefinida** | Não | **DT-02** — deveria expurgar |
| `portfolio_snapshots` | **Indefinida** | Não | **DT-03** — cresce mais rápido |
| Cookies | 1 min a 14 dias | Sim (navegador) | — |
| Memória | Até o restart | Sim | — |
| Logs | Definida por quem opera | — | Fora do escopo da aplicação |

**Não há**: exclusão de conta, anonimização, exportação de dados pessoais (além do
CSV do próprio extrato), nem política formal de retenção. Para um sistema
educacional que não movimenta dinheiro real, é adequado; para operação real com
dados de terceiros, seria necessário revisar à luz da legislação aplicável.

## 9. Evidências

```text
- migrations/                (definição de cada campo, com comentários)
- src/models.rs              · Asset, UserRecord, UserIdentity, Holding,
                               Transaction, WalletSummary, PortfolioSnapshot
- src/repository.rs          (toda leitura e escrita)
- src/auth/session.rs        · hash_token, RefreshToken
- src/auth/throttle.rs       · LoginThrottle (dado em memória)
- src/market.rs              · Market (snapshot em memória)
- src/config.rs              · Config (segredos em memória)
- src/routes/frontend.rs     · transactions_to_csv
```
