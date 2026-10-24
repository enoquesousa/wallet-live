# Justificativa das tecnologias

## Objetivo

Documentar, para cada linguagem, framework e biblioteca relevante: onde é usada,
qual requisito atende, quais limitações traz, quais alternativas existiam e em que
condições a escolha deveria ser reavaliada.

## Escopo

Coberto: as 12 tecnologias com impacto arquitetural. Não coberto: dependências
transitivas e utilitárias (ver [dependency-management.md](../development/dependency-management.md)
para o inventário completo e a política de atualização) e as decisões formalizadas
como ADR (ver [../adr/](../adr/), que traz alternativas e consequências no formato
canônico).

## Aviso sobre a natureza destas justificativas

> **O motivo histórico da maioria destas escolhas não está formalmente registrado
> no repositório.** O projeto nasceu como projeto final de um bootcamp, e a
> primeira escolha de stack foi feita sem ADR. As justificativas abaixo são
> **análise técnica baseada na implementação atual** e, quando o código ou um
> comentário nomeia explicitamente o motivo, isso está marcado como **motivo
> confirmado** com a evidência correspondente.

Legenda usada nas seções:

- **Motivo confirmado** — há evidência textual no repositório (comentário, doc de
  módulo, mensagem de migração, configuração).
- **Justificativa inferida** — reconstrução técnica a partir do uso observado.
- **Recomendação de formalização** — o que deveria virar ADR ou registro.

---

## 1. Rust

### Utilização no projeto

Linguagem única de todo o sistema, edition 2024, ~8.700 linhas. Um só crate, com
alvo binário (`src/main.rs`) **e** alvo de biblioteca (`src/lib.rs`). Cobre
backend, regras financeiras, renderização de HTML, projeção de SVG, jobs em
segundo plano e acesso a banco — não há segunda linguagem no servidor.

### Motivação técnica

**Motivo confirmado** para a escolha do bootcamp (o projeto é o trabalho final de
um curso de Rust — evidência: `docs/delivery/course-delivery.md`). Isso torna a
linguagem uma **restrição de contexto**, não uma escolha livre — e é importante
registrar isso com honestidade em vez de inventar uma comparação que não
aconteceu.

**Justificativa inferida** de por que a restrição se mostrou adequada ao domínio:

- **Tipos como invariantes de negócio.** `User` tem campos privados e só pode ser
  construído por um fluxo de autenticação: ter um `User` em mãos é prova de
  autenticação cumprida. `Admin` é uma unit struct que existe apenas como prova de
  autorização. Uma rota desprotegida é visível na **assinatura** do handler, não
  numa lista de exceções em outro arquivo.
- **Erro exaustivo.** `AppError` é um enum de 21 variantes, e o `match` que mapeia
  para status HTTP não compila se uma variante nova ficar sem tratamento.
- **Ausência de runtime e de GC.** Distribuição como binário único, sem
  interpretador nem máquina virtual na imagem final.
- **Verificação em compilação estendida a SQL e HTML.** Combinada com `sqlx` e
  `askama`, uma coluna renomeada ou uma variável ausente num template viram erro
  de build.

### Limitações

- **Curva de aprendizado.** Ownership, lifetimes e async são custos reais de
  entrada para quem mantém.
- **Tempo de compilação.** Build limpo de release na ordem de minutos; o CI
  depende de `Swatinem/rust-cache` para ser tolerável.
- **Ecossistema web menos maduro** que JVM, .NET ou Node em bibliotecas de nicho.
  Consequência concreta neste projeto: `jwt-simple` traz `rsa` transitivamente, com
  um advisory sem correção upstream (ver §11).
- **Verbosidade em código de fronteira.** Cada extrator exige uma `impl
  FromRequestParts`.

### Alternativas consideradas

Nenhuma foi avaliada em código — a linguagem era premissa do curso. Comparação
técnica *post hoc*, para registro:

| Alternativa | O que ofereceria | O que se perderia |
| --- | --- | --- |
| TypeScript / Node | Ecossistema web maior, iteração mais rápida | GC no caminho financeiro; ausência de decimal exato nativo; cadeia de suprimentos npm (que este projeto evita **inteiramente**, inclusive no build de CSS) |
| Go | Compilação rápida, binário único, concorrência simples | Sem enums com dados nem exaustividade de `match`; tratamento de erro por convenção em vez de tipo |
| Java / Kotlin (Spring) | `BigDecimal` maduro, ferramental corporativo | JVM na imagem; consumo de memória base muito maior |
| C# / .NET | `decimal` nativo de 128 bits, bom ORM | Runtime na imagem; menor afinidade com deploy minimalista |

### Evidências no repositório

```text
- Cargo.toml            (edition 2024)
- src/lib.rs            (alvo de biblioteca que habilita tests/)
- src/auth/user.rs      (campos privados de User)
- src/error.rs          (enum de 21 variantes)
```

### Quando reavaliar

Se a equipe de manutenção não tiver familiaridade com Rust, ou se o projeto passar
a exigir bibliotecas que só existem maduras em outro ecossistema.

