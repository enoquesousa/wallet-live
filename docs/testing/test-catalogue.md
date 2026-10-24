# Catálogo de testes

## Objetivo

Inventário **caso a caso** dos testes: o que cada um trava e por que existe.
Um teste cujo motivo não está escrito em algum lugar é um teste que alguém apaga
no primeiro refactor em que ele incomoda.

## Escopo

Coberto: todos os testes automatizados existentes, agrupados por arquivo, com o
risco que cada um reduz. Não coberto: por que a suíte tem dois níveis, escolha de
dublês, execução e limites conhecidos — isso fica em
[test-strategy.md](test-strategy.md). Para a leitura por risco em vez de por
arquivo, ver [test-matrix.md](test-matrix.md).

## Como ler as tabelas

Cada linha nomeia o teste, o invariante que ele trava e o motivo pelo qual foi
escrito. Onde a coluna "Por que existe" está vazia, o teste é um caso de borda
óbvio do teste imediatamente acima e o motivo é o mesmo.

Para rodar um teste isolado:

```bash
cargo test <nome_do_teste>
```

---

## 1. Cobertura por fase entregue

As fases são as de [../decisions/roadmap.md](../decisions/roadmap.md). Cada fase
entregue tem testes, e eles nem sempre moram onde a fase foi implementada — o que importa é que exista uma asserção travando a decisão.

| Fase | Onde estão os testes |
|---|---|
| 1 — Fundações e integridade de dados | `repository` (26, contra Postgres real), `quotes` (2), `payload_quotes` (5), `services::portfolio` (7) |
| 2 — Segurança de sessão | `auth::csrf` (3), `auth::throttle` (4), `auth::user` (1), `repository` (rotação/revogação/expiração), `http_web` (CSRF, gate de sessão, cabeçalhos), `http_session` (5: renovação, logout, troca de conta e expiração) |
| 3 — Arquitetura e API | `routes::api` (4), `http_api` (8, inclusive autorização e alias `/api`), `services::portfolio` (paginação) |
| 4 — Operação | `http_web` (sondas separadas, cache de asset estático), `routes::frontend` (CSP sem inline) |
| 5 — Produto e UX | `routes::frontend` (18: render, i18n, htmx, CSV, máscara), `routes::flash` (3), `i18n` (4), `market` (11), `payload_market` (7), `http_web` (fluxo de dinheiro ponta a ponta) |

---

## 2. Testes de unidade

### `src/repository.rs` — 26 testes, contra Postgres efêmero

O núcleo financeiro. Banco de verdade porque as garantias que importam aqui são
do banco: transação, `CHECK`, `UNIQUE`, `NUMERIC`.

