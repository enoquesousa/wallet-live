# Autenticação e autorização na interface HTTP

## Objetivo

Descrever, do ponto de vista de quem **consome** a interface HTTP, como autenticar,
como a sessão se mantém viva, como autorizar as operações administrativas e o que
esperar em cada falha.

## Escopo

Coberto: o contrato observável — cookies, headers, códigos de resposta, sequência de
verificações. Não coberto: o desenho interno da sessão (ver
[ADR-0007](../adr/0007-sessao-jwt-curto-com-refresh-rotativo.md)), o modelo de
autorização (ver [ADR-0008](../adr/0008-autorizacao-por-papel-e-credencial-de-servico.md))
e a análise de risco (ver [../security/threat-model.md](../security/threat-model.md)).

---

## 1. Os três mecanismos, e quando cada um se aplica

| Mecanismo | Superfície | Transporte | Consumidor |
| --- | --- | --- | --- |
| **Sessão por cookie** | Interface HTML e escritas de admin | Cookies `token` + `refresh_token` | Navegador |
| **Credencial de serviço** | Só escritas de `/api/*/assets` | Header `Authorization` | Integração máquina-a-máquina |
| **Token CSRF** | Só formulários HTML | Campo `csrf_token` + cookie `csrf` | Navegador |

Não há `Bearer`, OAuth2, OIDC, chave de API por consumidor nem MFA. A ausência é
deliberada para o escopo atual e está registrada em
[../decisions/known-limitations.md](../decisions/known-limitations.md).

## 2. Sessão por cookie

### 2.1 Obter uma sessão

```http
POST /login
Content-Type: application/x-www-form-urlencoded

username=alice&password=<senha>&csrf_token=<token do formulário>
```

O `csrf_token` precisa ser lido do formulário renderizado por `GET /login` — o
mesmo valor está no cookie `csrf`, e os dois têm de bater.

Resposta de sucesso: `303` para `/`, que encaminha o usuário autenticado a
`/assets`, com dois `Set-Cookie`.

| Cookie | Conteúdo | `Max-Age` |
| --- | --- | --- |
| `token` | JWT HS256 com `id`, `username`, `role` | `SESSION_TTL_MINUTES` (10 min) |
| `refresh_token` | 32 bytes aleatórios, opacos | `REFRESH_TTL_DAYS` (14 dias) |

Ambos: `HttpOnly`, `SameSite=Strict`, `Path=/`, e `Secure` se `COOKIE_SECURE=true`.

O JWT tem TTL nominal de 10 minutos por padrão. `User::from_auth_token` admite
**30 segundos de tolerância de relógio**, explicitamente; uma cópia do JWT pode
ser aceita até `exp + 30 s`. O `Max-Age` do cookie continua no TTL nominal.

### 2.2 A ordem das verificações no login

Importa, e não é a ordem intuitiva:

1. **CSRF** — divergente ou ausente ⇒ `403`.
2. **Lockout** — em bloqueio ⇒ `429`.
3. **Senha** — inválida ⇒ mensagem genérica.

O lockout roda **antes** da verificação de senha. Durante o bloqueio, **nem a senha
correta passa** — é isso que retira o lucro de um ataque de força bruta, que de
outra forma poderia usar o tempo de resposta ou o sucesso eventual como sinal.

### 2.3 Parâmetros do lockout

| Parâmetro | Valor | Constante |
| --- | --- | --- |
| Tentativas livres | 5 | `FREE_ATTEMPTS` |
| Primeiro bloqueio | 30 s | `BASE_LOCK` |
| Progressão | Dobra a cada falha | — |
| Teto | 15 min | `MAX_LOCK` |
| Perdão por inatividade | 1 h sem novas falhas | `FORGET_AFTER` |

Contagem **por usuário**, com o nome normalizado (`trim().to_lowercase()`): `ALICE`,
`  alice  ` e `Alice` são o mesmo alvo. Login correto zera o contador.

> **Limitação relevante para quem opera:** o estado vive **em memória do processo**.
> Reiniciar o serviço zera todos os bloqueios, e com múltiplas réplicas o lockout
> passa a ser por instância. Ver **DT-01** em
> [../decisions/technical-debt.md](../decisions/technical-debt.md).

### 2.4 Renovação transparente

Não há endpoint de refresh. O middleware pode renovar nas demais rotas, mas
**não renova em `/login`, `/register` ou `/logout`**, em nenhum método. Essas
rotas controlam os próprios cookies: a renovação da conta anterior não pode
sobrescrever o login/cadastro nem recriar uma sessão durante o logout.

