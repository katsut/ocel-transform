# Architecture

How ocel-transform cleans logs without losing the user's trust.

## Recipes are data

A recipe is a named list of declarative steps — filters as predicates,
renames and identity merges as tables, never code. That keeps every
transformation reviewable, diffable, re-runnable in seconds, and honest
about provenance: the recipe file *is* the record of what was done to the
log. The source log is never modified; cleaning writes a new file, so the
raw truth stays recoverable ("extract faithfully, interpret downstream").

## Preview before apply

`preview` runs the same engine against an in-memory copy and reports, per
step, the true number of events that would be dropped plus samples of them.
The studio surfaces this before a recipe is ever materialized — deleting
by rule requires seeing what the rule catches.

## One step, one effect

Event-shaped steps run through ocel-etl's `StagingLog` (objects are left
alone), object-shaped steps use the core's valid-sublog filters, and a final
`validate()` backstops the pipeline: a recipe cannot emit an invalid log.

## The identity-merge step

`mapObjectIds` applies an alias table (old id → canonical id) with full
E2O/O2O reference chasing and same-target merging — the application half of
the propose → human-approve → apply identity-resolution handoff (proposals
come from ocel-annotate's `ocel-aliases`).