| Teste | O que trava | Por que existe |
|---|---|---|
| `deposit_credits_balance_and_logs_transaction` | Depósito credita o saldo **e** grava no extrato | As duas escritas são uma transação; uma sem a outra é livro-razão furado |
| `deposit_rejects_non_positive_amounts` | Zero e negativo recusados | Depósito negativo é saque disfarçado |
| `deposits_and_trades_reject_excessive_scale` | Escala acima do canônico recusada antes do banco; zeros à direita não contam | Fronteira de precisão — `1.010 == 1.01` não pode virar erro |
| `legacy_high_scale_money_still_renders_the_wallet` | Valores de escala 28 plantados no banco continuam **legíveis** | Regressão do incidente de 2026-07-22: `1/taxa` sem arredondar gerou `NUMERIC(28)`, e produtos/somas estouravam o `Decimal` na leitura — `/assets` respondia 500 para qualquer conta com posições. O `ROUND` nas queries é o que sustenta isto |
| `buy_debits_balance_and_opens_holding` | Compra debita e abre posição | O caminho felizes da operação central |
| `buy_rejects_when_balance_is_insufficient` | Recusa **e reverte tudo**: saldo intacto, nenhuma posição | Uma compra parcialmente aplicada é pior que uma recusada |
| `buy_rejects_unknown_asset` | Ativo inexistente não é comprável | |
| `buying_more_averages_the_cost_basis` | `(2×10 + 2×20) / 4 = 15` | Custo médio ponderado é a base do lucro/prejuízo exibido |
| `selling_everything_closes_the_position` | Venda total devolve o caixa e fecha a posição | Posição zerada tem de sair da carteira, não ficar com quantidade 0 |
| `partial_sell_keeps_remaining_units` | Venda parcial **não altera** o custo médio do que sobrou | Recalcular custo médio na venda inventaria lucro |
| `sell_rejects_more_than_owned` | Não vende o que não tem — nem com posição nenhuma | Venda a descoberto não existe neste produto |
| `unquoted_assets_cannot_be_bought_or_sold_for_free` | Ativo que perdeu cotação não negocia por zero | Catálogo pode ter ativo sem preço; negociar a zero seria transferência de valor |
| `trades_that_round_to_zero_do_not_move_cash_or_holdings` | Operação cujo total arredonda a zero não move nada | Senão viraria um moedor de unidades grátis |
| `transactions_paginate_newest_first_without_gaps` | Página 1 traz 3→2, página 2 traz a mais antiga, sem repetir nem pular | Ordem instável em paginação some com linhas |
| `portfolio_snapshots_capture_cash_plus_holdings` | Snapshot = caixa + posições ao preço do momento; ordem do gráfico do mais antigo ao mais novo; o limite corta pelos mais recentes | É a série do gráfico de patrimônio |
| `admin_prices_are_capped_at_the_canonical_scale` | Preço vindo do admin também é travado na escala | A borda administrativa é uma porta de entrada como qualquer outra |
| `asset_creation_rejects_invalid_input` | Nome vazio/em branco e preço negativo recusados antes do banco | Preço negativo inverteria a matemática da carteira |
| `asset_update_rejects_invalid_input` | Mesma validação na correção, e o ativo fica **intocado** | |
| `asset_name_is_trimmed_on_write` | `"  bitcoin  "` grava como `bitcoin` | Espaço invisível cria ativo duplicado |
| `update_known_asset_prices_matches_by_normalized_name` | `"Bitcoin"` casa com a chave `bitcoin` | A sincronização de cotações depende dessa normalização |
| `market_bootstrap_inserts_once_and_respects_existing_aliases` | Catálogo que já usa o alias `BTC` não ganha uma segunda linha `bitcoin` | A instalação demonstrável cria o catálogo mínimo sem atropelar o que o admin cadastrou |
| `users_default_to_the_user_role_and_can_be_promoted` | Papel padrão é `user`; fora de `user`/`admin` o banco recusa | Privilégio não pode ser o default |
| `session_rotation_returns_the_user_and_burns_the_old_token` | Rotação devolve a identidade **e queima** o token anterior | Sem queimar, um refresh roubado vale para sempre |
| `revoked_session_cannot_rotate` | Logout revoga de verdade | |
| `expired_session_cannot_rotate` | Sessão nascida expirada não rotaciona | |
| `unknown_token_cannot_rotate` | Token fabricado não vira sessão | |

### `src/auth/` — 8 testes

| Teste | O que trava | Por que existe |
|---|---|---|
| `csrf::ensure_generates_and_then_reuses_the_token` | A segunda chamada devolve o **mesmo** token | Rotacionar por página faria duas abas abertas invalidarem uma à outra |
| `csrf::verify_accepts_matching_token_and_rejects_the_rest` | Casa o par cookie/campo; sem cookie, nada passa | O *double-submit* só vale se a ausência também for recusa |
| `csrf::tokens_are_unpredictable` | Tokens não se repetem nem seguem padrão | Token previsível é o mesmo que token nenhum |
| `throttle::allows_attempts_below_the_threshold` | Abaixo do limite, login normal funciona | Defesa que atrapalha o usuário legítimo é removida na primeira reclamação |
| `throttle::locks_after_too_many_failures` | Bloqueia por usuário; caixa do nome não escapa (`  ALICE  `); outros usuários não são afetados | Normalizar o nome é o que impede burlar o lockout mudando maiúsculas |
| `throttle::success_clears_the_counter` | Acerto limpa o contador | Senão o usuário acumularia bloqueio ao longo do dia |
| `throttle::lock_duration_backs_off_exponentially_with_a_cap` | Backoff exponencial com teto de 15 min | Sem teto, um erro de digitação viraria bloqueio de horas |
| `user::registration_requires_a_reasonable_username_and_password` | Username 3–32, senha 8–128 | Limite superior também importa: campo sem teto é vetor de negação de serviço no hash |

