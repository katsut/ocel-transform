# CLAUDE.md — ocel-transform

Deterministic log-cleaning recipes: a JSON recipe in, a transformed OCEL out,
with a preview that shows exactly what a step would delete before anything is
written. Concepts in [ARCHITECTURE.md](ARCHITECTURE.md).

## Build, test, verify

```sh
cargo test
cargo clippy --all-targets -- -D warnings && cargo fmt --check
cargo run --release -- --in <log> --recipe <recipe.json> --out <log>
```

After changing the binary: `cargo install --path .` (the studio resolves
`ocel-transform` from PATH).

## Map

- `src/recipe.rs` — the `Step` vocabulary (dropEventTypes / keepEventTypes /
  dropEventsWhere with equals/matches/min/max / renameEventTypes /
  timeWindow / keepObjectTypes / dropObjectsWithoutEvents / mapObjectIds /
  union / keepRelatedTo / liftEvents); externally tagged serde,
  `deny_unknown_fields`, predicates compiled (and rejected) at load
- `src/apply.rs` — `apply` + `preview` (both take the input log's directory,
  which anchors `union` file references; per-step dropped-event samples with
  true counts, via before-clone + id diff)
- `src/main.rs` — CLI, connector contract v1/v2 (NDJSON progress/log/done,
  honest per-step before/after counts on stderr)

## Invariants and traps

- Event-shaped steps go through ocel-etl's `StagingLog` (objects survive —
  one step, one effect); object-shaped steps use core filters; `validate()`
  backstops the end.
- `timeWindow` is half-open `[from, to)`; a date-only `to` means end of that
  day (the day is included).
- Recipes are data with provenance value — no expression language, no
  scripts (deliberate; revisit only with three concrete needs plus
  pure/terminating/previewable guarantees).
- Zero-condition predicates and value conditions without `attr` are load
  errors, not silent no-ops.

## Conventions

Issue → branch → PR → CI green → squash-merge. Published on crates.io;
publish needs the owner's GO. Design docs (ADR 0005) live in the private
ocel-workspace.