---

## 2. axum (framework web)

### Utilização no projeto

Roteamento, extratores, middlewares e serialização de resposta. 16 rotas de
frontend, 2 de API × 2 prefixos, 3 sondas, 3 camadas de middleware.

### Motivação técnica

**Motivo confirmado.** O repositório documenta explicitamente a comparação com
Rocket, o framework web apresentado no curso — evidência:
[ADR-0002](../adr/0002-axum-em-vez-de-rocket.md), e um comentário em
`src/app.rs` contrasta os modelos de autenticação.

O argumento central é o **modelo de extratores**: o handler declara nos parâmetros
o que exige (`State`, `User`, `Admin`, `Repository`, `Locale`), e o axum resolve
cada um **antes** de o corpo do handler rodar. Se um extrator falha, o handler
**nunca executa**. A consequência prática é que a proteção de uma rota é uma
propriedade da sua assinatura, não de uma configuração global que alguém pode
esquecer de atualizar — contraste explícito com o middleware global de
autenticação do Rocket.

**Justificativa inferida** adicional: axum é construído sobre `tower`, e é isso que
permite à suíte de testes empurrar requisições pelo router **de produção** com
`tower::oneshot`, sem abrir socket nem porta. Um framework sem essa composição
obrigaria a testar handlers isolados — deixando de fora CSP, renovação de sessão e
span da requisição, justamente as camadas que ninguém confere à mão.

### Limitações

- **Erros de tipo hostis.** Um handler cuja assinatura não satisfaz os traits do
  axum produz mensagens de erro longas e indiretas.
- **Ordem de `.layer()` invertida** em relação à leitura: a última adicionada
  executa primeiro. Exige comentário explicativo no código, e ele existe.
- **Rupturas entre versões menores.** A série 0.x ainda quebra API (0.7 → 0.8
  mudou a sintaxe de parâmetro de rota para `{code}`).

### Alternativas consideradas

| Alternativa | Avaliação |
| --- | --- |
| **Rocket** | Avaliado e documentado. Ergonomia inicial melhor (macros de rota), mas autenticação por middleware global em vez de por assinatura, e menor composição com `tower` |
| actix-web | Desempenho comparável ou superior; modelo de atores mais complexo do que o problema exige |
| warp | Composição por filtros; tipos ainda mais difíceis de ler em erro |
| Sem framework (hyper puro) | Controle total ao custo de reimplementar roteamento e extração |

### Evidências no repositório

```text
- Cargo.toml            (axum 0.8.9, features = ["macros"])
- src/app.rs            · App::router (ordem das camadas, com o porquê comentado)
- src/auth/admin.rs     · impl FromRequestParts for Admin
- tests/common/mod.rs   (tower::oneshot sobre App::router)
- docs/adr/0002-axum-em-vez-de-rocket.md
```

### Quando reavaliar

Se o axum estabilizar em 1.0 com quebras significativas, ou se o projeto passar a
precisar de WebSocket/streaming em escala que exponha limitação da pilha `tower`.

---

## 3. tokio (runtime assíncrono)

### Utilização no projeto

Runtime multi-thread, jobs em segundo plano (`tokio::spawn`), primitivas de
sincronização (`Mutex`, `RwLock`), consultas concorrentes (`try_join!`), tratamento
de sinais (SIGTERM/Ctrl+C) e temporizadores (`interval`).

### Motivação técnica

**Justificativa inferida.** É o runtime que axum e sqlx exigem — a escolha é, na
prática, consequência das duas anteriores. O uso próprio que o projeto faz dele
tem motivos documentados:

- **`tokio::try_join!`** nas seis consultas independentes da carteira: o tempo total
  é o da mais lenta, não a soma. **Motivo confirmado** — comentário em
  `src/services/portfolio.rs`.
- **Duas primitivas diferentes para os dois jobs**, e a diferença é deliberada.
  **Motivo confirmado** nos comentários de `quotes.rs` e `market.rs`:

  | Job | Primitiva | Por que |
  | --- | --- | --- |
  | Cotações | `Mutex` adquirido durante a rodada inteira | Exclusão mútua real: duas escritas simultâneas no catálogo administrativo seriam um problema |
  | Mercado | `RwLock` | Toda requisição **lê**, só o job **escreve** (1×/min). `RwLock` permite leituras concorrentes sem fila |

### Limitações

- **Cor de função.** `async` contamina a assinatura de tudo acima dele.
- **Cancelamento cooperativo.** Um `.await` abandonado pode deixar trabalho pela
  metade; o desligamento gracioso mitiga isso para requisições em voo, mas **não**
  para os jobs.
- **`std::sync::Mutex` vs `tokio::sync::Mutex`** é uma armadilha recorrente: manter
  o primeiro através de um `.await` bloqueia o executor.

### Alternativas consideradas

`async-std` (menor ecossistema, incompatível com o resto da pilha),
`smol` (mais leve, sem o ferramental), ou threads bloqueantes com pool (modelo mais
simples, incompatível com axum e sqlx).

