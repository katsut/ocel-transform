use std::path::Path;

use chrono::{DateTime, Utc};
use ocel::AttrValue;
use ocel_etl::{StagingEvent, StagingLog};
use ocel_transform::{apply, EventPredicate, Recipe, Step, TimeWindow};

fn ts(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).unwrap()
}

fn event(id: &str, ty: &str, secs: i64, attrs: Vec<(&str, AttrValue)>) -> StagingEvent {
    StagingEvent {
        id: id.into(),
        event_type: ty.into(),
        time: ts(secs),
        attributes: attrs.into_iter().map(|(n, v)| (n.into(), v)).collect(),
        relations: vec![("t1".into(), "subject".into())],
    }
}

/// issue t1 with a mixed little history; user u1 acts but has no own events.
fn sample() -> ocel::Ocel {
    let mut staging = StagingLog::new();
    staging.upsert_object("t1", "issue");
    staging.upsert_object("u1", "user");
    staging.add_o2o("t1", "u1", "assigned to");
    staging.add_event(event("e1", "open", 100, vec![]));
    staging.add_event(event(
        "e2",
        "comment",
        200,
        vec![("body", AttrValue::String("thanks!".into()))],
    ));
    staging.add_event(event(
        "e3",
        "comment",
        300,
        vec![("body", AttrValue::String("here is a real repro".into()))],
    ));
    staging.add_event(event("e4", "labeled", 400, vec![]));
    staging.add_event(event("e5", "unlabeled", 500, vec![]));
    staging.add_event(event("e6", "close", 600, vec![]));
    staging.into_ocel().unwrap()
}

fn recipe(steps: Vec<Step>) -> Recipe {
    Recipe {
        name: "test".into(),
        steps,
    }
}

#[test]
fn gratitude_comments_drop_by_regex_and_real_ones_stay() {
    let steps = vec![Step::DropEventsWhere(EventPredicate {
        event_type: Some("comment".into()),
        attr: Some("body".into()),
        matches: Some(r"(?i)^(thanks|thank you|lgtm)\W*$".into()),
        ..EventPredicate::default()
    })];
    let (log, reports) = apply(&recipe(steps), sample(), Path::new(".")).unwrap();
    assert_eq!(log.validate(), Ok(()));
    assert_eq!(reports[0].events_before, 6);
    assert_eq!(reports[0].events_after, 5);
    let bodies: Vec<String> = log
        .events
        .iter()
        .filter(|e| e.event_type == "comment")
        .flat_map(|e| e.attributes.iter().map(|a| a.value.to_text()))
        .collect();
    assert_eq!(bodies, vec!["here is a real repro"]);
}

#[test]
fn rename_merges_labeled_and_unlabeled_into_triage() {
    let steps = vec![Step::RenameEventTypes(
        [
            ("labeled".to_owned(), "triage".to_owned()),
            ("unlabeled".to_owned(), "triage".to_owned()),
        ]
        .into_iter()
        .collect(),
    )];
    let (log, _) = apply(&recipe(steps), sample(), Path::new(".")).unwrap();
    assert_eq!(
        log.events
            .iter()
            .filter(|e| e.event_type == "triage")
            .count(),
        2
    );
    assert!(log.event_types.iter().all(|t| t.name != "labeled"));
}

#[test]
fn time_window_is_half_open_and_keeps_objects() {
    let steps = vec![Step::TimeWindow(TimeWindow {
        from: Some("1970-01-01T00:03:20Z".into()), // ts(200)
        to: Some("1970-01-01T00:08:20Z".into()),   // ts(500), exclusive
    })];
    let (log, _) = apply(&recipe(steps), sample(), Path::new(".")).unwrap();
    let ids: Vec<&str> = log.events.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["e2", "e3", "e4"]);
    // objects are untouched by the window
    assert_eq!(log.objects.len(), 2);
}

#[test]
fn drop_event_types_then_cleanup_removes_orphans() {
    // keeping only comments leaves u1 unreferenced by any event; the cleanup
    // step drops it and strips t1's O2O link to it
    let steps = vec![
        Step::KeepEventTypes(vec!["comment".into()]),
        Step::DropObjectsWithoutEvents,
    ];
    let (log, reports) = apply(&recipe(steps), sample(), Path::new(".")).unwrap();
    assert_eq!(log.validate(), Ok(()));
    assert_eq!(log.events.len(), 2);
    // u1 had no events of its own -> dropped; t1 stays
    let ids: Vec<&str> = log.objects.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, vec!["t1"]);
    // and t1's O2O to the dropped u1 is gone
    assert!(log.objects[0].relationships.is_empty());
    assert_eq!(reports[1].objects_before, 2);
    assert_eq!(reports[1].objects_after, 1);
}

#[test]
fn keep_object_types_drops_unrelated_events() {
    // keeping only "user" objects leaves every event unrelated (all events
    // reference the issue t1) -> events drop with their subject
    let steps = vec![Step::KeepObjectTypes(vec!["user".into()])];
    let (log, _) = apply(&recipe(steps), sample(), Path::new(".")).unwrap();
    assert_eq!(log.validate(), Ok(()));
    assert_eq!(log.events.len(), 0);
    assert_eq!(log.objects.len(), 1);
}

