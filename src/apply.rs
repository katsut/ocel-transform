//! Applying a recipe: each step maps a valid log to a valid log.
//!
//! Event-level steps run through [`ocel_etl::StagingLog`] (whose gate
//! re-validates), object-level steps use the core's OCEL-aware filters, and
//! the final result is validated once more — a recipe can drop data, but it
//! can never produce an inconsistent log.

use std::collections::BTreeSet;

use chrono::{DateTime, NaiveDate, Utc};
use ocel::{AttrValue, Ocel, Violation};
use ocel_etl::StagingLog;
use regex::Regex;
use thiserror::Error;

use crate::recipe::{EventPredicate, Recipe, Step};

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
}

/// Apply every step of `recipe` in order.
pub fn apply(recipe: &Recipe, log: Ocel) -> Result<(Ocel, Vec<StepReport>), TransformError> {
    let mut log = log;
    let mut reports = Vec::with_capacity(recipe.steps.len());
    for (index, step) in recipe.steps.iter().enumerate() {
        let events_before = log.events.len();
        let objects_before = log.objects.len();
        log = apply_step(index, step, log)?;
        reports.push(StepReport {
            step: step.label().to_owned(),
            events_before,
            events_after: log.events.len(),
            objects_before,
            objects_after: log.objects.len(),
        });
    }
    log.validate().map_err(TransformError::Invalid)?;
    Ok((log, reports))
}

fn apply_step(index: usize, step: &Step, log: Ocel) -> Result<Ocel, TransformError> {
    match step {
        Step::DropEventTypes(names) => Ok(with_staging(log, |staging| {
            staging.retain_events(|e| !names.contains(&e.event_type));
        })),
        Step::KeepEventTypes(names) => Ok(with_staging(log, |staging| {
            staging.retain_events(|e| names.contains(&e.event_type));
        })),
        Step::DropEventsWhere(predicate) => {
            let matcher = Matcher::compile(index, predicate)?;
            Ok(with_staging(log, |staging| {
                staging.retain_events(|e| !matcher.matches(&e.event_type, &e.attributes));
            }))
        }
        Step::RenameEventTypes(renames) => Ok(with_staging(log, |staging| {
            staging.map_events(|e| {
                if let Some(new_name) = renames.get(&e.event_type) {
                    e.event_type.clone_from(new_name);
                }
            });
        })),
        Step::TimeWindow(window) => {
            let from = parse_bound(index, window.from.as_deref(), false)?;
            let to = parse_bound(index, window.to.as_deref(), true)?;
            Ok(with_staging(log, |staging| {
                staging.retain_events(|e| {
                    from.is_none_or(|f| e.time >= f) && to.is_none_or(|t| e.time < t)
                });
            }))
        }
        Step::KeepObjectTypes(names) => {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            Ok(log.filter_object_types(&names))
        }
        Step::DropObjectsWithoutEvents => Ok(drop_objects_without_events(log)),
    }
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