### Evidências no repositório

```text
- Cargo.toml                    (features rt-multi-thread, macros, net, sync, signal)
- src/services/portfolio.rs     · wallet_view (try_join!)
- src/quotes.rs                 · QuoteSync (Mutex)
- src/market.rs                 · Market (RwLock)
- src/app.rs                    · shutdown_signal
```

### Quando reavaliar

Não há gatilho realista enquanto axum e sqlx forem mantidos.

---

## 4. rust_decimal + NUMERIC — a decisão mais consequente do projeto

### Utilização no projeto

`rust_decimal::Decimal` ↔ `NUMERIC` do Postgres para **todo** valor monetário:
saldo, preço unitário, quantidade, custo médio, `cash_delta`, valor total de
snapshot. `f64` aparece exclusivamente em coordenadas de desenho SVG.

### Motivação técnica

**Motivo confirmado, com evidência em migração e em incidente real.**

A migração `20260613000000_money_to_numeric.up.sql` nomeia o motivo da troca de
`DOUBLE PRECISION` para `NUMERIC`:

> "`DOUBLE PRECISION` carries rounding noise (e.g. 0.1 + 0.2 != 0.3) that is
> unacceptable for financial values."

E há um segundo motivo, descoberto **em produção**, que a documentação registra em
detalhe: `NUMERIC` é ilimitado, mas `Decimal` tem **28 dígitos significativos**. A
sincronização de cotações gravava `preço = 1/taxa` sem arredondar, e a divisão de
`Decimal` preenche a mantissa inteira. Um preço individual com 28 casas ainda cabe;
o **produto ou a soma** desse valor com outro — exatamente o que o resumo da
carteira faz — estoura o limite, e a leitura de volta falha com `value not
representable`. `/assets` respondia **500 para qualquer conta com posições**.

A correção tem três camadas, todas verificáveis:

1. **Escrita arredonda sempre** para `MONEY_SCALE = 8` (`brl_price`,
   `validated_unit_value`, `buy_asset`, `sell_asset`).
2. **Leitura envolve todo agregado em `ROUND(..., 8)`** — necessário porque
   produtos e somas de `NUMERIC` acumulam escala sem limite.
3. **A migração `normalize_money_scales` saneou o estado já gravado**, deixando
   `transactions` intacto por ser histórico imutável de valores já representáveis.

E existe um teste de regressão nomeado pelo incidente:
`legacy_high_scale_money_still_renders_the_wallet` planta deliberadamente valores
de 28 casas no banco e confirma que toda leitura continua decodificando.

### Limitações

- **Aritmética mais lenta** que ponto flutuante nativo. Irrelevante nesta escala.
- **28 dígitos significativos é um limite real**, não teórico — como o incidente
  provou. Qualquer query nova que some ou multiplique dinheiro precisa do `ROUND`.
- **Impedância com `f64`** em fronteiras de terceiros: a CoinGecko devolve número
  JSON, e a conversão precisa travar a escala explicitamente
  (`decimal_from_f64`), porque `from_f64_retain` traz o erro de representação
  binária (0,1 vira 0,1000000000000000055…).
- **`MONEY_SCALE = 8` é uma escolha, não uma lei.** É sub-centavo suficiente para
  cripto, mas ativos com precisão maior exigiriam revisão do invariante inteiro.

### Alternativas consideradas

| Alternativa | Por que não |
| --- | --- |
| `f64` / `DOUBLE PRECISION` | Era o estado inicial, e foi **removido por migração**. Ruído de arredondamento inaceitável |
| Inteiro de centavos (`i64`) | Exato e rápido, mas 2 casas é insuficiente para cripto (BTC tem 8 casas) e toda formatação passaria a exigir divisão manual |
| `bigdecimal` | Precisão arbitrária, sem o teto de 28 dígitos — resolveria o incidente na raiz. Perde a integração direta com `sqlx` e `serde` que o `rust_decimal` tem, e não estava disponível como escolha no material do curso |

### Evidências no repositório

```text
- src/models.rs                                    · MONEY_SCALE
- src/quotes.rs                                    · brl_price (round_dp obrigatório)
- src/repository.rs                                · validated_unit_value, buy_asset, wallet_summary
- src/market.rs                                    · decimal_from_f64
- migrations/20260613000000_money_to_numeric.up.sql
- migrations/20260722000000_normalize_money_scales.up.sql
- src/repository.rs · legacy_high_scale_money_still_renders_the_wallet
```

### Quando reavaliar

Se um ativo exigir mais de 8 casas decimais, ou se algum agregado passar a
encadear multiplicações a ponto de 28 dígitos significativos ficarem apertados
mesmo com `ROUND`. Nesse caso, `bigdecimal` é a substituição natural.

---

## 5. sqlx (acesso a dados)

### Utilização no projeto