### `src/market.rs` — 11 testes

| Teste | O que trava | Por que existe |
|---|---|---|
| `coin_normaliza_simbolo_e_trava_a_escala` | Ticker em maiúsculas; escala travada na entrada; as três janelas de variação chegam juntas | A fonte manda número JSON (`f64`); a escala tem de morrer na fronteira |
| `variacao_ausente_vira_zero_e_a_moeda_permanece` | Campo ausente vira zero e a moeda **fica** na lista | Uma linha útil pelo preço é melhor que uma linha a menos |
| `moeda_sem_preco_ou_com_preco_invalido_e_descartada` | Sem preço, com `NaN` ou `inf` → descartada | Preço é o único campo indispensável |
| `medidor_da_faixa_de_24h_fica_dentro_do_viewbox` | Marcador em coordenada de SVG, com `clamp` nos extremos e omissão sem faixa | A CSP fecha `style-src`: não existe `style="width:63%"`. E a fonte publica preço fora da faixa às vezes |
| `janela_de_24h_e_o_rabo_da_serie_semanal` | 24 h = últimas 25 amostras; série curta não estoura o slice; `from_tag` valida | Trocar de janela não pode custar chamada externa |
| `grafico_projeta_a_serie_no_viewbox_com_eixo_do_tempo` | Caminho exato do `<path>`, preenchimento fechado na base, 5 marcas de hora | Compara a string do SVG: é a única forma de travar geometria |
| `janela_semanal_e_rotulada_por_data` | 7 d rotula por dia/mês, não por hora | O que muda entre as pontas de uma semana é o dia |
| `serie_constante_vira_linha_reta_e_serie_curta_nao_vira_grafico` | Série constante → reta no meio (sem divisão por zero); 1 ponto e `NaN` → sem gráfico | Divisão por zero aqui viraria `NaN` no atributo do SVG |
| `grafico_sem_horario_de_coleta_sai_sem_eixo` | Sem âncora de tempo, sai sem eixo — mas com a linha | Melhor sem rótulo que com rótulo inventado |
| `selecao_cai_na_primeira_do_ranking_quando_o_id_nao_existe` | Id desconhecido → primeira do ranking; snapshot vazio → `None` | Link velho ou parâmetro digitado à mão não pode esvaziar a tela |
| `busca_casa_ticker_e_nome` | "btc" e "bitcoin" chegam no mesmo lugar | |

### `src/quotes.rs` — 2 testes

| Teste | O que trava | Por que existe |
|---|---|---|
| `brl_price_inverts_the_rate_and_rejects_the_uninvertible` | `1 / 0,2 = 5`; taxa zero e moeda desconhecida → `None` | O preço em BRL é o **inverso** da taxa BRL→moeda; inverter errado dobra ou divide o patrimônio de todos |
| `brl_price_caps_the_scale_of_non_terminating_inversions` | `1/3` não escapa com 28 casas | O arredondamento não é cosmético: foi a causa raiz do 500 em `/assets` |

### `src/services/portfolio.rs` — 7 testes

Orquestração sem banco, com um dublê de `Repository`: a matemática financeira já
tem cobertura contra o Postgres real: aqui o alvo é a montagem da view.

| Teste | O que trava | Por que existe |
|---|---|---|
| `equity_chart_scales_the_series_into_the_viewbox` | Caminho exato da linha, do preenchimento e do marcador; +50% de 100 para 150 | Mesma geometria do gráfico de mercado, mesmo motivo de travar a string |
| `equity_chart_handles_flat_and_short_series` | Série constante → reta neutra; série que cai → vermelho com percentual negativo; menos de 2 pontos → sem gráfico | Carteira nova tem série de um ponto |
| `next_page_math_covers_the_edges` | 25 de 27 → tem próxima; página cheia exata → não inventa página vazia; extrato vazio | Paginação erra nas bordas, não no meio |
| `wallet_view_assembles_repository_data_and_paginates` | A view junta saldo, posições e extrato; catálogo pode ter ativo sem cotação, mas a **compra só oferece os negociáveis** | Oferecer um ativo sem preço levaria o usuário a um erro que ele não causou |
| `deposit_result_flows_through_unchanged` | O serviço não reinterpreta o resultado | |
| `buy_error_flows_through_unchanged` | Nem o erro | Camada de serviço que "melhora" o erro do repositório esconde a causa |
| `sell_error_flows_through_unchanged` | Idem na venda | |

