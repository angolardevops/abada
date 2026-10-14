# Consolidado — o estado de `develop` a 2026-10-14

`develop` é o ramo onde está **tudo o que ainda não chegou à `main`** do `abada`,
medido contra `origin/main` (`e8b7323`, a fusão do #14) e não contra nenhuma
árvore local. É um ramo de integração: não substitui os PRs, dá-lhes uma base
comum já medida.

- `origin/develop` = `9c0a70a`
- 36 commits sobre `origin/main`: **28 de ramo** + 5 de fusão + 3 de correcção
  feitos aqui (ver «O que foi preciso corrigir»).

## O que entrou

Os cinco ramos formam uma pilha com uma raiz só, `harness/seguranca-desempenho`:

| Ramo | HEAD | Commits fora da `main` | PR | O que traz |
|---|---|---|---|---|
| `harness/seguranca-desempenho` | `7a54118` | 18 | #11 CLOSED, #13 merged para dentro dele | portões de paridade S1-S6/P1-P6 face ao grpc-gateway, corpus hostil, o limite de 100 níveis na query |
| `perf/servidor-de-carga` | `7563b4f` | 22 | **#12 OPEN** | servidor de carga P1-P5: backend comum, gateway de referência em Go, gateway abada em hyper, gerador em malha aberta e fechada |
| `service/unary-e2e` | `f06204a` | 24 | nenhum | `response_body` no escritor, cabeçalhos e trailers separados na chamada, oráculo e2e que devolve a resposta HTTP inteira |
| `parity/s5-s1-s4` | `f896d36` | 25 | nenhum | corpus hostil respondido pelo próprio grpc-gateway, S5 (campo repetido construído uma vez), S6 (`deny.toml` e `cargo deny` no CI) |
| `docs/adr-0003-profundidade-query` | `5a68673` | 19 | nenhum | ADR 0003, a profundidade máxima da query |

Ordem de fusão = ordem de dependência: `harness` → `servidor-de-carga` →
`unary-e2e` → `parity` → `adr-0003`. As fusões são `--no-ff`, para que cada
cabeça fique nomeada no grafo.

**Já contido, sem fusão própria:** `service/call-unary` (`d430fad`) é ancestral
de `harness/seguranca-desempenho`. **Já na `main`:** `stack/05-integra`,
`service/response-success`, `service/metadata-forwarding`,
`proto/tonic-unary-call` — zero commits à frente.

## O que ficou de fora, e porquê

`stack/02-json` (`e2dc6c0`) **não entrou**. O PR #8 foi fechado e sucedido pelo
#10, que fundiu. O único commit do ramo é uma fusão da `main` para dentro dele,
e a árvore do ramo está estritamente **atrás** da `main`: fundi-lo apagaria 53
ficheiros que a `main` já tem (`git diff origin/main stack/02-json` dá 851
inserções contra 5105 remoções). Está guardado no bundle da salvaguarda.

## O que foi preciso corrigir para o portão fechar

Três commits `develop:` que não vêm de nenhum ramo. **Dois são dívida que já
existia nos ramos de origem** — nenhum deles é efeito da consolidação:

1. `fc00514` — `cargo fmt --all --check` falhava no `matches!` dos cabeçalhos
   reservados de `service::call`. Medido: falha também em `service/unary-e2e` e
   em `parity/s5-s1-s4`.
2. `9537682` — `clippy::too_many_arguments` em `service::call::unary`, que
   `f06204a` levou a 8 argumentos. Fica um `#[allow]` com o porquê escrito;
   agrupar os argumentos num tipo é desenho e pertence ao PR que fechar o
   `response_body`, não a esta consolidação.
3. `9c0a70a` — os vectores `e2e-*` regenerados. **Este é efeito da fusão, mas
   não é um erro dela:** o `protojson` do protobuf-go injecta espaços de
   propósito (`internal/detrand`), semeados pelo binário do oráculo.
   `parity/s5-s1-s4` acrescentou `conformance/oracle/mem.go` e tocou
   `conformance/oracle/main.go`, o binário do módulo mudou, e o espaçamento do
   corpo e2e mudou com ele. Os vectores aqui são a resposta do grpc-gateway
   v2.27.3 construído desta árvore, com os pinos do CI (go1.26.2, protoc 25.1).

### Fragilidade que fica nomeada e não resolvida

Qualquer mudança futura no módulo `conformance/oracle` volta a reembaralhar o
espaçamento e a tornar os vectores `e2e-*` obsoletos **sem que nada de semântico
mude**. O `scripts/regen-vectors.sh --check` vai acusar isso como se fosse uma
divergência de paridade. A réplica em Rust não se engana — compara o JSON
normalizado, não os bytes — mas o portão do CI engana-se.

## Portões medidos nesta árvore, a 2026-10-14

| Portão | Comando | Resultado |
|---|---|---|
| formato | `cargo fmt --all --check` | PASS |
| lint | `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| testes | `cargo test --workspace` | PASS — 52 testes, 0 falhas |
| MSRV | `cargo +1.85 check --workspace --all-targets` | PASS |
| harness | `python3 scripts/check-harness.py` | PASS — 24 documentos, 6 crates |
| cadeia de fornecimento | `cargo deny check advisories bans licenses sources` | PASS |
| conformidade | `scripts/regen-vectors.sh --check` | PASS — «vectors match grpc-gateway v2.27.3» |

Os seis primeiros correram antes e depois do commit dos vectores. O último
correu com go1.26.2 e protoc 25.1 — os pinos do `.github/workflows/ci.yml`.

**O que isto NÃO prova.** Que o `abada` está ao nível do grpc-gateway: não
está. O relatório de paridade (`readiness/grpc-gateway-parity.md`) mantém um
FAIL na amplificação de memória de campos repetidos e mapas grandes (1,4–1,9×
o grpc-gateway) e a maior parte das linhas continua NÃO VALIDADA. A
consolidação juntou a medição; não mudou o que ela diz.

## Trabalho não commitado que foi salvaguardado, não consolidado

Em `../.worktrees/.salvaguarda-2026-10-14/abada/`:

- `abada-ramos-fora-da-main.bundle` — os seis ramos, `stack/02-json` incluído.
- `untracked-parity-security_differential.rs` — `crates/abada/tests/security_differential.rs`
  (27 KB), ficheiro novo e nunca commitado no worktree `parity-s5-s1-s4`.
- `dirty-stack-05-integra.patch` — alteração não commitada a
  `conformance/protos/abada/conformance/request/v1/request.proto` no worktree
  `stack-05-integra`.

Os dois últimos **não entraram em `develop`**: não são commits, e decidir se
entram é escolha de quem os escreveu.

## Como se reconstrói isto noutra máquina

`/consolidado` — o comando está em `../.claude/commands/consolidado.md` e é
idempotente: se `origin/develop` já existir, verifica-o em vez de o recriar.
