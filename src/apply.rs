//! Applying a recipe: each step maps a valid log to a valid log.
//!
//! Event-level steps run through [`ocel_etl::StagingLog`] (whose gate
//! re-validates), object-level steps use the core's OCEL-aware filters, and
//! the final result is validated once more — a recipe can drop data, but it
//! can never produce an inconsistent log.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};
use ocel::{AttrValue, Event, Ocel, Relationship, Violation};
use ocel_etl::{StagingEvent, StagingLog};
use regex::Regex;
use thiserror::Error;

use crate::recipe::{EventPredicate, LiftEvents, Recipe, RelatedTo, Step};

#[derive(Debug, Error)]
pub enum TransformError {
    #[error("step {step}: invalid regex: {message}")]
    BadRegex { step: usize, message: String },

    #[error("step {step}: invalid time bound {value:?} (RFC 3339 or YYYY-MM-DD)")]
    BadTime { step: usize, value: String },

    #[error("step {step}: predicate has no conditions (it would drop every event)")]
    EmptyPredicate { step: usize },

    #[error("step {step}: value conditions (equals/matches/min/max) require `attr`")]
    ValueConditionWithoutAttr { step: usize },

    #[error("step {step}: union: cannot read {path}: {message}")]
    UnionRead {
        step: usize,
        path: String,
        message: String,
    },

    #[error("step {step}: union: {total} events share an id with the current log but differ (e.g. {ids:?}); same-id events must be identical to merge")]
    UnionConflict {
        step: usize,
        total: usize,
        ids: Vec<String>,
    },

    #[error("step {step}: keepRelatedTo requires exactly one of `via` / `notVia`")]
    ViaXorNotVia { step: usize },

    #[error("step {step}: liftEvents requires a non-empty `eventTypes`")]
    EmptyLiftEventTypes { step: usize },

    #[error("the transformed log failed validation: {0:?}")]
    Invalid(Vec<Violation>),
}

/// Effect of one step, for honest reporting in CLIs and UIs.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepReport {
    pub step: String,
    pub events_before: usize,
    pub events_after: usize,
    pub objects_before: usize,
    pub objects_after: usize,
    /// `union` only: incoming events skipped because an identical event
    /// already existed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicates_skipped: Option<usize>,
    /// `liftEvents` only: events that gained at least one lifted relation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events_lifted: Option<usize>,
}

/// Step-specific effect detail beyond the generic before/after counts.
#[derive(Debug, Clone, Copy, Default)]
struct StepNotes {
    duplicates_skipped: Option<usize>,
    events_lifted: Option<usize>,
}

/// Apply every step of `recipe` in order. `base_dir` anchors relative file
/// references in steps like `union` (pass the input log's directory).
pub fn apply(
    recipe: &Recipe,
    log: Ocel,
    base_dir: &Path,
) -> Result<(Ocel, Vec<StepReport>), TransformError> {
    let mut log = log;
    let mut reports = Vec::with_capacity(recipe.steps.len());
    for (index, step) in recipe.steps.iter().enumerate() {
        let events_before = log.events.len();
        let objects_before = log.objects.len();
        let (transformed, notes) = apply_step(index, step, log, base_dir)?;
        log = transformed;
        reports.push(StepReport {
            step: step.label().to_owned(),
            events_before,
            events_after: log.events.len(),
            objects_before,
            objects_after: log.objects.len(),
            duplicates_skipped: notes.duplicates_skipped,
            events_lifted: notes.events_lifted,
        });
    }
    log.validate().map_err(TransformError::Invalid)?;
    Ok((log, reports))
}

/// A dropped event, summarized for a "this is what the recipe deletes"
/// preview — machine cleaning must stay human-inspectable.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedEvent {
    pub id: String,
    pub event_type: String,
    pub time: DateTime<Utc>,
    /// (name, value as text) pairs.
    pub attributes: Vec<(String, String)>,
}

