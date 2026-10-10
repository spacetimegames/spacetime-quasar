# Spacetime: Quasar - agent manual

Desktop-first 3D space trading-and-combat sandbox in Rust, no game engine, all
content original and procedural. Status: Phase 0 bootstrap, nothing playable yet.
Not related to SpacetimeDB; never use that name in code or docs naming.

Start at [docs/index.adoc](docs/index.adoc). Detail lives in `docs/`; this file
is the short version. Where a rule has an ADR, the ADR wins.

## Where to look

- [docs/index.adoc](docs/index.adoc): map of all documentation.
- [docs/adr/index.adoc](docs/adr/index.adoc): binding ADRs. 0001 toolchain/MSRV/Nix,
  0002 dependencies, 0003 CI, 0004 build resources for parallel Builders.
- [docs/decisions/index.adoc](docs/decisions/index.adoc): product decisions of record (PD-xx).
- [docs/60-contributing.adoc](docs/60-contributing.adoc): git flow, Definition of Done,
  verification tags, PR conventions, docs rules.
- Code context: use graft (see the block at the bottom of this file) before grepping.

## Open questions: never invent answers

Design questions are answered by the product owner or by an ADR. If you meet one,
stop and escalate. Decisions of record and ADRs are not re-litigated in code;
propose a superseding ADR instead.

## Hard rules

- No engine (no Bevy, Godot, Unreal, `three-d`, `rg3d`/Fyrox). Compose mature crates
  (wgpu, winit, Rapier/Parry, serde/ron, ...). Own PBR renderer on wgpu, WGSL only.
  No ray-tracing pipelines; ray queries optional, feature-gated, native-only.
- No mature crate for a need: write a small reusable crate `crates/st-*` with stable
  API, tests, docs, no game types in public signatures, no panics on misuse. The
  binary in `apps/` is thin; algorithms belong in crates.
- Never hardcode data: designs, economy, factions, items, layouts live in `assets/**/*.ron`.
- Fully procedural assets. Nothing downloaded at build or run time. A non-procedural
  file (e.g. a font) is vendored in-repo under a compatible licence, by ADR.
- Toolchain: exact pin in `rust-toolchain.toml`, no nightly; MSRV and Nix per
  [ADR 0001](docs/adr/0001-toolchain-msrv-nix.adoc).
- Dependencies per [ADR 0002](docs/adr/0002-dependency-policy.adoc): permissive licences
  only; a new dependency needs Architect sign-off, a non-trivial one an ADR. Every
  crate has `publish = false`.
- Every crate compiles for x86_64 Linux, x86_64 Windows MSVC, aarch64 macOS and
  wasm32-unknown-unknown. Native-only code is `cfg`-gated inside `st-platform` and `apps/`.
- Determinism: the sim is bit-identical across platforms (state hash in CI; renderer
  exempt). Software libm in sim math, no implicit FMA, no unordered parallel or SIMD
  reductions in sim code.
- Multiplayer-ready, no netcode: the player is not special (N pilots fed by input
  sources); the sim advances only through commands (seed + per-tick command log);
  `st-sim` stays headless; saves separate universe state from pilot state.
- VR- and browser-ready: N-view camera; HUD renderable in world space; every post
  effect toggleable; threads and fs only through `st-platform`; async-capable
  loading; non-blocking main loop.
- GPU-driven: no CPU-side geometry transform, skinning, particle simulation or
  lighting loop. Physically based materials only.
- Performance target: 60 fps at 1080p on an RTX 4060 / RX 7600 class GPU (8 GB), with a
  Low tier at reduced internal resolution.
- Runtime: no network, no telemetry, no LLM. LLMs are development-time only and their
  output is committed as reviewed data.

## Git flow

Details in [docs/60-contributing.adoc](docs/60-contributing.adoc).

- `main` is protected: PR only, required CI green. Never push to `main`.
- One branch per Subtask, `SPAC-<n>-<slug>`, in its own git worktree outside the
  repo. Build resources per [ADR 0004](docs/adr/0004-build-resources.adoc).
- Commit messages and PR titles start with `SPAC-<n>: `.
- PR body lists the commands run, results and verification levels.
- Squash-merge, then delete the branch and remove the worktree.
- A Reviewer's approval is recorded as a PR comment.

## Quality gates (required on every PR)

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