Todo o SQL do sistema, em `src/repository.rs`. As queries principais usam
`sqlx::query!`/`query_as!`, **verificadas em tempo de compilação** contra o schema
real. Também fornece o runner de migrações (`sqlx::migrate!`, embutido no binário)
e o `#[sqlx::test]`, que cria um banco efêmero por teste.

### Motivação técnica

**Motivo confirmado** para preferir SQL explícito a um ORM — evidência:
[ADR-0006](../adr/0006-sqlx-com-checagem-em-compilacao.md), que discute a
alternativa Diesel.

**Justificativa inferida** do ganho concreto: a verificação em compilação transforma
uma classe inteira de erro de runtime em erro de build. Coluna renomeada, tipo
trocado, `NULL` não tratado — tudo aparece em `cargo build`, não na primeira
requisição em produção. É o mesmo princípio que o Askama aplica a templates e o
`i18n::Strings` a traduções, e essa **coerência de abordagem** é um dos traços mais
consistentes do projeto.

O cache offline `.sqlx/` (31 arquivos versionados) permite compilar sem banco —
essencial para o `lint` do CI, para o build Docker (`SQLX_OFFLINE=true`) e para o
rust-analyzer não acusar o arquivo inteiro em vermelho quando o Postgres está
desligado (**motivo confirmado** em `.cargo/config.toml`).

### Limitações

- **O cache pode descolar do schema.** Mitigado por `cargo sqlx prepare --check` no
  CI, mas é um passo manual a cada query nova.
- **Consultas dinâmicas perdem a verificação.** Onde a query é montada em runtime
  (bootstrap do catálogo, `SELECT 1` da sonda), a checagem não se aplica — e o
  projeto restringe deliberadamente esses casos a dois lugares.
- **Sem migração automática de tipos.** Renomear uma coluna exige migração,
  regeneração de cache e ajuste do código.
- **`#[sqlx::test]` exige Postgres de pé.** Torna a suíte completa dependente de
  Docker.

### Alternativas consideradas

| Alternativa | Por que não |
| --- | --- |
| **Diesel** | Avaliado no material do curso. DSL própria e checagem via macros; mais abstração entre o autor e o SQL executado, e menos afinidade com `async` |
| SeaORM | ORM async completo; entrega geração de query ao custo de opacidade — exatamente o que o projeto quis evitar num sistema financeiro |
| `tokio-postgres` puro | Sem verificação em compilação, sem migrações, sem `#[sqlx::test]` |

### Evidências no repositório

```text
- Cargo.toml                (sqlx 0.9, features macros/postgres/runtime-tokio/migrate/time/rust_decimal)
- src/repository.rs         (query!/query_as! em todo o arquivo)
- .sqlx/                    (31 queries em cache)
- .cargo/config.toml        (SQLX_OFFLINE = "true", com o motivo comentado)
- .github/workflows/ci.yml  (cargo sqlx prepare --check)
```

### Quando reavaliar

Se o projeto passar a precisar de query dinâmica extensa (filtros compostos
opcionais), o ponto forte do `sqlx` — verificação estática — deixa de se aplicar à
maior parte do código.

---

## 6. Askama (templates) + htmx (interatividade)

### Utilização no projeto

7 templates HTML compilados no binário via `#[derive(Template)]`. htmx 2.0.8
vendorado em `static/htmx.js` (51 KB) e servido pelo próprio binário via
`include_str!`.

### Motivação técnica

**Motivo confirmado** para os dois, com evidência em código e em roadmap.

**Askama:** as variáveis usadas no `.html` são verificadas contra a struct em tempo
de compilação. Duas structs por tela (`AssetsPage`/`WalletFragment`,
`MarketPage`/`MarketFragment`) compartilham o mesmo tipo de dado e o mesmo
fragmento interno — a diferença é só o esqueleto ao redor.

**htmx com dois caminhos simultâneos:** toda ação tem o `action`/`method` de um
formulário POST normal **e** os atributos `hx-*` que interceptam o mesmo clique
quando JavaScript está disponível. O handler devolve fragmento ou página inteira
conforme o header `HX-Request` — nunca dois códigos de handler para o mesmo dado.
Sem JavaScript, o fluxo clássico de redirect continua inteiro
(*progressive enhancement*).

O ganho concreto e mensurável: **a CSP pode fechar em `'self'`**, sem
`'unsafe-inline'`. Isso só é possível porque não existe `<style>` nem `<script>`
inline em nenhuma página, e há um teste que trava o invariante
(`pages_carry_no_inline_style_or_script`) iterando todo `<script` de cada página
renderizada e falhando se algum não tiver `src=`.

### Limitações

- **Erro de template é erro de compilação**, com mensagem por vezes indireta.
- **htmx não é testado no DOM.** A suíte verifica o HTML emitido (atributos `hx-*`,
  ordem dos `<script>`, `defer`), não o comportamento em navegador. Um erro de
  runtime no htmx passaria — limitação registrada em
  [../decisions/known-limitations.md](../decisions/known-limitations.md).