### `src/routes/frontend.rs` — 18 testes

Renderização: o HTML é comparado como texto, porque é como texto que ele chega
ao navegador.

| Teste | O que trava | Por que existe |
|---|---|---|
| `pages_carry_no_inline_style_or_script` | **Nenhuma** página emite `<style>` ou `<script>` sem `src` | A CSP fecha `script-src`/`style-src` em `'self'`. Um bloco inline passaria em revisão e o navegador o ignoraria em produção — bug visual difícil de rastrear |
| `static_assets_revalidate_by_content_and_answer_304_when_unchanged` | `ETag` de conteúdo, `no-cache`, 304 no retorno, etiqueta fraca e lista tratadas | A URL é fixa e o conteúdo muda a cada build: com cache cego, o rebuild deixa HTML novo com CSS velho. Foi o que empilhou o painel de mercado |
| `the_wallet_fragment_is_partial_html_embedded_by_the_full_page` | O fragmento é exatamente o alvo do swap, sem esqueleto; a página completa embute o **mesmo** fragmento | É o que sustenta "operar sem recarregar mantendo SSR" com um único template |
| `unauthenticated_pages_redirect_the_whole_browser_for_classic_and_htmx_requests` | Sem sessão, redireciona nos dois caminhos | Página de login dentro de um pedaço da carteira é pior que um erro |
| `htmx_marks_partial_requests_but_history_restore_wants_the_full_page` | `HX-Request` → fragmento; `HX-History-Restore-Request` → página inteira | Voltar/avançar com o cache expirado precisa reconstruir o DOM do zero |
| `the_wallet_page_renders_in_both_languages` | A carteira sai em pt-BR e em inglês | |
| `the_login_page_renders_in_both_languages` | O login também | Tela de entrada é a que mais vê visitante de fora |
| `language_switch_only_follows_local_absolute_paths` | Protocolo-relativo, URL absoluta e lixo caem no fallback `/` | Sem isso a troca de idioma seria um **open redirect** |
| `csv_export_formats_the_statement_in_ptbr_conventions` | Ponto e vírgula, decimal com vírgula, aspas internas dobradas, tipo traduzido | É o separador que o Excel em pt-BR entende como coluna |
| `deposit_amount_field_is_a_plain_number_input_hooked_for_the_mask` | O campo é `<input type="number">` de verdade, com o gancho `data-money-input` | A máscara é **aditiva**: sem JS o campo continua funcionando |
| `money_input_script_loads_after_htmx_with_defer` | `money-input.js` vem depois do htmx, os dois com `defer` | Ele usa `htmx.onLoad` para reanexar a máscara após cada troca; ordem invertida = máscara morta |
| `market_dashboard_marks_direction_with_arrow_and_sign_not_only_colour` | Toda variação sai com seta **e** sinal; `R$ 0,00004125` não vira `R$ 0,00`; as classes de cor existem no CSS compilado | Verde e vermelho medem ΔE ~4,6 sob deuteranopia — indistinguíveis para ~8% dos homens. Cor é o canal redundante, nunca o único |
| `market_dashboard_shows_the_selected_coin_with_its_time_series` | Agregados compactos, faixa do dia, marcador em `x="335.4"`, eixo do tempo, as regiões que o htmx troca | O painel inteiro numa asserção: se um pedaço sair, o teste diz qual |
| `market_dashboard_selects_by_id_and_keeps_the_state_in_every_link` | `aria-current` na linha em foco; todo link carrega o estado; id desconhecido cai na primeira | Selecionar moeda é um link comum: funciona sem JavaScript |
| `market_search_filters_the_side_list_without_losing_the_selection` | A busca filtra a lista e **mantém** o painel; o formulário dispara para o caminho nu | Se a busca mandasse query na URL, os parâmetros duplicariam e o extrator recusaria |
| `market_dashboard_shows_a_status_message_before_the_first_round` | Antes da primeira rodada: `role="status"`, sem lista, e a tela inteira se reconstrói | A tela existe antes de haver dado |
| `market_urls_percent_encode_free_text` | `&`, `=` e espaço viram `%XX`; o termo chega normalizado e limitado a 32 | Texto livre do usuário virando link é onde a query se parte em dois |
| `compact_scale_keeps_big_aggregates_readable` | `R$ 6,51 tri`, arredondamento antes da formatação, zero → `—`, sufixo traduzido | `R$ 0,00` mentiria parecendo medição; "não publicado" é um traço |

