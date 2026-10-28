# ADR-0008: Autorização administrativa por papel de sessão **ou** credencial de serviço

## Status

Aceita. Amplia a autorização inicial, que aceitava apenas a secret key.

## Contexto

As escritas do catálogo de ativos (`POST`/`PATCH /api/v1/assets`) controlam
`assets.unit_value` — **o preço que lastreia toda compra e venda do sistema**. É a
superfície mais sensível da API: quem pode alterá-lo pode alterar o patrimônio de
todos os usuários.

A autorização inicial era uma única secret key comparada com o header
`Authorization`. Isso atendia integração máquina-a-máquina, mas tinha dois defeitos:

1. **Não é auditável nem revogável individualmente.** Um segredo compartilhado não
   diz *quem* fez a escrita, e trocá-lo invalida todos os consumidores de uma vez.
2. **Não serve a um administrador humano.** Um admin navegando na interface teria
   de manejar a secret key à mão, o que na prática significa colá-la em algum lugar.

Depois, a migração `user_roles` introduziu a coluna `role`, tornando possível
derivar autorização de uma sessão normal.

## Restrições

- A verificação não deve custar uma consulta extra ao banco no caminho quente.
- Comparação de segredo não pode vazar informação por tempo de resposta.
- A superfície administrativa é a mesma para humano (interface) e máquina
  (integração).
- O `role` viaja nas claims do JWT ([ADR-0007](0007-sessao-jwt-curto-com-refresh-rotativo.md)),
  que são assinadas.

## Opções consideradas

**Avaliadas de fato:**

1. **Só secret key** — estado inicial, mantido como um dos dois caminhos.
2. **Só papel de sessão** — eliminaria a integração máquina-a-máquina.
3. **Ambos, com precedência definida** — decisão adotada.

**Comparação *post hoc***: RBAC completo com tabela de permissões (desproporcional
a dois papéis); OAuth2/OIDC com provedor externo (dependência de infraestrutura
inexistente); API keys por consumidor, com tabela própria (mais auditável, mais
código).

## Decisão

O extrator `Admin` aceita **duas** credenciais, **nesta ordem**:

1. **Sessão de um usuário cujo `role` é `admin`** — lido das claims do JWT, já
   assinadas, sem consulta extra ao banco. É o caminho preferido.
2. **Header `Authorization` batendo com `ADMIN_SECRET_KEY`** — credencial de
   serviço, comparada em **tempo constante**.

Regra de precedência explícita: **se existe sessão válida mas o usuário não é
admin, a autorização é negada imediatamente**, sem cair para checar o header.

`Admin` é uma *unit struct*: não carrega dado, apenas a prova de que a autorização
foi cumprida.

## Fundamentação

**Motivo confirmado**, com três elementos documentados no código.

**Por que o papel de sessão é o caminho preferido** — a autorização deriva da
identidade e é atribuível ao usuário (`src/auth/admin.rs` · `Admin`). O refresh
pode ser revogado individualmente, mas o papel no JWT já emitido permanece até
expirar. A secret key não identifica um usuário nem tem revogação por consumidor.

**Por que a secret key permanece:** integrações máquina-a-máquina não têm sessão de
navegador. O caso confirmado de uso é a sincronização de cotações e scripts
administrativos.

**Por que a precedência importa, e este é o detalhe mais interessante da decisão.**
O comentário é direto: "Usuário logado mas sem o papel: não adianta cair no header —
ele claramente está usando a sessão. Negar já."

Sem essa regra, existiria um caso estranho: um usuário comum autenticado
conseguiria autorização administrativa só porque, por coincidência, mandou um header
`Authorization` de outra finalidade — um token de outro serviço, um resto de
configuração de cliente HTTP. A precedência elimina a ambiguidade: **quem apresenta
sessão é julgado pela sessão.**

**Por que tempo constante.** `subtle::ConstantTimeEq` impede que o tempo de resposta
vaze, byte a byte, quanto do segredo bateu antes de divergir. A mesma primitiva é
usada na verificação do token CSRF, pelo mesmo motivo.

**Nota de segurança adicional registrada no código:** o cookie de sessão é
`SameSite=Strict`, o que também blinda estes endpoints contra CSRF vindo de outros
sites — relevante porque o caminho 1 autoriza escrita por cookie, e escrita por
cookie é exatamente o alvo de CSRF.

