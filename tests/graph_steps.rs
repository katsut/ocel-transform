//! The union / keepRelatedTo / liftEvents steps.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ocel_etl::{StagingEvent, StagingLog};
use ocel_transform::{apply, preview, LiftEvents, Recipe, RelatedTo, Step, UnionSource};

fn ts(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).unwrap()
}

fn event(id: &str, ty: &str, secs: i64, relations: Vec<(&str, &str)>) -> StagingEvent {
    StagingEvent {
        id: id.into(),
        event_type: ty.into(),
        time: ts(secs),
        attributes: vec![],
        relations: relations
            .into_iter()
            .map(|(o, q)| (o.into(), q.into()))
            .collect(),
    }
}

fn recipe(steps: Vec<Step>) -> Recipe {
    Recipe {
        name: "test".into(),
        steps,
    }
}

fn base() -> &'static Path {
    Path::new(".")
}

// --- union -------------------------------------------------------------------

/// Current log: issue t1 with events e1/e2, actor u1 shared with the other log.
fn current() -> ocel::Ocel {
    let mut staging = StagingLog::new();
    staging.upsert_object("t1", "issue");
    staging.upsert_object("u1", "user");
    staging.add_event(event(
        "e1",
        "open",
        100,
        vec![("t1", "subject"), ("u1", "actor")],
    ));
    staging.add_event(event("e2", "close", 200, vec![("t1", "subject")]));
    staging.into_ocel().unwrap()
}

/// The other source: issue t2 assigned to the same user u1.
fn other() -> ocel::Ocel {
    let mut staging = StagingLog::new();
    staging.upsert_object("t2", "issue");
    staging.upsert_object("u1", "user");
    staging.add_o2o("t2", "u1", "assigned to");
    staging.add_event(event(
        "e3",
        "open",
        300,
        vec![("t2", "subject"), ("u1", "actor")],
    ));
    staging.into_ocel().unwrap()
}

/// Write `log` where a `{"union": {"file": "other.json"}}` step will find it;
/// returns the base directory to pass to apply/preview.
fn write_other(name: &str, log: &ocel::Ocel) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    ocel::io::write_path(log, dir.join("other.json")).unwrap();
    dir
}

fn union_step() -> Vec<Step> {
    vec![Step::Union(UnionSource {
        file: "other.json".into(),
    })]
}

#[test]
fn union_merges_events_and_shared_objects() {
    let dir = write_other("union-merge", &other());
    let (log, reports) = apply(&recipe(union_step()), current(), &dir).unwrap();
    assert_eq!(log.validate(), Ok(()));
    assert_eq!(log.events.len(), 3);
    assert_eq!(log.objects.len(), 3); // u1 merged, not duplicated
    assert_eq!(reports[0].events_before, 2);
    assert_eq!(reports[0].events_after, 3);
    assert_eq!(reports[0].objects_after, 3);
    assert_eq!(reports[0].duplicates_skipped, Some(0));
    // the merged u1 carries the other log's O2O
    assert!(log
        .o2o()
        .any(|r| r.source_id == "t2" && r.target_id == "u1"));
}

#[test]
fn union_skips_identical_duplicates_and_reports_the_count() {
    // the other log repeats e1 exactly as the current log has it
    let mut staging = StagingLog::new();
    staging.upsert_object("t1", "issue");
    staging.upsert_object("u1", "user");
    staging.upsert_object("t2", "issue");
    staging.add_event(event(
        "e1",
        "open",
        100,
        vec![("t1", "subject"), ("u1", "actor")],
    ));
    staging.add_event(event("e3", "open", 300, vec![("t2", "subject")]));
    let dir = write_other("union-dup", &staging.into_ocel().unwrap());

    let (log, previews) = preview(&recipe(union_step()), current(), &dir, 10).unwrap();
    assert_eq!(log.validate(), Ok(()));
    assert_eq!(log.events.len(), 3); // e1 skipped, e3 added
    assert_eq!(previews[0].report.duplicates_skipped, Some(1));
    assert_eq!(previews[0].dropped_total, 0); // union never drops
}

#[test]
fn union_fails_on_same_id_events_that_differ() {
    // the other log claims e1 happened at a different time
    let mut staging = StagingLog::new();
    staging.upsert_object("t1", "issue");
    staging.add_event(event("e1", "open", 999, vec![("t1", "subject")]));
    let dir = write_other("union-conflict", &staging.into_ocel().unwrap());

    let err = apply(&recipe(union_step()), current(), &dir).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("e1"), "{message}");
    assert!(message.contains("differ"), "{message}");
}

// --- keepRelatedTo -----------------------------------------------------------