### `src/i18n.rs` — 4 · `src/routes/flash.rs` — 3 · `src/routes/api.rs` — 4

| Teste | O que trava | Por que existe |
|---|---|---|
| `i18n::tags_resolve_ignoring_case_and_region` | `PT-br`, `pt-BR`, `pt` resolvem igual | Navegador manda de tudo |
| `i18n::accept_language_takes_the_first_supported_entry` | Pega o primeiro **suportado**, pulando os que não temos | `fr-FR,fr;q=0.9,pt-BR;q=0.8` tem de virar pt-BR, não erro |
| `i18n::explicit_cookie_beats_browser_preference_and_default_is_ptbr` | Escolha explícita ganha do navegador; o padrão é pt-BR | Quem clicou "en" não quer voltar para pt na próxima página |
| `i18n::transaction_kinds_localize_with_a_fallback` | Tipo de transação traduzido, com fallback | Tipo novo no banco não pode virar tela em branco |
| `flash::flash_roundtrips_through_the_cookie_including_accents` | Acento sobrevive ao cookie; a mensagem é de **uso único** | Flash que não se apaga reaparece em toda navegação |
| `flash::success_kind_survives_the_roundtrip` | Sucesso não volta como erro | |
| `flash::business_errors_become_messages_and_internal_errors_do_not` | Credencial errada e usuário inexistente dão a **mesma** mensagem, nos dois idiomas; erro interno segue para o 500 | Mensagens diferentes vazariam quais contas existem |
| `api::openapi_spec_covers_the_asset_routes` | A spec cobre as rotas e os três verbos | Spec gerada do código é documentação que não pode mentir |
| `api::test_create_asset` / `test_list_assets` / `test_update_asset` | Escrita, leitura e correção no banco, com snapshot `insta` do JSON | Travam o **formato** da resposta: um campo renomeado quebra o consumidor |

---

## 3. Testes de contrato

### `tests/payload_quotes.rs` — 5 testes

Payload real da Coinbase. A integração mais perigosa do projeto, porque alimenta
o preço que **lastreia compra, venda e saldo**.

| Teste | O que trava | Por que existe |
|---|---|---|
| `the_real_payload_decodes_all_rates_including_the_ones_that_exceed_the_mantissa` | As 636 taxas decodificam num único `from_str`, inclusive a de 41 dígitos; todos os pares de `MARKET_PAIRS` estão presentes | Se o `Decimal` recusasse uma taxa em vez de arredondar, a sincronização **inteira** morreria — e o sintoma seria "os preços pararam", sem nada apontando a causa |
| `inverting_real_rates_never_leaks_a_scale_the_database_cannot_take_back` | Nenhum preço invertido passa de `MONEY_SCALE` | É a taxa **real** que produz a dízima: `1/0,195043…` não termina |
| `the_inversion_direction_is_right_and_not_silently_flipped` | Dólar em faixa plausível; bitcoin ordens de grandeza acima | Se a inversão fosse esquecida num refactor, o dólar apareceria como 0,19 |
| `a_pair_missing_from_the_response_is_skipped_without_taking_the_others_down` | Par removido não inventa preço; os outros seguem; taxa ≤ 0 → `None` | A saída de uma moeda da API não pode congelar o catálogo |
| `a_malformed_payload_becomes_a_typed_error_not_a_panic` | Vazio, HTML de erro, sem envelope, truncado → erro tipado | Cotação atrasada não derruba o serviço; pânico derruba |

### `tests/payload_market.rs` — 7 testes

Payload real da CoinGecko.