| Estado | Comportamento |
| --- | --- |
| `token` válido | Nada acontece — a validação não toca o banco |
| `token` ausente, inválido ou expirado além da tolerância + `refresh_token` válido | Rotação automática; dois `Set-Cookie` novos na resposta; a requisição prossegue normalmente |
| `token` sem validade + refresh inválido/revogado/expirado | Segue sem sessão ⇒ a rota privada redireciona ao login |

**Rotação a cada uso:** cada renovação queima o token anterior e emite um novo. Um
refresh token roubado para de funcionar assim que o usuário legítimo renovar.

Consequência observável para o cliente: **duas abas renovando ao mesmo tempo** — uma
vence a reivindicação e a outra é mandada ao login. É o comportamento correto do
ponto de vista de segurança, e pode surpreender.

### 2.5 Encerrar a sessão

```http
GET /logout
```

Remove os cookies apresentados e tenta revogar a linha do refresh recebido em
`sessions` (`revoked_at`), sem renová-lo antes. Quando a escrita no banco tem
sucesso, esse refresh e suas cópias deixam de renovar imediatamente. Se o banco
falhar, o logout local continua, mas a revogação no servidor não é garantida.

O JWT de acesso é **stateless**: uma cópia já emitida continua aceita até
`exp + 30 s` (até 10 min e 30 s desde a emissão, com o TTL padrão), inclusive
depois do logout ou de um rebaixamento de papel. Revogar o refresh impede novas
renovações; não consulta nem invalida cada JWT em circulação.

O contrato `GET /logout` é mantido. A rota não exige token CSRF; migrar essa
operação para POST com CSRF exige ajustar também os links da interface.

### 2.6 Respostas sem sessão válida

| Origem | Resposta |
| --- | --- |
| Navegação normal | `303` para `/login` |
| Requisição com `HX-Request` | Header **`HX-Redirect`** — o navegador inteiro é redirecionado |

O segundo caso existe porque renderizar a página de login dentro de um fragmento da
carteira seria pior que um erro.

## 3. Proteção CSRF nos formulários

Padrão *double-submit cookie*. O servidor gera um token aleatório de 32 bytes,
grava-o no cookie `csrf` **e** embute o mesmo valor num campo oculto do formulário
renderizado. No `POST`, os dois têm de bater, com comparação em **tempo constante**.

Um site malicioso consegue fazer o navegador da vítima **enviar** os cookies, mas não
consegue **ler** o cookie para preencher o campo — então a requisição forjada falha.

| Situação | Resposta |
| --- | --- |
| Campo `csrf_token` ausente do corpo | `422` (o extrator de formulário rejeita) |
| Token divergente ou vazio | `403` ⇒ na interface, `303` com banner |
| Cookie `csrf` ausente | `403` — **ausência é recusa, não permissão** |

**O token não rotaciona por página.** A segunda chamada de `ensure_csrf_token`
devolve o mesmo valor: rotacionar faria duas abas abertas invalidarem uma à outra.

Rotas que exigem CSRF: `POST /login`, `/register`, `/deposit`, `/buy`, `/sell`,
`/quotes/sync`.

> **A API JSON não exige CSRF** porque não é chamada por formulário de navegador. A
> proteção equivalente vem do `SameSite=Strict` do cookie de sessão, que impede outro
> site de usar a sessão da vítima.

Verificação importante do teste `forms_without_a_matching_csrf_token_are_refused`:
ele confere não só o status, mas que **o saldo continua zerado**. Conferir apenas o
status deixaria passar um refactor que redireciona corretamente e credita o depósito
de qualquer forma.

## 4. Autorização administrativa

Duas credenciais aceitas para `POST`/`PATCH /api/*/assets`, **nesta ordem**:

### Caminho 1 — sessão com papel `admin`

Preferido. A autorização deriva da identidade e é atribuível a
um usuário. O `role` vem das claims do JWT, já assinadas — **sem consulta extra ao
banco**.

### Caminho 2 — credencial de serviço

```http
POST /api/v1/assets
Authorization: <ADMIN_SECRET_KEY>
Content-Type: application/json
```

O valor é o conteúdo **cru** da variável `ADMIN_SECRET_KEY` — **não** é `Bearer
<token>`, não é Basic, não há prefixo. Comparação em tempo constante.

### A regra de precedência

**Se existe sessão válida mas o usuário não é admin, a autorização é negada
imediatamente** — o header `Authorization` nem chega a ser consultado.