/// o1 (opportunity) shares an event with c1 (contract); c1 —O2O— a1 (account)
/// —O2O— u1 (user) and i1 (invoice); i1 —O2O— p1 (payment); u1 shares an
/// event with x1 (ticket); z1 (ticket) is a disconnected island.
fn crm() -> ocel::Ocel {
    let mut staging = StagingLog::new();
    staging.upsert_object("o1", "opportunity");
    staging.upsert_object("c1", "contract");
    staging.upsert_object("a1", "account");
    staging.upsert_object("u1", "user");
    staging.upsert_object("x1", "ticket");
    staging.upsert_object("i1", "invoice");
    staging.upsert_object("p1", "payment");
    staging.upsert_object("z1", "ticket");
    staging.add_o2o("c1", "a1", "billed to");
    staging.add_o2o("a1", "u1", "owned by");
    staging.add_o2o("a1", "i1", "invoiced as");
    staging.add_o2o("i1", "p1", "settled by");
    staging.add_event(event(
        "e1",
        "contract signed",
        100,
        vec![("o1", "subject"), ("c1", "contract")],
    ));
    staging.add_event(event(
        "e2",
        "ticket opened",
        200,
        vec![("u1", "reporter"), ("x1", "subject")],
    ));
    staging.add_event(event("e3", "ticket opened", 300, vec![("z1", "subject")]));
    staging.into_ocel().unwrap()
}

fn keep_related(via: Option<Vec<String>>, not_via: Option<Vec<String>>) -> Vec<Step> {
    vec![Step::KeepRelatedTo(RelatedTo {
        object_type: "opportunity".into(),
        via,
        not_via,
    })]
}

fn sorted_object_ids(log: &ocel::Ocel) -> Vec<&str> {
    let mut ids: Vec<&str> = log.objects.iter().map(|o| o.id.as_str()).collect();
    ids.sort_unstable();
    ids
}

#[test]
fn keep_related_to_via_walks_allowed_types_and_keeps_endpoints() {
    let steps = keep_related(Some(vec!["contract".into(), "account".into()]), None);
    let (log, reports) = apply(&recipe(steps), crm(), base()).unwrap();
    assert_eq!(log.validate(), Ok(()));
    // u1 and i1 are reached from the expandable a1 and kept, but neither is
    // in `via`, so the walk stops there: x1 (behind u1) and p1 (behind i1)
    // are dropped, as is the island z1
    assert_eq!(sorted_object_ids(&log), ["a1", "c1", "i1", "o1", "u1"]);
    let event_ids: Vec<&str> = log.events.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(event_ids, ["e1", "e2"]);
    // e2 lost its dropped ticket but keeps the kept user
    let e2 = log.events.iter().find(|e| e.id == "e2").unwrap();
    assert_eq!(e2.relationships.len(), 1);
    assert_eq!(e2.relationships[0].object_id, "u1");
    assert_eq!(reports[0].objects_before, 8);
    assert_eq!(reports[0].objects_after, 5);
}

#[test]
fn keep_related_to_not_via_stops_at_the_listed_types() {
    let steps = keep_related(None, Some(vec!["user".into()]));
    let (log, _) = apply(&recipe(steps), crm(), base()).unwrap();
    assert_eq!(log.validate(), Ok(()));
    // everything except users is walkable, so p1 behind i1 is reached; u1 is
    // kept but not walked through, so x1 stays out — and so does island z1
    assert_eq!(
        sorted_object_ids(&log),
        ["a1", "c1", "i1", "o1", "p1", "u1"]
    );
}

#[test]
fn keep_related_to_requires_exactly_one_of_via_and_not_via() {
    let neither = keep_related(None, None);
    let err = apply(&recipe(neither), crm(), base()).unwrap_err();
    assert!(err.to_string().contains("exactly one"), "{err}");

    let both = keep_related(Some(vec!["contract".into()]), Some(vec!["user".into()]));
    let err = apply(&recipe(both), crm(), base()).unwrap_err();
    assert!(err.to_string().contains("exactly one"), "{err}");
}

#[test]
fn keep_related_to_preview_samples_the_dropped_events() {
    // via contract only: a1 is kept but not expanded, so u1/x1/z1 drop and
    // take e2 and e3 with them
    let steps = keep_related(Some(vec!["contract".into()]), None);
    let (log, previews) = preview(&recipe(steps), crm(), base(), 10).unwrap();
    assert_eq!(log.validate(), Ok(()));
    assert_eq!(previews[0].dropped_total, 2);
    let dropped: Vec<&str> = previews[0]
        .dropped_events
        .iter()
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(dropped, ["e2", "e3"]);
}

// --- liftEvents --------------------------------------------------------------