| Teste | O que trava | Por que existe |
|---|---|---|
| `the_real_payload_decodes_into_the_coins_the_screen_consumes` | As 4 moedas decodificam, ordenadas pelo **nosso** critério de ranking | Depender da ordem do JSON deixaria a lista à mercê de um parâmetro na URL |
| `every_decimal_from_the_payload_arrives_with_its_scale_capped` | Preço/máxima/mínima ≤ `MONEY_SCALE`; variação e agregado ≤ 2 casas | Escala de 28 casas escapando desta fronteira já derrubou o resumo da carteira |
| `the_payload_carries_the_time_series_that_makes_the_chart_free` | A série vem no mesmo pedido; janela de 24 h ⊆ 7 d; caminho válido sem `NaN`; 5 marcas, hora vs data | Se a fonte parar de mandar `sparkline`, a promessa de "trocar de moeda é grátis" vira mentira em silêncio |
| `the_trading_range_marker_stays_inside_the_viewbox_for_real_data` | Marcador dentro de `[10, 590]` para toda moeda real | A fonte apura preço, mínima e máxima em momentos diferentes — preço fora da faixa acontece |
| `missing_fields_degrade_to_neutral_instead_of_dropping_the_coin` | Campo ausente vira zero representável; nenhuma moeda com preço é descartada | Moeda recém-listada sem série existe no payload real, não num fixture que a gente lembrou de escrever |
| `search_matches_ticker_and_name_on_real_coins` | Busca por ticker, por nome e por termo parcial, sobre dado real | É onde "Tether" vs "USDT" e maiúsculas importam |
| `a_malformed_payload_becomes_a_typed_error_not_a_panic` | Corpo inválido → erro; `[]` é válido e vira snapshot vazio | O snapshot vazio é o que faz a tela mostrar "buscando as cotações" em vez de quebrar |

### `tests/http_api.rs` — 8 testes

Requisições HTTP reais pelo router de produção. Os testes de unidade da API
chamam os handlers direto, passando o `Admin` como valor construído à mão — a
**autorização nunca é exercida**. Esta é a lacuna.

| Teste | O que trava | Por que existe |
|---|---|---|
| `writing_to_the_catalogue_requires_the_admin_credential` | Sem credencial e com credencial errada → 4xx; com a certa → 200 | É a única coisa entre um estranho e o preço que lastreia todas as operações |
| `the_catalogue_round_trips_through_real_http_requests` | Criar → listar → corrigir; **dinheiro sai como string JSON** | Um `f64` no meio do caminho é exatamente o que o projeto evita de ponta a ponta |
| `invalid_payloads_are_rejected_at_the_edge_as_client_errors` | Nome vazio/em branco e preço negativo → 4xx, e a mensagem **não** é "internal server error" | 500 aqui significaria validação no banco em vez da borda, e o cliente levaria a culpa pelo erro dele |
| `malformed_json_bodies_never_reach_the_handler` | JSON quebrado, campo ausente, tipo trocado → 4xx | |
| `patching_an_unknown_asset_is_a_404` | Não é 500 nem 200 silencioso | 200 para um id inexistente faria o operador achar que corrigiu |
| `the_unversioned_alias_serves_the_same_thing_as_v1` | `/api` e `/api/v1` devolvem byte a byte o mesmo | Versionamento só vale se o alias não divergir com o tempo |
| `the_openapi_spec_is_served_and_describes_the_real_routes` | A spec é servida e é JSON válido, com `openapi` e `paths` | Spec malformada é pior que nenhuma: um gerador de cliente a consome sem perguntar |
| `every_api_response_carries_the_security_headers` | CSP sem `unsafe-inline`, `nosniff`, `DENY`, `no-referrer`, `no-store` — no sucesso **e** no erro | É a camada mais externa do router; só passando pela pilha se prova que não foi contornada |

### `tests/http_session.rs` — 5 testes

Exercitam o router de produção, seus cookies e o Postgres real. Os JWTs expirados
são assinados com a chave de teste e timestamps no passado, sem esperas. As
asserções leem **todos** os `Set-Cookie`; ler só o primeiro esconderia credenciais
que o middleware anexasse depois do handler.