Sem essa regra, um usuário comum autenticado poderia ganhar acesso administrativo
por acidente, apenas por enviar um header `Authorization` de outra finalidade (um
token de outro serviço, um resto de configuração de cliente HTTP). A regra elimina a
ambiguidade: **quem apresenta sessão é julgado pela sessão.**

### Como promover um usuário a admin

**Não há rota para isso.** O método `set_user_role` existe no `Repository`, mas
nenhum endpoint o expõe. A promoção exige `UPDATE` manual:

```sql
UPDATE users SET role = 'admin' WHERE username = 'alice';
```

Papel padrão é `user`, e o schema restringe os valores a `'user'`/`'admin'` por
`CHECK`. É seguro por omissão, e é atrito operacional real — registrado como débito
técnico.

## 5. Códigos de resposta relacionados a autenticação

| Código | Significado | Origem |
| --- | --- | --- |
| `400` | Header `Authorization` ausente onde é exigido | `MissingAuthorization` |
| `401` | Credencial inválida, token fabricado/expirado, sessão sem papel admin | `InvalidCredentials`, `Jwt` |
| `403` | Token CSRF ausente ou divergente | `CsrfMismatch` |
| `422` | Campo `csrf_token` ausente do corpo do formulário | Extrator do axum |
| `429` | Lockout de login ativo | `TooManyAttempts` |

Catálogo completo em [errors.md](errors.md).

## 6. Requisitos de cadastro

| Campo | Regra |
| --- | --- |
| `username` | 3 a 32 caracteres (contagem por *chars*, não bytes) |
| `password` | 8 a 128 caracteres |

O **limite superior também importa**: um campo sem teto é vetor de negação de serviço
no cálculo da hash argon2, que é deliberadamente custoso.

O `username` é trimado na entrada e é `UNIQUE` no banco. Colisão é traduzida para
`UsernameTaken` ⇒ `400` — não vira `500` genérico.

A senha nunca é armazenada em texto: só a hash argon2, gerada por `password-auth`. A
hash inclui o algoritmo usado, o que permite migrar de algoritmo sem migração de
dados.

## 7. O que **não** existe

Registrado explicitamente para que a ausência não seja confundida com omissão de
documentação:

| Recurso | Estado |
| --- | --- |
| Autenticação multifator | Não implementado |
| Recuperação de senha | Não implementado — **não há como redefinir uma senha esquecida** |
| Troca de senha pelo usuário | Não implementado |
| Confirmação de e-mail | Não implementado — não há campo de e-mail |
| Bloqueio/desativação de conta | Não implementado |
| Listagem das sessões ativas | Não implementado (a tabela existe; não há rota) |
| Revogação de outras sessões | Não implementado |
| Rotação de `JWT_SECRET` | Não implementado — trocar invalida JWTs antigos, mas refresh válido pode renovar; revogação global exige também revogar `sessions` |
| Rotação de `ADMIN_SECRET_KEY` | Não implementado — trocar exige reiniciar |
| Auditoria de acesso administrativo | Não implementado |
| Limpeza de sessões expiradas | Não implementado — a tabela cresce indefinidamente |

## 8. Evidências

```text
- src/auth/user.rs       · User::auth_token, from_auth_token, UnauthenticatedUser::authenticate,
                           register, valid_registration, TOKEN_COOKIE
- src/auth/session.rs    · refresh_session, RefreshToken, access_cookie, refresh_cookie
- src/auth/admin.rs      · Admin::from_request_parts (as duas vias e a precedência)
- src/auth/csrf.rs       · ensure_csrf_token, verify_csrf
- src/auth/throttle.rs   · LoginThrottle, FREE_ATTEMPTS, BASE_LOCK, MAX_LOCK, FORGET_AFTER
- src/repository.rs      · rotate_session, revoke_session, create_session, set_user_role
- src/routes/frontend.rs · login, register, logout, authenticate_form
- migrations/20260716000001_create_sessions.up.sql
- migrations/20260717000000_user_roles.up.sql
- testes: forms_without_a_matching_csrf_token_are_refused,
          protected_requests_rotate_missing_or_expired_access_tokens,
          logout_never_renews_a_missing_or_expired_access_token,
          login_and_registration_do_not_restore_the_previous_account,
          authentication_pages_and_rejected_forms_do_not_rotate_refresh_tokens,
          logout_revokes_refresh_but_access_lasts_until_its_expiry,
          private_screens_send_anonymous_visitors_to_the_login,
          an_expired_session_redirects_the_whole_browser_not_just_the_fragment,
          writing_to_the_catalogue_requires_the_admin_credential,
          registering_starts_a_session_that_opens_the_wallet,
          session_rotation_returns_the_user_and_burns_the_old_token
```