#[test]
fn numeric_range_predicate() {
    let mut staging = StagingLog::new();
    staging.upsert_object("t1", "issue");
    staging.add_event(event(
        "e1",
        "score",
        100,
        vec![("value", AttrValue::Integer(3))],
    ));
    staging.add_event(event(
        "e2",
        "score",
        200,
        vec![("value", AttrValue::Integer(80))],
    ));
    let log = staging.into_ocel().unwrap();

    let steps = vec![Step::DropEventsWhere(EventPredicate {
        attr: Some("value".into()),
        max: Some(10.0),
        ..EventPredicate::default()
    })];
    let (out, _) = apply(&recipe(steps), log, Path::new(".")).unwrap();
    let ids: Vec<&str> = out.events.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["e2"]);
}

#[test]
fn empty_predicate_is_rejected() {
    let steps = vec![Step::DropEventsWhere(EventPredicate::default())];
    let err = apply(&recipe(steps), sample(), Path::new(".")).unwrap_err();
    assert!(err.to_string().contains("no conditions"), "{err}");
}

#[test]
fn value_condition_without_attr_is_rejected() {
    let steps = vec![Step::DropEventsWhere(EventPredicate {
        event_type: Some("comment".into()),
        matches: Some("x".into()),
        ..EventPredicate::default()
    })];
    let err = apply(&recipe(steps), sample(), Path::new(".")).unwrap_err();
    assert!(err.to_string().contains("require `attr`"), "{err}");
}

#[test]
fn full_recipe_parses_from_json_and_applies() {
    let json = r#"{
      "name": "clean",
      "steps": [
        { "dropEventTypes": ["open"] },
        { "dropEventsWhere": { "eventType": "comment", "attr": "body",
                               "matches": "(?i)^thanks\\W*$" } },
        { "renameEventTypes": { "labeled": "triage", "unlabeled": "triage" } },
        { "timeWindow": { "from": "1970-01-01", "to": "1970-01-01" } },
        { "keepObjectTypes": ["issue", "user"] },
        "dropObjectsWithoutEvents"
      ]
    }"#;
    let recipe: Recipe = serde_json::from_str(json).unwrap();
    assert_eq!(recipe.steps.len(), 6);
    let (log, reports) = apply(&recipe, sample(), Path::new(".")).unwrap();
    assert_eq!(log.validate(), Ok(()));
    assert_eq!(reports.len(), 6);
    // the whole sample happens on 1970-01-01, whose date-window keeps it all
    assert_eq!(reports[3].events_before, reports[3].events_after);
}

#[test]
fn unknown_recipe_fields_fail_loudly() {
    let json =
        r#"{ "name": "x", "steps": [ { "dropEventsWhere": { "attr": "a", "regex": "y" } } ] }"#;
    assert!(serde_json::from_str::<Recipe>(json).is_err()); // "regex" is not a field
}

#[test]
fn preview_samples_the_dropped_events() {
    use ocel_transform::preview;

    let steps = vec![Step::DropEventsWhere(EventPredicate {
        event_type: Some("comment".into()),
        attr: Some("body".into()),
        matches: Some(r"(?i)^thanks\W*$".into()),
        ..EventPredicate::default()
    })];
    let (log, previews) = preview(&recipe(steps), sample(), Path::new("."), 10).unwrap();
    assert_eq!(log.events.len(), 5);
    assert_eq!(previews[0].dropped_total, 1);
    let dropped = &previews[0].dropped_events;
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].event_type, "comment");
    assert_eq!(dropped[0].attributes[0].1, "thanks!");
}

/// Identity resolution as data: two ids alias to one canonical object,
/// every reference follows, unmapped ids stay as they are.
#[test]
fn map_object_ids_applies_the_alias_table() {
    use ocel_transform::AliasTable;

    let mut staging = StagingLog::new();
    staging.upsert_object("alice@corp", "user");
    staging.upsert_object("@alice", "user");
    staging.upsert_object("t1", "issue");
    staging.add_o2o("t1", "@alice", "assigned to");
    staging.add_event(StagingEvent {
        id: "e1".into(),
        event_type: "comment".into(),
        time: ts(100),
        attributes: vec![],
        relations: vec![
            ("t1".into(), "subject".into()),
            ("alice@corp".into(), "actor".into()),
        ],
    });
    let log = staging.into_ocel().unwrap();

    let steps = vec![Step::MapObjectIds(AliasTable {
        aliases: [
            ("alice@corp".to_owned(), "user:alice".to_owned()),
            ("@alice".to_owned(), "user:alice".to_owned()),
        ]
        .into_iter()
        .collect(),
    })];
    let (out, reports) = apply(&recipe(steps), log, Path::new(".")).unwrap();
    assert_eq!(out.validate(), Ok(()));
    assert_eq!(reports[0].objects_before, 3);
    assert_eq!(reports[0].objects_after, 2); // the two aliases merged
    assert!(out.objects.iter().any(|o| o.id == "user:alice"));
    assert!(out
        .events
        .iter()
        .all(|e| e.relationships.iter().all(|r| r.object_id != "alice@corp")));
    assert!(out
        .o2o()
        .any(|r| r.source_id == "t1" && r.target_id == "user:alice"));
}

/// The JSON spelling of the alias step round-trips through serde.
#[test]
fn map_object_ids_parses_from_json() {
    let json = r#"{ "name": "identity", "steps": [
      { "mapObjectIds": { "aliases": { "alice@corp": "user:alice" } } }
    ] }"#;
    let recipe: Recipe = serde_json::from_str(json).unwrap();
    assert_eq!(recipe.steps.len(), 1);
    assert_eq!(recipe.steps[0].label(), "mapObjectIds");
}
