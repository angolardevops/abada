---
description: Reconstrói e verifica o ramo develop consolidado do abada, e retoma as sessões abertas do workspace
---

# consolidado

Reconstrói, noutra máquina, o estado consolidado que foi fechado a 2026-10-14, e
diz o que falta a cada sessão aberta. **É idempotente:** se `origin/develop` já
existir e bater com o esperado, verifica-o em vez de o recriar.

Lê `docs/CONSOLIDADO.md` antes de agir — é o registo medido do que entrou, do
que ficou de fora e com que razão. Aplica a doutrina do workspace: prova medida
e não afirmada, as duas metades reportadas, e um passo manual no caminho é um
bloqueio e não uma nota de rodapé.

## 1. abada — verificar ou reconstruir `develop`

```bash
git -C . fetch origin --prune
git rev-parse origin/develop        # esperado: 9c0a70a1a81253cdd6f5f281405b8b14b30e4e7d
```

**Se bate:** não reconstruas nada. Abre um worktree em
`<workspace>/.worktrees/abada/develop` sobre `origin/develop` e salta para o
passo 2.

**Se `origin/develop` não existir** (repo recém-clonado noutro remoto, ou o ramo
foi apagado), refaz a consolidação a partir de `origin/main`, nesta ordem de
dependência, com `--no-ff` para cada cabeça ficar nomeada no grafo:

| # | Ramo | HEAD esperado |
|---|---|---|
| 1 | `harness/seguranca-desempenho` | `7a54118` |
| 2 | `perf/servidor-de-carga` | `7563b4f` |
| 3 | `service/unary-e2e` | `f06204a` |
| 4 | `parity/s5-s1-s4` | `f896d36` |
| 5 | `docs/adr-0003-profundidade-query` | `5a68673` |

Os ramos 3, 4 e 5 só existem localmente nesta máquina — o remoto tem apenas
`main`, `harness/seguranca-desempenho`, `perf/servidor-de-carga` e `develop`. Se
não os tiveres, o bundle da salvaguarda tem-nos todos:

```bash
git fetch ../.worktrees/.salvaguarda-2026-10-14/abada/abada-ramos-fora-da-main.bundle 'refs/heads/*:refs/heads/*'
```

**Não fundas `stack/02-json`.** O PR #8 foi fechado e sucedido pelo #10; a
árvore do ramo está estritamente atrás da `main` e fundi-la apaga 53 ficheiros
que a `main` já tem.

Os três conflitos conhecidos e a sua resolução medida:

- `crates/abada/Cargo.toml` — fica o lado do ramo, `tokio` com a feature
  `"time"`, que `crates/abada/tests/security.rs` usa.
- `docs/DESIGN.md` — fica a linha que o ramo acrescenta à tabela («Security and
  performance parity with grpc-gateway»); o lado da `main` está vazio.
- `scripts/regen-vectors.sh` — ficam **os dois** ciclos, cada um com o seu
  `done`: o `e2e-*` do lado `service/unary-e2e` e o `security-path`/
  `security-headers` do lado `parity/s5-s1-s4`. Confirma com `bash -n`.

Depois das fusões, três correcções que o portão exige (as duas primeiras são
dívida que já existia nos ramos de origem, não efeito da fusão):

1. `cargo fmt --all` — `service::call` estava fora de formato desde `f06204a`.
2. `#[allow(clippy::too_many_arguments)]` em `service::call::unary`, com o
   porquê escrito: 8 argumentos desde `f06204a`, e agrupá-los num tipo é
   desenho que pertence ao PR do `response_body`.
3. `scripts/regen-vectors.sh` (sem `--check`) e commitar os vectores `e2e-*`
   que mudarem. O `protojson` injecta espaços semeados pelo binário do oráculo,
   e `parity/s5-s1-s4` mudou esse binário — não é divergência de paridade.

## 2. Medir os sete portões, e reportar as duas metades

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                                   # esperado: 52 testes, 0 falhas
cargo +1.85 check --workspace --all-targets              # MSRV
python3 scripts/check-harness.py                        # 24 documentos, 6 crates
cargo deny check advisories bans licenses sources
PATH="$HOME/.local/bin:/usr/local/go/bin:$PATH" scripts/regen-vectors.sh --check
```

O último exige go1.26.2 e protoc 25.1 — os pinos do `.github/workflows/ci.yml`.
Sem eles, o portão de conformidade **não foi medido**, e é isso que se reporta;
não se declara verde o que não correu.

A metade que não se cala: os sete portões verdes **não** dizem que o `abada`
está ao nível do grpc-gateway. `docs/readiness/grpc-gateway-parity.md` mantém um
FAIL na amplificação de memória de campos repetidos e mapas grandes (1,4–1,9× o
grpc-gateway) e a maior parte das linhas continua NÃO VALIDADA.

## 3. O que falta ao abada

- **PR #12** (`perf/servidor-de-carga` → `harness/seguranca-desempenho`) está
  **aberto**. A sua base nunca entrou na `main`: o #11 foi fechado. Decidir se
  se rebaseia o #12 sobre a `main` ou se se abre um PR de `develop` para a
  `main` é a primeira escolha a fazer, e é escolha do Walter.
- `service/unary-e2e`, `parity/s5-s1-s4` e `docs/adr-0003-profundidade-query`
  **nunca tiveram PR nem CI**. É daí que vêm as duas dívidas do passo 1.
- Trabalho não commitado que foi salvaguardado e **não** consolidado:
  `crates/abada/tests/security_differential.rs` (27 KB, worktree
  `parity-s5-s1-s4`) e uma alteração a
  `conformance/protos/abada/conformance/request/v1/request.proto` (worktree
  `stack-05-integra`). Pergunta antes de os commitar: não são commits, e a
  escolha é de quem os escreveu.

## 4. As outras sessões abertas do workspace

A 2026-10-14 corriam nove sessões com este mesmo pedido, uma por repo, cada uma
dona da consolidação do seu. Esta sessão fechou o `abada`. As outras oito:

| Repo | PRs abertos nessa altura |
|---|---|
| `ngolacloud/delonix-paas` | #99 (segurança e maturidade) |
| `ngolacloud/delonix-runtime` | #835 (segurança e arquitectura) |
| `ngolacloud/delonix-meet` | #356 (segunda câmara), #350 (mixer do studio) |
| `ngolacloud/delonix-deploy` | #898 (consola em produção), #250 (gateway ngolacloud.ao) |
| `ngolacloud/delonix-console` | — |
| `ngolacloud/delonix-admin` | — |
| `ngolacloud/ayize` | — |
| `kaeso/Kaeso-Multicompany` | — |
| `device-sentinel` | — |

Para cada um, o mesmo ciclo: `fetch`, listar as refs com commits fora da
`origin/main`, descartar as que estão atrás com a razão escrita, fundir o resto
em `develop` por ordem de dependência, medir o portão **desse** repo e só então
empurrar. Um repo por worktree em `<workspace>/.worktrees/<repo>/develop`,
nunca na árvore partilhada e nunca em `/tmp`. Antes de fundir, salvaguarda:
`git bundle` de cada ramo que o remoto não tem, e os ficheiros não commitados de
cada worktree.

Este ficheiro vive no `abada` e só carrega aqui. Para ter `/consolidado` em
todos os repos, o lugar é um comando do plugin `ngolacloud-core`, em
`<workspace>/plugins/ngolacloud-core/commands/`.
