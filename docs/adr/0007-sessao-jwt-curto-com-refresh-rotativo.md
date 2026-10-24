# ADR-0007: JWT de acesso curto + refresh token opaco rotativo

## Status

Aceita. Substitui a sessão inicial de JWT único e longo, sem revogação.

## Contexto

A sessão inicial era um JWT assinado em cookie, com validade longa — uma
simplificação adequada a um protótipo, mas não a um fluxo de sessão real.

O problema estrutural de um JWT puro é que ele **não é revogável**. A validação não
consulta o banco (é isso que o torna rápido), e a consequência é que um token
roubado permanece válido até expirar. Isso deixa três requisitos sem resposta:

1. **Logout de verdade.** Apagar o cookie do navegador não invalida o token — quem
   o copiou continua entrando.
2. **Reação a comprometimento.** Não há como cortar uma sessão específica.
3. **Sessão longa sem token longo.** Reduzir a validade do JWT resolve a revogação
   na prática (a janela fica curta), mas obriga o usuário a fazer login a cada 10
   minutos.

## Restrições

- A validação do caminho quente **não deve tocar o banco**: cada requisição
  autenticada validando sessão no Postgres seria uma consulta a mais em toda a
  navegação.
- Cookies são o transporte (interface SSR, não SPA com header `Authorization`).
- Um vazamento do banco não deve entregar credenciais utilizáveis.
- O sistema é uma instância única, sem armazenamento de sessão compartilhado.
- `jwt-simple` compilado com `pure-rust` para dispensar BoringSSL/cmake no Windows.

## Opções consideradas

**Avaliadas de fato:**

1. **JWT único e longo, sem revogação** — estado inicial, **revogado**.
2. **Sessão puramente opaca**, com toda validação no banco.
3. **JWT de acesso curto + refresh token opaco rotativo** — decisão adotada.

**Comparação *post hoc***: refresh token sem rotação (revogável, mas token roubado
vale até expirar); JWT com lista de revogação em memória (não sobrevive a restart).

## Decisão

**Dois tokens, dois cookies, dois propósitos:**

| | Access token | Refresh token |
| --- | --- | --- |
| Cookie | `token` | `refresh_token` |
| Formato | JWT HS256 com claims (`id`, `username`, `role`) | 32 bytes aleatórios do SO, **opaco** |
| Validade | `SESSION_TTL_MINUTES` (padrão 10 min) | `REFRESH_TTL_DAYS` (padrão 14 dias) |
| Validação | Assinatura e claims temporais, com tolerância explícita de 30 s — **não toca o banco** | Consulta `sessions` pela hash |
| No banco | Nada | **Só a hash SHA-256** |
| Revogável | Não, até expirar | **Sim** |

Ambos: `HttpOnly`, `SameSite=Strict`, `Secure` conforme `COOKIE_SECURE`, `Path=/`,
`Max-Age` alinhado ao TTL correspondente.

O prazo de aceitação do JWT pode chegar a `exp + 30 s`, ou 10 min e 30 s desde
a emissão com o TTL padrão. A tolerância é definida em `User::from_auth_token`;
não depende do padrão de 15 minutos de `jwt-simple` 0.12.16.

Renovação transparente por middleware, com **rotação a cada uso**.

## Fundamentação

**Motivo confirmado** — a mensagem da migração `create_sessions` enuncia a decisão:

> "O access token curto (JWT) fica stateless, mas o refresh token longo é um valor
> aleatório opaco cuja hash SHA-256 vive aqui. Isso nos dá o que um JWT puro não
> pode: revogação real (logout mata a linha) e rotação (cada refresh queima o token
> antigo e emite um novo, então um token roubado para de funcionar no momento em
> que o usuário legítimo renova). Só a hash é guardada: um vazamento do banco não
> vaza tokens utilizáveis."