| Teste | O que trava | Por que existe |
|---|---|---|
| `protected_requests_rotate_missing_or_expired_access_tokens` | Access ausente ou expirado há 60 s + refresh válido → um único par de cookies, identidade correta, token antigo revogado e replay recusado | Preserva renovação das telas privadas e impede a antiga tolerância implícita de 15 min no JWT |
| `logout_never_renews_a_missing_or_expired_access_token` | GET logout só remove credenciais; nenhuma nova linha de sessão, nenhum refresh ativo e replay recusado | Renovar antes do logout deixava uma sessão nova ativa e anexava cookies depois da remoção |
| `login_and_registration_do_not_restore_the_previous_account` | Login/cadastro de outra conta emite um único par, abre a identidade correta e não rotaciona a conta anterior | Cookies tardios do middleware podiam sobrescrever os do novo login/cadastro |
| `authentication_pages_and_rejected_forms_do_not_rotate_refresh_tokens` | GET dos formulários, senha incorreta, cadastro duplicado e CSRF inválido não consomem o refresh nem emitem credenciais | Os caminhos de erro também atravessam o middleware e não devem criar sessões escondidas |
| `logout_revokes_refresh_but_access_lasts_until_its_expiry` | Refresh revogado não renova; uma cópia do access válido ainda entra; access expirado há 60 s é recusado | Documenta e trava o limite real da arquitetura stateless, incluindo a tolerância explícita de 30 s |

### `tests/http_web.rs` — 15 testes

O produto pelo caminho do navegador. Entre o `repository` (aritmética) e os
testes de template (HTML) fica tudo o que faz o produto ser produto — cookie,
CSRF, redirecionamento, banner — e nada disso era exercitado montado.

| Teste | O que trava | Por que existe |
|---|---|---|
| `private_screens_send_anonymous_visitors_to_the_login` | `/assets`, `/market`, `/transactions.csv`, `/deposit` → 303 para `/login` | O gate de autenticação, em todas as telas com dado privado |
| `an_expired_session_redirects_the_whole_browser_not_just_the_fragment` | Requisição htmx recebe `HX-Redirect` | Login renderizado dentro de um pedaço da carteira é pior que um erro |
| `registering_starts_a_session_that_opens_the_wallet` | Registro → sessão → carteira | Ciclo cookie → JWT → tela, e pré-requisito dos testes abaixo |
| `forms_without_a_matching_csrf_token_are_refused` | Campo ausente → 422 no extrator; token forjado/vazio → 303 com banner; **e o saldo continua zerado** | Conferir só o status deixaria passar um refactor que redireciona bonito e credita o depósito de qualquer forma |
| `the_money_flow_works_end_to_end_through_the_browser_path` | Depositar → comprar → vender, cada passo com CSRF renovado, saldo aparecendo na tela | O caminho que o usuário percorre, com as camadas todas no meio |
| `a_business_error_comes_back_as_a_banner_not_a_500` | Comprar sem saldo → 303, nunca 5xx | É a diferença entre "você não tem saldo" e "erro interno do servidor" |
| `the_market_screen_degrades_gracefully_before_the_first_refresh` | Snapshot vazio → `role="status"` e o contêiner que se reconstrói | A tela responde antes de existir dado |
| `the_market_screen_accepts_any_state_in_the_query_string` | 7 combinações, incluindo moeda inexistente, período inválido e `%26%3D` → sempre 200 | Parâmetro digitado à mão nunca pode virar 500 |
| `htmx_requests_get_a_fragment_and_plain_navigation_gets_the_full_page` | Com `HX-Request`, sem `<!DOCTYPE>`; sem ele, página completa com CSS | A base de "operar sem recarregar mantendo SSR" |
| `static_assets_revalidate_instead_of_being_cached_blind` | Os três assets: `ETag`, `no-cache`, e 304 **com corpo vazio** | Mesmo motivo do teste de unidade, agora pela pilha real |
| `the_language_switch_is_not_an_open_redirect` | `?next=` só aceita caminho local absoluto | Um link `/lang/pt-BR?next=https://site-falso` levaria o usuário para fora com um clique que parece do produto |
| `the_interface_answers_in_both_languages` | `lang="pt-BR"` e `lang="en"`, e os HTML diferem | |
| `liveness_and_readiness_are_separate_probes` | `/healthz`, `/readyz`, `/health` respondem | Liveness não toca o banco: reiniciar o app não conserta um Postgres fora do ar |
| `the_statement_exports_as_an_authenticated_csv_download` | Autenticado, com `Content-Disposition: attachment` e separador `;` | Sem o cabeçalho o navegador renderiza o CSV como texto na tela |
| `the_admin_credential_authorises_the_catalogue_route` | A credencial do painel é a mesma da API | |