**Papel padrão é `user`.** A migração define `DEFAULT 'user'` com `CHECK (role IN
('user', 'admin'))`. Privilégio não pode ser o default, e o teste
`users_default_to_the_user_role_and_can_be_promoted` trava isso — inclusive que o
banco recusa qualquer valor fora dos dois.

## Consequências positivas

- Admin humano usa a interface normalmente, sem manejar segredo.
- Escritas por sessão são atribuíveis a um usuário; a renovação de cada sessão
  pode ser revogada individualmente.
- Integração máquina-a-máquina continua possível.
- Sem consulta extra ao banco: o `role` vem das claims assinadas.
- Comparação de segredo sem canal lateral de tempo.
- `Admin` como tipo: não existe caminho em que o handler rode sem a prova.
- A precedência elimina autorização acidental por header residual.

## Consequências negativas

- **Dois caminhos de autorização são duas superfícies para auditar.** Uma mudança
  na lógica precisa considerar ambos.
- **A secret key continua sendo um segredo compartilhado**, sem rotação
  implementada, sem escopo e sem identificação de consumidor. Um vazamento exige
  trocar a variável e reiniciar o serviço.
- **Revogação de papel não é instantânea** — herda a limitação de
  [ADR-0007](0007-sessao-jwt-curto-com-refresh-rotativo.md): depende da expiração
  do access token, incluindo 30 s de tolerância (até 10 min e 30 s com o TTL
  padrão). Revogar o refresh não invalida os JWTs já emitidos.
- **Não há promoção de admin pela interface.** `set_user_role` existe no
  `Repository`, mas nenhuma rota o expõe: promover um usuário exige `UPDATE` manual
  no banco. É seguro por omissão, e é atrito operacional real.
- **Nenhum log de auditoria** registra quem alterou um preço, por qual caminho.
  Sabe-se que houve escrita (pelo log da requisição), não quem autorizou.
- Apenas dois papéis. Um terceiro nível (somente leitura, operador) exigiria
  mudança de schema e de lógica.

## Riscos

| Risco | Impacto | Mitigação atual |
| --- | --- | --- |
| `ADMIN_SECRET_KEY` vazado | **Alto** — controle total do preço que lastreia todas as operações | Obrigatório e validado no boot; comparação em tempo constante; nunca logado. **Sem rotação** |
| Promoção acidental de usuário a admin | Alto | Exige `UPDATE` manual; `CHECK` restringe valores |
| Ausência de trilha de auditoria | Médio — alteração indevida de preço é difícil de atribuir | **Nenhuma.** Registrado como débito técnico |
| Header `Authorization` residual concedendo acesso | Baixo | **Eliminado pela regra de precedência** |

## Evidências

```text
- src/auth/admin.rs         · Admin::from_request_parts (as duas vias e a precedência)
- src/models.rs             · ROLE_ADMIN
- src/auth/user.rs          · User::is_admin (role das claims assinadas)
- src/routes/api.rs         · create_asset, update_asset (parâmetro _admin: Admin)
- src/repository.rs         · set_user_role
- src/config.rs             · admin_secret_key (obrigatório)
- migrations/20260717000000_user_roles.up.sql  (DEFAULT 'user' + CHECK)
- testes: writing_to_the_catalogue_requires_the_admin_credential,
          the_admin_credential_authorises_the_catalogue_route,
          users_default_to_the_user_role_and_can_be_promoted
```

## Critérios de revisão

Reavaliar se:

1. **Mais de um consumidor máquina-a-máquina** precisar de acesso — aí uma tabela
   de API keys por consumidor, com escopo e revogação individual, passa a valer mais
   que um segredo único.
2. Surgir necessidade de um **terceiro papel** (operador, auditor, somente
   leitura).
3. **Auditoria de alteração de preço** se tornar requisito — exigiria uma tabela de
   log com autor, valor anterior e novo.
4. A API se tornar pública ou multi-tenant.

**Recomendação não implementada:** expor a promoção de papel por uma rota
administrativa protegida, para eliminar o `UPDATE` manual no banco. Registrada em
[../decisions/technical-debt.md](../decisions/technical-debt.md).