- **Ausência de componentes reativos.** Estado complexo de cliente exigiria
  repensar a abordagem.
- **Cada mudança de HTML exige recompilar** o binário.
- **htmx vendorado precisa de atualização manual**, e não há verificação
  automatizada de que a versão embutida não tem vulnerabilidade conhecida (o
  `cargo audit` cobre só crates Rust).

### Alternativas consideradas

| Alternativa | Por que não |
| --- | --- |
| SPA (React/Vue/Svelte) | Introduziria cadeia npm, build separado, estado duplicado e API pública para o próprio frontend. Desproporcional a formulários e tabelas |
| Tera / Handlebars | Templates em runtime: erro de variável só aparece ao renderizar |
| Maud (templates em macro Rust) | Verificação equivalente, mas HTML deixa de ser HTML — perde-se edição direta do markup |
| htmx via CDN | **Era o estado anterior e foi removido.** Requisição a terceiro em cada carregamento, e dependência de disponibilidade externa para a interface funcionar |
| Alpine.js | Exigiria `unsafe-eval` na CSP |

### Evidências no repositório

```text
- Cargo.toml                (askama 0.16, features = ["derive"])
- templates/               (7 arquivos)
- static/htmx.js           (2.0.8, vendorado)
- src/routes/frontend.rs   · render_wallet, is_partial_request, app_css, htmx_js
- src/app.rs               · security_headers (CSP sem unsafe-inline)
- teste: pages_carry_no_inline_style_or_script
```

### Quando reavaliar

Se a interface passar a exigir estado de cliente complexo (edição colaborativa,
formulários com dependência dinâmica profunda) ou atualização em tempo real via
WebSocket.

---

## 7. Tailwind CSS via CLI standalone

### Utilização no projeto

`styles/app.css` (3,8 KB, fonte) é compilado para `static/app.css` (19 KB,
minificado) pelo executável standalone do Tailwind, e o resultado é **versionado**
como o cache `.sqlx`.

### Motivação técnica

**Motivo confirmado**, com dois motivos distintos documentados em `styles/app.css`
e no roadmap:

1. **Saída do Play CDN.** O CDN é um **compilador que roda no navegador** — 407 KB
   de JavaScript por carregamento — e injeta `<style>` em runtime, o que obrigava a
   CSP a carregar `style-src 'unsafe-inline'`, justamente a diretiva que mais
   enfraquece uma política. Com CSS estático, a CSP fecha e o custo em JS cai a
   zero.
2. **`source(none)` desliga a varredura automática.** Sem isso, o Tailwind vasculha
   o repositório inteiro e recolhe candidatos de arquivos que não são template —
   **inclusive do próprio CSS gerado, que assim se auto-alimenta e nunca solta uma
   classe removida**. O efeito prático era um build não-determinístico: a mesma
   entrada rendia CSS diferente em máquinas diferentes, e o check de frescor do CI
   quebrava sozinho.

O CLI standalone é um executável único: **sem Node e sem npm**, então o build não
herda a cadeia de suprimentos do ecossistema JavaScript. Esse é o traço mais
distintivo desta escolha — o projeto tem **zero dependências JS na cadeia de
build**.

### Limitações

- **O CSS compilado é artefato versionado**, e pode descolar dos templates. O CI
  recompila e faz `diff` para provar frescor — mas é um passo manual a cada classe
  nova.
- **O binário do CLI não é versionado** (`/tools/` está no `.gitignore`): quem
  precisa recompilar baixa a versão certa por conta própria.
- **Versão fixada em dois lugares** — o CI baixa `v4.3.3` explicitamente, e o
  desenvolvedor local pode ter outra, gerando `diff` espúrio.
- Classes usadas dinamicamente (montadas em runtime) não são detectadas pelo
  gerador.

### Alternativas consideradas

| Alternativa | Por que não |
| --- | --- |
| **Tailwind Play CDN** | Estado anterior, removido: 407 KB de JS e CSP enfraquecida |
| Tailwind via npm/PostCSS | Traria Node e a árvore npm ao build — exatamente o que se evitou |
| CSS artesanal | Sem utilitários; mais código para manter, mas zero ferramenta de build |
| Sass/SCSS | Outra ferramenta externa, sem o ganho de utilitários |

### Evidências no repositório

```text
- styles/app.css                     (fonte, com o porquê no comentário de topo)
- static/app.css                     (compilado, versionado, embutido no binário)
- .github/workflows/ci.yml           (job lint: baixa v4.3.3, recompila, diff)
- .gitignore                         (/tools/ — o CLI não é versionado)
```

### Quando reavaliar

Se a divergência de versão do CLI entre CI e máquinas locais começar a produzir
falsos negativos com frequência, vale fixar a versão num arquivo lido pelos dois.

---

## 8. jwt-simple + password-auth + subtle (autenticação)

### Utilização no projeto

- **`jwt-simple`** (feature `pure-rust`): assina e valida o JWT de acesso com
  HS256.