Nesse comentário histórico, "token roubado" se refere ao **refresh antigo**.
Uma cópia de um JWT já emitido continua aceita até expirar com a tolerância de
relógio. A rotação tampouco identifica qual concorrente é o legítimo: ela aceita
o primeiro uso do refresh e rejeita os seguintes, sem revogar uma família inteira.

**A peça central é o `UPDATE ... RETURNING` atômico.** `rotate_session` executa,
numa transação:

```sql
UPDATE sessions SET revoked_at = NOW()
WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > NOW()
RETURNING user_id
```

seguido de um `INSERT` da nova sessão. O `UPDATE ... RETURNING` **reivindica** a
sessão numa operação atômica: se um token roubado e o legítimo tentarem rotacionar
ao mesmo tempo, o segundo a chegar encontra a sessão já revogada (`revoked_at IS
NULL` não bate mais) e recebe `None`. **Não há janela de corrida em que os dois
consigam rotacionar.**

**Por que SHA-256 e não argon2** no refresh token: o token é 32 bytes de
aleatoriedade do SO, não uma senha escolhida por humano. Não há ataque de
dicionário a mitigar, e o custo de argon2 seria pago em **cada renovação de sessão**
sem ganho de segurança.

**Por que o middleware roda antes dos extratores.** `refresh_session` é aplicado
antes do handler porque o extrator `User` lê o cookie: sem essa ordem, o handler
veria sessão expirada mesmo com refresh válido em mãos. O `User` renovado vai nas
`extensions` da requisição, de onde o extrator o recupera — o handler nem fica
sabendo que houve renovação.

**Exceção: `/login`, `/register` e `/logout`, em todos os métodos.** Essas rotas
não renovam a sessão anterior. Seus handlers criam ou removem os cookies; deixar
o middleware anexar cookies de renovação depois deles poderia restaurar a conta
anterior ou deixar uma nova sessão ativa no logout.

**Falha de renovação não interrompe a requisição.** Sessão inexistente, revogada,
expirada, erro de banco ou falha ao assinar o novo JWT: todos caem no fluxo normal,
e o extrator produz o 401 ou o redirecionamento habitual. É deliberado — um erro de
renovação não deve virar 500 quando o caminho correto é pedir login.

**Logout tenta revogar o refresh no servidor**, marcando `revoked_at`, e remove
os cookies. Uma falha no banco não impede a remoção local, mas também não garante
a revogação remota. JWTs de acesso já emitidos não são revogados por essa escrita.

## Consequências positivas

- Logout bem-sucedido no banco impede novas renovações com o refresh recebido.
- O refresh antigo deixa de funcionar depois da primeira rotação bem-sucedida.
- Vazamento do banco não entrega tokens utilizáveis — só hashes.
- Validação do caminho quente não toca o banco.
- Sessão de 14 dias sem token de 14 dias.
- Renovação invisível ao usuário e ao handler.
- Testes de repositório cobrem rotação, revogação, expiração e token fabricado;
  contratos HTTP cobrem também cookies, troca de conta e a ordem dos middlewares.

## Consequências negativas

- **Revogação de privilégio não é instantânea.** O `role` viaja nas claims
  assinadas: um novo token lê o papel atual, mas os JWTs vigentes mantêm o papel
  até `exp + 30 s` (até 10 min e 30 s com o TTL padrão). Revogar o refresh não
  antecipa esse limite.
- **HS256 é simétrico:** o mesmo segredo assina e valida. Múltiplos serviços
  validando compartilhariam a capacidade de emitir. Adequado a um serviço único;
  RS256/EdDSA seria necessário num cenário distribuído.
- **`jwt-simple` traz `rsa` transitivamente** (via `superboring`), com
  **RUSTSEC-2023-0071** (Marvin Attack) **sem correção upstream**. Ignorado em
  `.cargo/audit.toml` com justificativa verificável: a aplicação usa exclusivamente
  HS256, então o código RSA nunca é exercitado. A revisão de dependências de
  2026-10-24 atualiza `jwt-simple` para 0.13.2, mas mantém `rsa` 0.9.10 na árvore
  e preserva essa exceção. A política completa, incluindo a exceção de `rkyv`
  opcional na CI, está em [gestão de dependências](../development/dependency-management.md).
