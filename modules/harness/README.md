# metteur-harness

System-prompt assembly for Metteur agents: the prompt texts, the fragment
model, and the rules that keep the rendered prefix stable enough for provider
prompt caching.

## Layout

| Path | Contents |
|---|---|
| `src/facts.rs` | `EnvFacts` — the runtime facts the prompt renders (workspace, date, model) |
| `src/sections.rs` | Embedded `prompts/*.md` texts, their priorities, and the scope prefix |
| `src/environment.rs` | The environment block, split into a stable half and a volatile half |
| `src/project.rs` | Workspace instruction files (`METTEUR.md`, `AGENTS.md`) |
| `src/assembly.rs` | `HarnessPrompt::{fragments_for, apply, refresh, compress_fragment}` |
| `src/prompts/*.md` | The prompt texts themselves |
| `tests/` | Assembly behaviour and the prefix-cache contract |

The crate has no clocks, no environment lookups and no I/O policy: callers pass
`EnvFacts` in. That is what makes assembly a pure function and lets the tests
run without a daemon.

## Fragment ordering

Fragments render highest priority first. Priorities are grouped by how often a
section can change, because a change invalidates the provider prefix cache from
that point on:

| Priority | Fragment | Changes |
|---|---|---|
| 100 | `harness.identity` | never (compiled in) |
| 90 | `harness.tools` | never |
| 80 | `harness.coding` | never |
| 70 | `harness.plan` | never |
| 65 | `harness.verify` | never |
| 60 | `harness.progress` | never |
| 50 | `harness.project` | when the workspace instruction file changes |
| 40 | `harness.env.base` | when the workspace or platform changes |
| 39 | `harness.env.date` | once a day, or when the model changes |
| 20 | node instruction | every call |
| 10 | `harness.append` | when the operator edits the config |

The static sections plus the tool definitions clear the 1024-token minimum that
OpenAI-family caches require for any hit at all. Anthropic's explicit
breakpoints are placed by the provider (tools, then system, then the
conversation tail).

## Writing standard

Prompt text is code. It is reviewed, versioned and tested like code, and it
follows these rules (checked by `tests/stability.rs` where it can be):

1. **Every rule must be decidable.** "Read before you write" is; "mind code
   quality" is not, and does not belong in a prompt.
2. **One claim per bullet**, 5–12 words for tactical rules. Delete clauses that
   do not change behaviour.
3. **Keyword conventions**: `MUST` / `REQUIRED`, `NEVER`, `SHOULD` /
   `RECOMMENDED`, `AVOID`, `MAY` / `OPTIONAL`. They are defined once in
   `identity.md`; do not use bold for emphasis in their place.
4. **No ornamental tags.** Tag-looking text is treated as a contract by models,
   so only documented tags are allowed: `<environment>`, `<project-instructions>`.
5. **Every prohibition carries an alternative.** "NEVER X; do Y instead".
6. **No contradictory MUSTs.** A strong instruction follower spends reasoning
   reconciling conflicts instead of working.
7. **No token-budget talk.** "Be efficient with tokens" causes premature
   abandonment; the engine bounds the run, not the prompt.
8. **Critical rules appear at both ends** of a long prompt; recall degrades in
   the middle.
9. **No implementation details** — no retry, cache or concurrency internals.
10. **Every rule traces to an observed failure.** Record which failure in a
    comment next to the rule; a rule with no failure behind it should be
    deleted.

## Changing a prompt

Editing a `prompts/*.md` file invalidates the stored prefix of running sessions
once, by design. After a change:

1. Run `cargo test -p metteur-harness` — the assembly and stability tests must
   pass unchanged.
2. Re-check the static section sizes: an empty or gutted section fails the
   writing-standard test.
3. Watch the cache-hit metric (`cached_input_tokens / input_tokens`) on the next
   real session; a drop that persists across turns means the prefix is moving.