- **`password-auth`**: gera e verifica hash de senha com argon2.
- **`subtle`**: comparação em tempo constante da credencial administrativa e do
  token CSRF.
- **`sha2`**: hash do refresh token antes de gravar.

### Motivação técnica

**Motivo confirmado** em vários pontos:

- **`pure-rust` dispensa BoringSSL e cmake** — evidência: nota de ambiente no
  README. Reduz atrito de build no Windows.
- **`password-auth` em vez de argon2 direto:** a biblioteca escolhe o algoritmo e
  os parâmetros, e a hash armazenada **inclui o algoritmo usado** — o que permite
  migrar de algoritmo sem migração de dados. O comentário em
  `UnauthenticatedUser::authenticate` é explícito: "não sabemos (by design) como a
  hash é verificada".
- **`subtle` para comparar segredos:** o tempo de resposta não deve vazar, byte a
  byte, quanto do segredo bateu antes de divergir.
- **SHA-256 no refresh token, não argon2:** o token é 32 bytes de aleatoriedade do
  SO, não uma senha escolhida por humano. Não há ataque de dicionário a mitigar, e
  o custo de argon2 seria pago em **cada renovação de sessão** sem ganho.

### Limitações

- **HS256 é simétrico:** o mesmo segredo assina e valida. Múltiplos serviços
  validando o token compartilhariam a capacidade de emiti-lo. Adequado a um
  serviço único; RS256/EdDSA seria necessário num cenário distribuído.
- **`jwt-simple` traz `rsa` transitivamente** (via `superboring`), com
  **RUSTSEC-2023-0071** (Marvin Attack) **sem correção upstream**. O advisory está
  documentadamente ignorado em `.cargo/audit.toml`, com justificativa verificável:
  esta aplicação assina e valida exclusivamente com HS256, então o código RSA nunca
  é exercitado e o canal lateral não é alcançável. **Requer reavaliação** se houver
  fix upstream ou se algum algoritmo RSA passar a ser usado.
- **`jwt_simple::Error` não implementa `std::error::Error`** (é um `anyhow::Error`
  por baixo), o que impede `#[from]`/`transparent` no `thiserror` e obriga a um
  `impl From` manual guardando só a mensagem.
- **`role` nas claims** significa que revogação de privilégio não é instantânea.

### Alternativas consideradas

| Alternativa | Por que não |
| --- | --- |
| `jsonwebtoken` | Mais popular; exigiria avaliar sua própria árvore de dependências. Trocaria um conjunto de advisories por outro |
| Sessão puramente opaca (sem JWT) | Toda requisição consultaria o banco. O JWT curto existe justamente para que a validação do caminho quente não toque o Postgres |
| `argon2` direto | Mais controle sobre parâmetros; perde a negociação automática de algoritmo |
| bcrypt | Mais antigo, sem resistência a hardware dedicado comparável |

### Evidências no repositório

```text
- Cargo.toml            (jwt-simple pure-rust, password-auth, subtle, sha2)
- src/auth/user.rs      · auth_token, from_auth_token, authenticate
- src/auth/admin.rs     · ct_eq
- src/auth/csrf.rs      · verify_csrf (ct_eq)
- src/auth/session.rs   · hash_token
- .cargo/audit.toml     (RUSTSEC-2023-0071 com justificativa)
```

### Quando reavaliar

Imediatamente, se um algoritmo RSA passar a ser usado. Também se o sistema ganhar
uma segunda instância que precise **validar** tokens sem poder **emiti-los** — aí
HS256 deixa de servir.

---

## 9. OpenTelemetry (observabilidade opt-in)

### Utilização no projeto

`tracing` + `#[instrument]` em praticamente todo handler. Exportação OTLP/HTTP de
traces e do histograma `http.server.request.duration`, **ligada apenas** se
`OTEL_EXPORTER_OTLP_ENDPOINT` estiver definida.

### Motivação técnica

**Motivo confirmado** — o comentário de `init_otel` explica a decisão de projeto
mais interessante deste componente: os instrumentos são construídos a partir do
`Meter` **global**. Se nenhum `MeterProvider` foi instalado, os handles funcionam
do mesmo jeito, só que descartam tudo. **Sem `Option`, sem ramificação no caminho
quente** — o serviço nunca precisa saber se a exportação está ligada.

Segunda decisão explícita: **falha ao montar um exportador não derruba o boot**, ao
contrário de um segredo obrigatório ausente. A justificativa está no código:
"observabilidade é infraestrutura auxiliar, não algo pelo qual vale a pena recusar
servir requisições financeiras."

Terceira: a métrica é rotulada pelo **padrão de rota** (`MatchedPath`), não pela
URL crua. Um 404 em caminho aleatório viraria uma série nova a cada requisição —
cardinalidade ilimitada no backend.

### Limitações

- **Rotação de versões acoplada.** `opentelemetry`, `opentelemetry-otlp`,
  `opentelemetry_sdk` e `tracing-opentelemetry` precisam de versões compatíveis
  entre si; atualizar um exige atualizar os quatro.