These three were run by an agent inside `nix develop` on the reference dev machine
(untagged, passing). `cargo deny check` and the MSRV job are also required but are not
wired up yet, so they have not been run. CI enforcement is defined by
[ADR 0003](docs/adr/0003-ci-design.adoc) and is being set up in Phase 0; do not assume it
runs yet. From Phase 1: cross-platform determinism hash. From Phase 2: renderer checks
on lavapipe with golden images (updating a golden needs Reviewer approval).

## Definition of Done (Subtask)

1. Acceptance criteria met and verified by the Subtask's test command.
2. fmt, clippy and tests pass locally; CI green on all targets.
3. Docs updated: rustdoc, `docs/` pages, an ADR if a decision was made,
   `docs/handoff/state.adoc` if status changed.
4. Reviewer's "approve" verdict recorded on the PR.
5. PR squash-merged; branch and worktree removed.
6. Issue commented with PR link, CI result and verdict, then moved to Done.

## Docs rules

- Docs are AsciiDoc (`.adoc`). Markdown only where a tool requires it: `AGENTS.md`,
  rustdoc, a crate `README.md` at crates.io publication.
- Every crate has a `//!` doc (purpose, boundary, non-goals, compiling example). Every
  public item has rustdoc; doc tests are mandatory for public APIs. Non-obvious
  algorithms get a "why" comment.
- An ADR (context, decision, alternatives, consequences) is written before the change
  it governs. Start from `docs/adr/0000-template.adoc` and add a row to the index.
- Docs describe only commands actually run, and tag every command with its
  verification level: `[ci-built]`, `[ci-tested]`, `[human]`, or no tag (agent run on
  the reference dev machine). Never claim a higher level than achieved.

## Public repo hygiene

Never commit usernames, home paths, hostnames, mount points, network details,
issue-tracker site identifiers or personal remarks. GPU/CPU model and OS family are
fine in benchmark and verification records. Use repo-relative paths or
`<workspace-root>`. Jira keys `SPAC-n` are fine.

## Working today

`nix develop` gives the pinned toolchain. Docs check:
`nix shell nixpkgs#asciidoctor -c asciidoctor --failure-level WARN -o /dev/null <file>.adoc`.

<!-- graft:start -->
## Graft — repo context graph

This repo is indexed in `graft/`: small linked markdown nodes that explain each
system and carry exact file:line spans, kept in sync with the code through git.

For ANY task here — understanding how something works, finding where code lives,
or scoping a change — get context from the graph before grepping or opening
source files. Re-ask freely (it's cheap) and reuse literal identifiers you
already have (symbol, error string, file name) as the query. New to this repo?
Run `graft map` first — a token-budgeted orientation (dir clusters, hubs,
hotspots), no LLM, no key.

- Run `graft ask "<your question>" --source` → ranked nodes with the relevant
  code spans inlined (each hit's ≤8-line crux by default; `--full` for whole
  definitions when the crux isn't enough). Match the tool to the task shape:
  for understanding or editing, the top node IS the answer — cite its
  `covers:` file:line spans and edit straight from `--source`. For
  exhaustive tasks ("every occurrence / every caller of this pattern"), ranked
  results are top-N, not complete — run `graft grep "<literal>"` instead
  (exhaustive over indexed files, grouped by enclosing symbol), falling back
  to raw `grep -rn` only for unindexed files.
- `graft skeleton <file>` → every definition's signature + span, ~10× cheaper
  than reading the file; use it to skim an API surface.
- `graft callers <symbol>` gives precomputed, exact edges — who calls this.
  Add `--direction out` for what it calls, or `--depth N` to walk
  transitively for the full blast radius. For structural questions, skip
  ranking and use this directly.
- Or browse: `graft/INDEX.md` lists every node; follow the links.
- Monorepos and folders of multiple repos rank fairly across sub-projects —
  hits carry `[scope/]` labels naming which one they're from. Narrow with
  `graft ask "<task>" --in <scope>/` once you know where you're working.

If a returned span is truncated ("+N more lines"), open the file at that exact
range before finalizing. Only open source files when a node genuinely lacks a
needed detail, and then at the exact file:line the node points to — never
re-read whole files.

After big code changes, refresh the graph with `graft build` (deterministic,
no API key, $0).
<!-- graft:end -->