/// One step's effect plus a sample of the events it removed.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepPreview {
    #[serde(flatten)]
    pub report: StepReport,
    /// Up to `sample` of the events this step dropped.
    pub dropped_events: Vec<DroppedEvent>,
    /// Total events this step dropped (the sample may be shorter).
    pub dropped_total: usize,
}

/// Like [`apply`], but additionally samples the events each step drops.
/// Costs one id-set pass per step; meant for interactive previews.
pub fn preview(
    recipe: &Recipe,
    log: Ocel,
    base_dir: &Path,
    sample: usize,
) -> Result<(Ocel, Vec<StepPreview>), TransformError> {
    let mut log = log;
    let mut previews = Vec::with_capacity(recipe.steps.len());
    for (index, step) in recipe.steps.iter().enumerate() {
        let before = log.clone();
        let (transformed, notes) = apply_step(index, step, log, base_dir)?;
        log = transformed;
        let after_ids: BTreeSet<&str> = log.events.iter().map(|e| e.id.as_str()).collect();
        let dropped: Vec<&ocel::Event> = before
            .events
            .iter()
            .filter(|e| !after_ids.contains(e.id.as_str()))
            .collect();
        let dropped_events = dropped
            .iter()
            .take(sample)
            .map(|e| DroppedEvent {
                id: e.id.clone(),
                event_type: e.event_type.clone(),
                time: e.time,
                attributes: e
                    .attributes
                    .iter()
                    .map(|a| (a.name.clone(), a.value.to_text()))
                    .collect(),
            })
            .collect();
        previews.push(StepPreview {
            report: StepReport {
                step: step.label().to_owned(),
                events_before: before.events.len(),
                events_after: log.events.len(),
                objects_before: before.objects.len(),
                objects_after: log.objects.len(),
                duplicates_skipped: notes.duplicates_skipped,
                events_lifted: notes.events_lifted,
            },
            dropped_events,
            dropped_total: dropped.len(),
        });
    }
    log.validate().map_err(TransformError::Invalid)?;
    Ok((log, previews))
}

fn apply_step(
    index: usize,
    step: &Step,
    log: Ocel,
    base_dir: &Path,
) -> Result<(Ocel, StepNotes), TransformError> {
    let mut notes = StepNotes::default();
    let log = match step {
        Step::DropEventTypes(names) => with_staging(log, |staging| {
            staging.retain_events(|e| !names.contains(&e.event_type));
        }),
        Step::KeepEventTypes(names) => with_staging(log, |staging| {
            staging.retain_events(|e| names.contains(&e.event_type));
        }),
        Step::DropEventsWhere(predicate) => {
            let matcher = Matcher::compile(index, predicate)?;
            with_staging(log, |staging| {
                staging.retain_events(|e| !matcher.matches(&e.event_type, &e.attributes));
            })
        }
        Step::RenameEventTypes(renames) => with_staging(log, |staging| {
            staging.map_events(|e| {
                if let Some(new_name) = renames.get(&e.event_type) {
                    e.event_type.clone_from(new_name);
                }
            });
        }),
        Step::TimeWindow(window) => {
            let from = parse_bound(index, window.from.as_deref(), false)?;
            let to = parse_bound(index, window.to.as_deref(), true)?;
            with_staging(log, |staging| {
                staging.retain_events(|e| {
                    from.is_none_or(|f| e.time >= f) && to.is_none_or(|t| e.time < t)
                });
            })
        }
        Step::KeepObjectTypes(names) => {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            log.filter_object_types(&names)
        }
        Step::DropObjectsWithoutEvents => drop_objects_without_events(log),
        Step::MapObjectIds(table) => with_staging(log, |staging| {
            staging.map_object_ids(|id| {
                table
                    .aliases
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| id.to_owned())
            });
        }),
        Step::Union(source) => {
            let (merged, duplicates) = union_logs(index, &source.file, base_dir, log)?;
            notes.duplicates_skipped = Some(duplicates);
            merged
        }
        Step::KeepRelatedTo(related) => {
            let expansion = Expansion::compile(index, related)?;
            keep_related_to(&log, &related.object_type, &expansion)
        }
        Step::LiftEvents(lift) => {
            if lift.event_types.is_empty() {
                return Err(TransformError::EmptyLiftEventTypes { step: index });
            }
            let (lifted_log, lifted) = lift_events(log, lift);
            notes.events_lifted = Some(lifted);
            lifted_log
        }
    };
    Ok((log, notes))
}