- **`Drop` do `OtelGuard` é a única garantia de flush.** Nenhum dos providers expõe
  flush pela ponta do `tracing`; um `abort` do processo perde o lote em buffer.
- **Uma métrica só.** Só o histograma de duração é exportado. Não há contador de
  erro, gauge de pool de conexões nem métrica de negócio.
- **Sem tracing distribuído de entrada.** O `request_id` é propagado, mas o
  contexto W3C `traceparent` não é lido de requisições upstream.

### Alternativas consideradas

| Alternativa | Por que não |
| --- | --- |
| `tracing` puro (só logs) | Era o estado anterior. Sem latência agregada nem correlação com backend externo |
| Prometheus (endpoint `/metrics`) | Modelo pull; exigiria expor uma rota e não daria traces. OTLP cobre os dois sinais com um protocolo |
| Sem observabilidade | Diagnóstico de produção dependeria de leitura de log linha a linha |

### Evidências no repositório

```text
- Cargo.toml                        (opentelemetry 0.32, otlp, sdk, tracing-opentelemetry)
- src/app.rs                        · init_otel, init_tracing, OtelGuard, request_tracing
- docker/otel-collector/config.yaml (coletor local de verificação)
- docker-compose.yaml               (perfil observability)
```

### Quando reavaliar

Quando houver um backend de observabilidade real em operação: aí vale acrescentar
métricas de negócio (operações por tipo, falhas de sincronização) e ler
`traceparent` de entrada.

---

## 10. PostgreSQL 18

### Utilização no projeto

Único armazenamento persistente. 6 tabelas, 11 migrações, 3 índices explícitos.
Recursos usados: `NUMERIC`, `TIMESTAMPTZ`, `BYTEA`, `BIGSERIAL`, `CHECK`, `UNIQUE`,
chave primária composta, `FOR UPDATE`, `UPDATE ... RETURNING`, `ON CONFLICT`,
`scale()`.

### Motivação técnica

**Justificativa inferida**, com um elemento confirmado: `NUMERIC` é o tipo que
sustenta a decisão monetária (§4), e a migração que introduz `NUMERIC` nomeia o
motivo. O restante da escolha não tem registro histórico.

Recursos que o projeto **depende** e que não são universais:

- **`NUMERIC` de precisão arbitrária** — a base do núcleo financeiro.
- **`FOR UPDATE`** — serializa compras concorrentes do mesmo usuário.
- **`UPDATE ... RETURNING` atômico** — a rotação de sessão sem janela de corrida
  depende disto.
- **`CHECK` como última linha de defesa** — nenhum caminho de escrita, nem SQL
  manual, consegue persistir preço negativo.
- **`scale()`** — usado pela migração de saneamento para encontrar valores fora do
  invariante.

### Limitações

- **Dependência operacional adicional.** Exige Docker ou um servidor gerenciado; a
  suíte completa de testes não roda sem ele.
- **Postgres 18 mudou o ponto de mount** para `/var/lib/postgresql` (os dados ficam
  em subpasta versionada), e não mais `/var/lib/postgresql/data` — documentado no
  compose, porque quebra volumes de versões anteriores.
- **Sem estratégia de backup implementada.** Existe um volume nomeado `pgdata`,
  nada além. Ver [../operations/backup-and-recovery.md](../operations/backup-and-recovery.md).
- **Instância única.** Sem réplica, sem failover.

### Alternativas consideradas

| Alternativa | Por que não |
| --- | --- |
| SQLite | Zero operação e ótimo para dev, mas sem `NUMERIC` de precisão arbitrária (`DECIMAL` é armazenado como texto ou float) e com concorrência de escrita serializada. Inviável para o núcleo financeiro |
| MySQL / MariaDB | Tem `DECIMAL` exato; `FOR UPDATE` disponível. Perde `UPDATE ... RETURNING`, que a rotação de sessão usa |
| Armazenamento em JSON/arquivo | Sem transação nem constraint. Discutido no material do curso e descartado |

### Evidências no repositório

```text
- migrations/                (11 pares up/down)
- src/repository.rs          · buy_asset (FOR UPDATE), rotate_session (UPDATE...RETURNING)
- docker-compose.yaml        (postgres:18, mount em /var/lib/postgresql)
- .github/workflows/ci.yml   (postgres:18 em service container)
```

### Quando reavaliar

Se o volume de dados ou o requisito de disponibilidade exigir réplica de leitura,
o modelo de instância única precisa ser revisto — e `holdings` materializado passa
a exigir cuidado com replicação atrasada.

---

## 11. Docker e Docker Compose

### Utilização no projeto

Dockerfile multi-stage (`rust:1.95-slim` → `debian:bookworm-slim`) e compose com
três perfis: `db` (padrão), `app` (opcional) e `observability` (opcional).

### Motivação técnica

**Motivo confirmado** para a separação de perfis: "o ciclo do dia a dia (editar
código, `cargo run`) e o ciclo de validar o artefato de produção (imagem Docker
completa) têm necessidades diferentes — o primeiro não deveria pagar o custo de
rebuildar a imagem a cada mudança de uma linha."

