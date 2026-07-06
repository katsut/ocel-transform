# ocel-transform

Recipe-driven [OCEL 2.0](https://www.ocel-standard.org/) log transformation:
clean, rename, and filter event logs *before* mining — noise is best removed
in the data, not worked around in the algorithms.

```sh
ocel-transform --in fd.sqlite --recipe clean.json --out fd.clean.sqlite
```

A recipe is a named, ordered list of steps; the output is a new, valid OCEL
log (every step preserves referential integrity, and the result is
re-validated). Each step reports what it changed — events and objects before
and after — on stderr and as contract-v2 NDJSON progress on stdout, so
[ocel-studio](https://github.com/katsut/ocel-studio) can run recipes as data
sources with a live progress bar.

```json
{
  "name": "clean",
  "steps": [
    { "dropEventTypes": ["subscribed", "mentioned"] },
    { "dropEventsWhere": { "eventType": "comment", "attr": "body",
                           "matches": "(?i)^(thanks|thank you|lgtm)\\W*$" } },
    { "renameEventTypes": { "labeled": "triage", "unlabeled": "triage" } },
    { "timeWindow": { "from": "2024-01-01", "to": "2024-12-31" } },
    { "keepObjectTypes": ["issue", "user"] },
    "dropObjectsWithoutEvents"
  ]
}
```

## Steps

| Step | Effect |
|---|---|
| `dropEventTypes` / `keepEventTypes` | drop/keep events by type; objects stay |
| `dropEventsWhere` | drop events matching a predicate: `eventType`, and/or `attr` with `equals` / `matches` (regex) / `min` / `max` — all set conditions must hold |
| `renameEventTypes` | rename activity types; several old names may merge into one |
| `timeWindow` | keep events in the half-open window `[from, to)` (RFC 3339 or `YYYY-MM-DD`; a date-only `to` includes that day) |
| `keepObjectTypes` | keep objects of these types; events no longer related to any kept object are dropped |
| `dropObjectsWithoutEvents` | drop objects no remaining event references |
| `mapObjectIds` | re-key objects through an `{ "aliases": { "old": "canonical" } }` table — identity resolution as data; ids mapping to one canonical id merge, references follow |

Deterministic only: no step invents data. Semantic classification (e.g.
ML-flagging low-value comments) belongs in annotation attributes written by
other tools; recipes then filter on those attributes like any other.

## The ocel family

| Layer | Repo | License |
|---|---|---|
| Core model, I/O, validation | [ocel-rs](https://github.com/katsut/ocel-rs) (crates.io: [`ocel`](https://crates.io/crates/ocel)) | MIT |
| ETL engine (StagingLog → OCEL) | [ocel-etl](https://github.com/katsut/ocel-etl) | MIT |
| Backlog connector | [ocel-etl-backlog](https://github.com/katsut/ocel-etl-backlog) | MIT |
| GitHub connector | [ocel-etl-github](https://github.com/katsut/ocel-etl-github) | MIT |
| **Log transformation (this repo)** | ocel-transform | MIT |
| Analysis library | [ocel-mine](https://github.com/katsut/ocel-mine) (crates.io: [`ocel-mine`](https://crates.io/crates/ocel-mine)) | MIT |
| Studio — UI + data sources | [ocel-studio](https://github.com/katsut/ocel-studio) | ELv2 |

## License

MIT
