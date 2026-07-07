//! Recipe-driven OCEL 2.0 log transformation.
//!
//! A recipe is a declarative list of steps applied in order; the result is a
//! new, valid OCEL log. Deterministic only: no step invents data, and every
//! step reports what it changed.

pub mod apply;
pub mod recipe;

pub use apply::{apply, preview, DroppedEvent, StepPreview, StepReport, TransformError};
pub use recipe::{
    AliasTable, EventPredicate, LiftEvents, Recipe, RelatedTo, Step, TimeWindow, UnionSource,
};