- **`jwt_simple::Error` não implementa `std::error::Error`** (é `anyhow::Error` por
  baixo), o que impede `#[from]`/`transparent` no `thiserror` e obriga a um `impl
  From` manual guardando só a mensagem — perde-se a cadeia de causa.
- **Sessões revogadas e expiradas nunca são removidas** da tabela: `sessions` cresce
  indefinidamente. Não há job de limpeza.
- **Rotação a cada uso significa escrita no banco** a cada renovação — uma
  transação a cada 10 minutos por sessão ativa.
- Duas abas renovando simultaneamente: uma vence a reivindicação e a outra é
  mandada ao login. Comportamento correto do ponto de vista de segurança, mas pode
  surpreender.

## Riscos

| Risco | Impacto | Mitigação atual |
| --- | --- | --- |
| `JWT_SECRET` vazado | **Crítico** — permite forjar qualquer sessão, inclusive admin | Segredo obrigatório validado no boot; nunca logado. **Sem rotação de chave implementada** |
| `sessions` crescendo sem limite | Médio — degradação lenta | **Nenhuma.** Registrado como débito técnico |
| Privilégio revogado ainda válido no JWT emitido | Médio | TTL nominal de 10 min mais 30 s de tolerância por padrão; revogar o refresh impede novas renovações, sem invalidar o JWT |
| RUSTSEC-2023-0071 | Baixo neste uso | `ignore` documentado; **sem data de reavaliação** |
| `COOKIE_SECURE=false` em produção | **Alto** — cookies em HTTP claro | Padrão inseguro para dev local; comparação literal com `"true"` é uma armadilha (ver DT-04) |

## Evidências

```text
- migrations/20260716000001_create_sessions.up.sql   (a decisão, comentada)
- src/auth/session.rs   · RefreshToken::generate, hash_token, access_cookie,
                          refresh_cookie, session_expiry, refresh_session
- src/auth/user.rs      · auth_token, from_auth_token, UserClaims, TOKEN_COOKIE
- src/repository.rs     · rotate_session (UPDATE...RETURNING), revoke_session,
                          create_session
- src/routes/frontend.rs · logout
- src/config.rs         · session_ttl_minutes, refresh_ttl_days
- .cargo/audit.toml     (RUSTSEC-2023-0071 com justificativa)
- testes: session_rotation_returns_the_user_and_burns_the_old_token,
          revoked_session_cannot_rotate,
          expired_session_cannot_rotate,
          unknown_token_cannot_rotate,
          protected_requests_rotate_missing_or_expired_access_tokens,
          logout_never_renews_a_missing_or_expired_access_token,
          login_and_registration_do_not_restore_the_previous_account,
          authentication_pages_and_rejected_forms_do_not_rotate_refresh_tokens,
          logout_revokes_refresh_but_access_lasts_until_its_expiry,
          an_expired_session_redirects_the_whole_browser_not_just_the_fragment
```

## Critérios de revisão

Reavaliar se:

1. O sistema ganhar **mais de uma instância** ou um segundo serviço que precise
   **validar** tokens sem poder **emiti-los** — aí HS256 deixa de servir e a
   migração para RS256/EdDSA é obrigatória.
2. A tabela `sessions` crescer a ponto de afetar desempenho — precisa de job de
   limpeza antes disso.
3. Houver correção upstream para RUSTSEC-2023-0071, ou se algum algoritmo RSA
   passar a ser usado (**neste caso, reavaliar imediatamente**).
4. Revogação instantânea de privilégio se tornar requisito — exigiria consultar o
   `role` no banco em vez de lê-lo das claims, ao custo de uma consulta por
   requisição.