/// Round-trip through the staging representation; the gate cannot fail here
/// because the input was valid and these transforms only drop or rename.
fn with_staging(log: Ocel, f: impl FnOnce(&mut StagingLog)) -> Ocel {
    let mut staging = StagingLog::from_ocel(log);
    f(&mut staging);
    staging
        .into_ocel()
        .expect("dropping or renaming events keeps a valid log valid")
}

fn drop_objects_without_events(log: Ocel) -> Ocel {
    let referenced: BTreeSet<String> = log
        .events
        .iter()
        .flat_map(|e| e.relationships.iter().map(|r| r.object_id.clone()))
        .collect();
    let objects = log
        .objects
        .into_iter()
        .filter(|o| referenced.contains(&o.id))
        .map(|mut o| {
            o.relationships
                .retain(|r| referenced.contains(&r.object_id));
            o
        })
        .collect();
    Ocel { objects, ..log }
}

/// Merge the OCEL file at `file` (resolved against `base_dir`) into `log`.
///
/// The current log seeds a [`StagingLog`]; the other log's objects upsert
/// into it (same-id objects merge: types unify, attribute observations and
/// O2O relations append), then its events are added. Same-id events must be
/// identical — identical ones are skipped and counted, differing ones fail
/// with the conflicting ids. Returns the merged log and the skip count.
fn union_logs(
    step: usize,
    file: &str,
    base_dir: &Path,
    log: Ocel,
) -> Result<(Ocel, usize), TransformError> {
    let path = base_dir.join(file);
    let other = ocel::io::read_path(&path).map_err(|e| TransformError::UnionRead {
        step,
        path: path.display().to_string(),
        message: e.to_string(),
    })?;

    let ours: BTreeMap<&str, &Event> = log.events.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut duplicates = 0usize;
    let mut conflicts: Vec<String> = Vec::new();
    for theirs in &other.events {
        match ours.get(theirs.id.as_str()) {
            Some(&existing) if existing == theirs => duplicates += 1,
            Some(_) => conflicts.push(theirs.id.clone()),
            None => {}
        }
    }
    if !conflicts.is_empty() {
        let total = conflicts.len();
        conflicts.truncate(5);
        return Err(TransformError::UnionConflict {
            step,
            total,
            ids: conflicts,
        });
    }

    let known: BTreeSet<String> = log.events.iter().map(|e| e.id.clone()).collect();
    let mut staging = StagingLog::from_ocel(log);
    for object in other.objects {
        staging.upsert_object(&object.id, &object.object_type);
        for attr in object.attributes {
            staging.add_object_attribute(&object.id, &attr.name, attr.value, attr.time);
        }
        for rel in object.relationships {
            staging.add_o2o(&object.id, &rel.object_id, &rel.qualifier);
        }
    }
    for event in other.events {
        if known.contains(&event.id) {
            continue; // an identical duplicate, counted above
        }
        staging.add_event(StagingEvent {
            id: event.id,
            event_type: event.event_type,
            time: event.time,
            attributes: event
                .attributes
                .into_iter()
                .map(|a| (a.name, a.value))
                .collect(),
            relations: event
                .relationships
                .into_iter()
                .map(|r| (r.object_id, r.qualifier))
                .collect(),
        });
    }
    let merged = staging.into_ocel().map_err(TransformError::Invalid)?;
    Ok((merged, duplicates))
}

