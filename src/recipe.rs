//! The recipe format: a named, ordered list of transformation steps.
//!
//! JSON uses externally tagged steps, so a recipe reads as data:
//!
//! ```json
//! {
//!   "name": "clean",
//!   "steps": [
//!     { "dropEventTypes": ["subscribed", "mentioned"] },
//!     { "dropEventsWhere": { "eventType": "comment", "attr": "body",
//!                            "matches": "(?i)^(thanks|thank you|lgtm)[!. ]*$" } },
//!     { "renameEventTypes": { "labeled": "triage", "unlabeled": "triage" } },
//!     { "timeWindow": { "from": "2024-01-01", "to": "2025-01-01" } },
//!     { "keepObjectTypes": ["issue", "user"] },
//!     "dropObjectsWithoutEvents"
//!   ]
//! }
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A named transformation pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub name: String,
    #[serde(default)]
    pub steps: Vec<Step>,
}

/// One transformation step. Applied in recipe order; each step's effect is
/// reported (events/objects before and after).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Step {
    /// Drop every event of these types. Objects stay (use
    /// [`Step::DropObjectsWithoutEvents`] to clean up afterwards).
    DropEventTypes(Vec<String>),
    /// Keep only events of these types.
    KeepEventTypes(Vec<String>),
    /// Drop events matching the predicate (all set conditions must hold).
    DropEventsWhere(EventPredicate),
    /// Rename event types; several old names mapping to one new name merge.
    RenameEventTypes(BTreeMap<String, String>),
    /// Keep only events inside the half-open window `[from, to)`. Object
    /// attribute observations are not trimmed.
    TimeWindow(TimeWindow),
    /// Keep objects of these types; events no longer related to any kept
    /// object are dropped (their other E2O links are stripped).
    KeepObjectTypes(Vec<String>),
    /// Drop objects no remaining event references (O2O links to dropped
    /// objects are stripped from survivors).
    DropObjectsWithoutEvents,
    /// Re-key objects through an alias table (identity resolution as data,
    /// not code): ids map to their alias or stay as they are; several ids
    /// mapping to one canonical id merge, and every E2O/O2O reference
    /// follows.
    MapObjectIds(AliasTable),
}

/// The alias table of [`Step::MapObjectIds`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AliasTable {
    /// old id → canonical id.
    pub aliases: BTreeMap<String, String>,
}

impl Step {
    /// Stable display label (matches the JSON tag).
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Step::DropEventTypes(_) => "dropEventTypes",
            Step::KeepEventTypes(_) => "keepEventTypes",
            Step::DropEventsWhere(_) => "dropEventsWhere",
            Step::RenameEventTypes(_) => "renameEventTypes",
            Step::TimeWindow(_) => "timeWindow",
            Step::KeepObjectTypes(_) => "keepObjectTypes",
            Step::DropObjectsWithoutEvents => "dropObjectsWithoutEvents",
            Step::MapObjectIds(_) => "mapObjectIds",
        }
    }
}

/// Conditions on one event; all set fields must hold (AND). At least one
/// condition is required, and value conditions require `attr`. An event
/// without the named attribute never matches.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventPredicate {
    /// Match only events of this type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    /// Attribute the value conditions below apply to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attr: Option<String>,
    /// Value (as text) equals this exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,
    /// Value (as text) matches this regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
    /// Numeric value is at least this (non-numeric values never match).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Numeric value is at most this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

/// Half-open time window `[from, to)`. Bounds accept RFC 3339 or
/// `YYYY-MM-DD`; a date-only `to` means "up to and including that day"
/// (it parses to the following midnight).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimeWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}