/// Contracts and opportunities O2O-linked in both directions: c1 -> p1
/// (from-side), p2 -> c2 (to-side).
fn sales() -> ocel::Ocel {
    let mut staging = StagingLog::new();
    staging.upsert_object("c1", "contract");
    staging.upsert_object("c2", "contract");
    staging.upsert_object("p1", "opportunity");
    staging.upsert_object("p2", "opportunity");
    staging.add_o2o("c1", "p1", "fulfills");
    staging.add_o2o("p2", "c2", "closed by");
    staging.add_event(event(
        "s1",
        "contract signed",
        100,
        vec![("c1", "contract")],
    ));
    staging.add_event(event(
        "s2",
        "contract signed",
        200,
        vec![("c2", "contract")],
    ));
    staging.add_event(event(
        "s3",
        "contract signed",
        300,
        vec![("c1", "contract"), ("p1", "opportunity")],
    ));
    staging.add_event(event(
        "s4",
        "contract drafted",
        400,
        vec![("c1", "contract")],
    ));
    staging.into_ocel().unwrap()
}

fn lift_step(qualifier: &str) -> Vec<Step> {
    vec![Step::LiftEvents(LiftEvents {
        from: "contract".into(),
        to: "opportunity".into(),
        event_types: vec!["contract signed".into()],
        qualifier: qualifier.into(),
    })]
}

#[test]
fn lift_events_adds_relations_across_o2o_in_both_directions() {
    let (log, reports) = apply(&recipe(lift_step("closes")), sales(), base()).unwrap();
    assert_eq!(log.validate(), Ok(()));
    let relation = |id: &str| {
        let event = log.events.iter().find(|e| e.id == id).unwrap();
        event.relationships.clone()
    };
    // forward O2O (c1 -> p1)
    let s1 = relation("s1");
    assert!(s1
        .iter()
        .any(|r| r.object_id == "p1" && r.qualifier == "closes"));
    // reverse O2O (p2 -> c2)
    let s2 = relation("s2");
    assert!(s2
        .iter()
        .any(|r| r.object_id == "p2" && r.qualifier == "closes"));
    // already related to p1: no duplicate added
    assert_eq!(relation("s3").len(), 2);
    // other event types are untouched
    assert_eq!(relation("s4").len(), 1);
    assert_eq!(reports[0].events_lifted, Some(2));
    assert_eq!(reports[0].events_before, reports[0].events_after);
}

#[test]
fn lift_events_qualifier_defaults_to_lifted() {
    let json = r#"{ "name": "lift", "steps": [
      { "liftEvents": { "from": "contract", "to": "opportunity",
                        "eventTypes": ["contract signed"] } }
    ] }"#;
    let recipe: Recipe = serde_json::from_str(json).unwrap();
    let (log, _) = apply(&recipe, sales(), base()).unwrap();
    let s1 = log.events.iter().find(|e| e.id == "s1").unwrap();
    assert!(s1
        .relationships
        .iter()
        .any(|r| r.object_id == "p1" && r.qualifier == "lifted"));
}

#[test]
fn lift_events_rejects_empty_event_types() {
    let steps = vec![Step::LiftEvents(LiftEvents {
        from: "contract".into(),
        to: "opportunity".into(),
        event_types: vec![],
        qualifier: "closes".into(),
    })];
    let err = apply(&recipe(steps), sales(), base()).unwrap_err();
    assert!(err.to_string().contains("eventTypes"), "{err}");
}

#[test]
fn lift_events_requires_event_types_in_json() {
    let json = r#"{ "name": "x", "steps": [
      { "liftEvents": { "from": "contract", "to": "opportunity" } }
    ] }"#;
    assert!(serde_json::from_str::<Recipe>(json).is_err());
}

// --- JSON spellings ----------------------------------------------------------

#[test]
fn new_steps_parse_from_json() {
    let json = r#"{ "name": "graph", "steps": [
      { "union": { "file": "other.sqlite" } },
      { "keepRelatedTo": { "objectType": "opportunity", "via": ["contract", "account"] } },
      { "keepRelatedTo": { "objectType": "opportunity", "notVia": ["user"] } },
      { "liftEvents": { "from": "contract", "to": "opportunity",
                        "eventTypes": ["contract signed"], "qualifier": "closes" } }
    ] }"#;
    let recipe: Recipe = serde_json::from_str(json).unwrap();
    assert_eq!(recipe.steps.len(), 4);
    assert_eq!(recipe.steps[0].label(), "union");
    assert_eq!(recipe.steps[1].label(), "keepRelatedTo");
    assert_eq!(recipe.steps[3].label(), "liftEvents");
}