Decisões de segurança na imagem, todas verificáveis no Dockerfile:

- **Só o binário** é copiado para o runtime — nenhuma toolchain, nenhum
  código-fonte, nenhuma dependência de build.
- **Usuário sem privilégio** (`useradd --system --uid 10001 wallet`): um
  comprometimento não ganha root.
- **`SQLX_OFFLINE=true`**: o binário nasce sem nunca ter falado com um banco.
- **Suporte a CA extra** para ambientes com inspeção TLS, com o diretório ignorado
  pelo git por ser específico de cada máquina.

### Limitações

- **Imagem base `debian:bookworm-slim`** carrega mais superfície que
  `distroless`/`scratch`. Justificado: `reqwest` com `native-tls` precisa de
  `libssl3`, e o healthcheck usa `curl`.
- **Tag `latest` no coletor OTel** — build não reprodutível nesse serviço.
- **Segredos de desenvolvimento com valor padrão no compose**
  (`dev-admin-secret-change-me`). São claramente marcados, mas subir esse compose
  em ambiente exposto seria grave.
- **Sem `.dockerignore` para `docs/`**… na verdade `*.md` está ignorado, o que
  mantém o contexto enxuto, mas significa que o README **não** vai para a imagem.
- **Nenhum teste verifica que a imagem sobe e serve** — o CI só prova que ela
  compila.

### Alternativas consideradas

Build nativo sem container (perde reprodutibilidade); `distroless` (menor
superfície, mas exigiria `rustls` em vez de `native-tls` e outro mecanismo de
healthcheck); Nix (reprodutibilidade superior, curva e ferramental adicionais).

### Evidências no repositório

```text
- Dockerfile              (dois estágios, uid 10001, SQLX_OFFLINE)
- .dockerignore           (target/, .git/, .env, tools/, *.md)
- docker-compose.yaml     (perfis db/app/observability)
- docker/extra-ca/README.md
```

### Quando reavaliar

Ao migrar `reqwest` para `rustls`, `distroless` passa a ser viável e reduziria a
superfície da imagem.

---

## 12. Tabela-resumo

| Tecnologia | Versão | Responsabilidade | Motivo | Risco principal |
| --- | --- | --- | --- | --- |
| Rust | edition 2024 | Linguagem única | Confirmado (contexto do curso) | Curva de aprendizado para manutenção |
| axum | 0.8.9 | HTTP, extratores, camadas | Confirmado (extratores vs middleware global) | Quebras em versão menor |
| tokio | 1.52.3 | Runtime, jobs, sincronização | Inferido (exigido pela pilha) | Cancelamento de jobs não coberto |
| rust_decimal | 1.36 | Dinheiro exato | **Confirmado por incidente** | Teto de 28 dígitos significativos |
| sqlx | 0.9.0 | SQL verificado, migrações, testes | Confirmado (SQL explícito vs ORM) | Cache offline descolar do schema |
| PostgreSQL | 18 | Persistência | Parcialmente confirmado (`NUMERIC`) | Instância única, sem backup |
| askama | 0.16.0 | SSR verificado em compilação | Confirmado | Recompilar a cada mudança de HTML |
| htmx | 2.0.8 | Interatividade sem SPA | Confirmado (CSP + progressive enhancement) | Vendorado, sem auditoria automática |
| Tailwind CLI | 4.3.3 | CSS em build-time | Confirmado (CSP + determinismo) | Artefato versionado pode descolar |
| jwt-simple | 0.13.2 | JWT HS256 | Confirmado (`pure-rust` dispensa cmake) | `rsa` transitivo com advisory aberto |
| password-auth | 1.0.0 | Hash argon2 | Confirmado (negociação de algoritmo) | — |
| OpenTelemetry | 0.32 | Traces e métricas | Confirmado (opt-in sem overhead) | Versões acopladas entre 4 crates |

## Recomendações de formalização

O que deveria ganhar registro formal e ainda não tem:

1. **Fixar a versão do Tailwind CLI num arquivo** lido tanto pelo CI quanto pelo
   desenvolvedor, para eliminar `diff` espúrio de CSS.
2. **Revisão periódica de RUSTSEC-2023-0071** — hoje o `ignore` não tem data de
   reavaliação registrada.
3. **Registro de versão do htmx vendorado** num lugar verificável por
   ferramenta, já que o `cargo audit` não o alcança.

## Evidências

```text
- Cargo.toml, Cargo.lock
- .cargo/config.toml, .cargo/audit.toml
- Dockerfile, docker-compose.yaml, .dockerignore
- .github/workflows/ci.yml
- styles/app.css, static/htmx.js
- migrations/20260613000000_money_to_numeric.up.sql
- migrations/20260722000000_normalize_money_scales.up.sql
- docs/adr/0002-axum-em-vez-de-rocket.md
- docs/adr/0006-sqlx-com-checagem-em-compilacao.md
```