/// Which object types a `keepRelatedTo` walk may continue through.
enum Expansion<'a> {
    Via(BTreeSet<&'a str>),
    NotVia(BTreeSet<&'a str>),
}

impl<'a> Expansion<'a> {
    fn compile(step: usize, spec: &'a RelatedTo) -> Result<Expansion<'a>, TransformError> {
        match (&spec.via, &spec.not_via) {
            (Some(via), None) => Ok(Expansion::Via(via.iter().map(String::as_str).collect())),
            (None, Some(not_via)) => Ok(Expansion::NotVia(
                not_via.iter().map(String::as_str).collect(),
            )),
            _ => Err(TransformError::ViaXorNotVia { step }),
        }
    }

    fn allows(&self, object_type: &str) -> bool {
        match self {
            Expansion::Via(types) => types.contains(object_type),
            Expansion::NotVia(types) => !types.contains(object_type),
        }
    }
}

/// BFS over the object interaction graph (shared events + O2O, either
/// direction) from every object of `seed_type`. Every reached object is
/// kept; the walk continues through it only if its type is allowed (seeds
/// always are). The result is a consistent sub-log with the same semantics
/// as the core's object filters: E2O/O2O to dropped objects are stripped,
/// events left without any object are dropped, declarations stay.
fn keep_related_to(log: &Ocel, seed_type: &str, expansion: &Expansion<'_>) -> Ocel {
    let graph = log.object_graph();
    let types: BTreeMap<&str, &str> = log
        .objects
        .iter()
        .map(|o| (o.id.as_str(), o.object_type.as_str()))
        .collect();
    let mut kept: BTreeSet<&str> = log
        .objects
        .iter()
        .filter(|o| o.object_type == seed_type)
        .map(|o| o.id.as_str())
        .collect();
    let mut expanded = kept.clone();
    let mut queue: VecDeque<&str> = kept.iter().copied().collect();
    while let Some(id) = queue.pop_front() {
        for neighbor in graph.neighbors(id) {
            kept.insert(neighbor);
            let walkable = types
                .get(neighbor)
                .copied()
                .is_some_and(|ty| expansion.allows(ty));
            if walkable && expanded.insert(neighbor) {
                queue.push_back(neighbor);
            }
        }
    }

    let events = log
        .events
        .iter()
        .filter_map(|e| {
            let mut event = e.clone();
            event
                .relationships
                .retain(|r| kept.contains(r.object_id.as_str()));
            (!event.relationships.is_empty()).then_some(event)
        })
        .collect();
    let objects = log
        .objects
        .iter()
        .filter(|o| kept.contains(o.id.as_str()))
        .map(|o| {
            let mut object = o.clone();
            object
                .relationships
                .retain(|r| kept.contains(r.object_id.as_str()));
            object
        })
        .collect();
    Ocel {
        event_types: log.event_types.clone(),
        object_types: log.object_types.clone(),
        events,
        objects,
    }
}

/// For each event of the listed types related to a `from`-type object F, add
/// an E2O relation to every `to`-type object O2O-linked with F (either
/// direction), unless the event already relates to it. Returns the log and
/// the number of events that gained at least one relation.
fn lift_events(mut log: Ocel, spec: &LiftEvents) -> (Ocel, usize) {
    let from_ids: BTreeSet<&str> = log
        .objects
        .iter()
        .filter(|o| o.object_type == spec.from)
        .map(|o| o.id.as_str())
        .collect();
    let to_ids: BTreeSet<&str> = log
        .objects
        .iter()
        .filter(|o| o.object_type == spec.to)
        .map(|o| o.id.as_str())
        .collect();
    // from-object id -> to-object ids O2O-linked with it, either direction
    let mut linked: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for object in &log.objects {
        if from_ids.contains(object.id.as_str()) {
            for rel in &object.relationships {
                if to_ids.contains(rel.object_id.as_str()) {
                    linked
                        .entry(object.id.as_str())
                        .or_default()
                        .insert(rel.object_id.as_str());
                }
            }
        }
        if to_ids.contains(object.id.as_str()) {
            for rel in &object.relationships {
                if from_ids.contains(rel.object_id.as_str()) {
                    linked
                        .entry(rel.object_id.as_str())
                        .or_default()
                        .insert(object.id.as_str());
                }
            }
        }
    }

    let mut lifted = 0usize;
    for event in &mut log.events {
        if !spec.event_types.contains(&event.event_type) {
            continue;
        }
        let mut targets: BTreeSet<&str> = BTreeSet::new();
        for rel in &event.relationships {
            if let Some(found) = linked.get(rel.object_id.as_str()) {
                targets.extend(found.iter().copied());
            }
        }
        let additions: Vec<String> = targets
            .into_iter()
            .filter(|t| event.relationships.iter().all(|r| r.object_id != *t))
            .map(ToOwned::to_owned)
            .collect();
        if additions.is_empty() {
            continue;
        }
        lifted += 1;
        for object_id in additions {
            event.relationships.push(Relationship {
                object_id,
                qualifier: spec.qualifier.clone(),
            });
        }
    }
    (log, lifted)
}

/// A compiled event predicate.
struct Matcher {
    event_type: Option<String>,
    attr: Option<String>,
    equals: Option<String>,
    regex: Option<Regex>,
    min: Option<f64>,
    max: Option<f64>,
}

impl Matcher {
    fn compile(step: usize, p: &EventPredicate) -> Result<Matcher, TransformError> {
        let has_value_condition =
            p.equals.is_some() || p.matches.is_some() || p.min.is_some() || p.max.is_some();
        if p.event_type.is_none() && p.attr.is_none() {
            return Err(TransformError::EmptyPredicate { step });
        }
        if has_value_condition && p.attr.is_none() {
            return Err(TransformError::ValueConditionWithoutAttr { step });
        }
        let regex = p
            .matches
            .as_deref()
            .map(Regex::new)
            .transpose()
            .map_err(|e| TransformError::BadRegex {
                step,
                message: e.to_string(),
            })?;
        Ok(Matcher {
            event_type: p.event_type.clone(),
            attr: p.attr.clone(),
            equals: p.equals.clone(),
            regex,
            min: p.min,
            max: p.max,
        })
    }

    fn matches(&self, event_type: &str, attributes: &[(String, AttrValue)]) -> bool {
        if let Some(ty) = &self.event_type {
            if event_type != ty {
                return false;
            }
        }
        let Some(attr) = &self.attr else {
            return true; // type-only predicate
        };
        let Some((_, value)) = attributes.iter().find(|(name, _)| name == attr) else {
            return false; // events without the attribute never match
        };
        let text = value.to_text();
        if let Some(equals) = &self.equals {
            if text != *equals {
                return false;
            }
        }
        if let Some(regex) = &self.regex {
            if !regex.is_match(&text) {
                return false;
            }
        }
        if self.min.is_some() || self.max.is_some() {
            // numeric conditions apply to numeric values only; intentional cast
            #[allow(clippy::cast_precision_loss)]
            let number = match value {
                AttrValue::Integer(i) => Some(*i as f64),
                AttrValue::Float(f) => Some(*f),
                AttrValue::String(_) | AttrValue::Boolean(_) | AttrValue::Time(_) => None,
            };
            let Some(number) = number else {
                return false;
            };
            if self.min.is_some_and(|min| number < min) {
                return false;
            }
            if self.max.is_some_and(|max| number > max) {
                return false;
            }
        }
        true
    }
}

/// RFC 3339, or a date: `from` dates mean that midnight, `to` dates mean the
/// *following* midnight so the named day is included in the half-open window.
fn parse_bound(
    step: usize,
    bound: Option<&str>,
    is_to: bool,
) -> Result<Option<DateTime<Utc>>, TransformError> {
    let Some(raw) = bound else {
        return Ok(None);
    };
    if let Ok(instant) = DateTime::parse_from_rfc3339(raw) {
        return Ok(Some(instant.to_utc()));
    }
    let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") else {
        return Err(TransformError::BadTime {
            step,
            value: raw.to_owned(),
        });
    };
    let date = if is_to { date.succ_opt() } else { Some(date) };
    let midnight =
        date.and_then(|d| d.and_hms_opt(0, 0, 0))
            .ok_or_else(|| TransformError::BadTime {
                step,
                value: raw.to_owned(),
            })?;
    Ok(Some(midnight.and_utc()))
}
