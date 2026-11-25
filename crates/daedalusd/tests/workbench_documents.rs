use daedalus_proto::workbench::{WORKSPACE_FORMAT_VERSION, WidgetSource};
use daedalusd::workbench::documents::{parse_template, parse_workspace, serialize_workspace};

const WORKSPACE: &str = r#"
version = 1
name = "Rust API"

[[templates]]
id = "git"
name = "Git Worktree"

[[templates.definition.sources]]
id = "git"
kind = "git"

[[templates.definition.display.summary]]
source = "git"
field = "branch"

[[widgets]]
id = "branch"
template = "git"
name = "My branch"
pinned = true

[[widgets]]
id = "review"
template = "git"
name = "Review branch"
pinned = false
"#;

#[test]
fn template_and_workspace_round_trip() {
    let document = parse_workspace(WORKSPACE).unwrap();
    assert_eq!(document.version, WORKSPACE_FORMAT_VERSION);
    assert_eq!(document.widgets.len(), 2);
    assert_eq!(document.widgets[0].name, "My branch");
    assert!(!document.widgets[1].pinned);
    assert!(matches!(
        document.templates[0].definition.sources[0].source,
        WidgetSource::Git
    ));
    assert_eq!(
        parse_workspace(&serialize_workspace(&document).unwrap()).unwrap(),
        document
    );
    let template = parse_template(
        "version = 1\n[template]\nid = 'git'\nname = 'Git Worktree'\n[template.definition]\n",
    )
    .unwrap();
    assert_eq!(template.template.id, "git");
}

#[test]
fn reject_duplicate_identity_and_unknown_version() {
    for (invalid, message) in [
        (WORKSPACE.replace("version = 1", "version = 2"), "version"),
        (
            WORKSPACE.replace("id = \"review\"", "id = \"branch\""),
            "duplicate widget",
        ),
        (
            WORKSPACE.replace("template = \"git\"", "template = \"other\""),
            "unknown template",
        ),
        (
            WORKSPACE.replace("source = \"git\"", "source = \"other\""),
            "unknown source",
        ),
    ] {
        let error = parse_workspace(&invalid).unwrap_err().to_string();
        assert!(error.contains(message), "expected {message}: {error}");
    }
}

#[test]
fn merge_retains_local_edit_and_adds_repo_widget() {
    use daedalusd::workbench::merge::merge_workspace;
    let base = parse_workspace(WORKSPACE).unwrap();
    let mut local = base.clone();
    local.widgets[0].name = "My local title".into();
    let mut incoming = base.clone();
    incoming.widgets[0].pinned = false;
    let mut added = base.widgets[0].clone();
    added.id = "another".into();
    incoming.widgets.push(added);
    let (merged, conflicts) =
        merge_workspace(&base, &local, &incoming, &Default::default()).unwrap();
    assert!(conflicts.is_empty());
    assert_eq!(merged.widgets[0].name, "My local title");
    assert!(!merged.widgets[0].pinned);
    assert_eq!(merged.widgets[2].id, "another");
}

#[test]
fn merge_conflicts_on_remote_delete_of_local_edit() {
    use daedalus_proto::workbench::ConflictChoice;
    use daedalusd::workbench::merge::merge_workspace;
    let base = parse_workspace(WORKSPACE).unwrap();
    let mut local = base.clone();
    local.widgets[0].name = "Local".into();
    let mut incoming = base.clone();
    incoming.widgets.remove(0);
    let (merged, conflicts) =
        merge_workspace(&base, &local, &incoming, &Default::default()).unwrap();
    assert_eq!(merged, local);
    assert_eq!(conflicts[0].path, "/widgets/branch");
    let (resolved, conflicts) = merge_workspace(
        &base,
        &local,
        &incoming,
        &[("/widgets/branch".to_string(), ConflictChoice::Incoming)].into(),
    )
    .unwrap();
    assert!(conflicts.is_empty());
    assert_eq!(resolved, incoming);
}

#[test]
fn widget_fields_render_real_values_and_match_a_rule() {
    use daedalus_proto::StatusLevel;
    use daedalus_proto::workbench::{WidgetCondition, WidgetRule};
    use daedalusd::workbench::runtime::{SourceFields, evaluate_widget};
    let mut definition = parse_workspace(WORKSPACE)
        .unwrap()
        .templates
        .remove(0)
        .definition;
    let field = definition.display.summary[0].clone();
    definition.rules.push(WidgetRule {
        field,
        condition: WidgetCondition::Equals {
            value: "detached".into(),
        },
        level: StatusLevel::Warn,
    });
    let inputs = [(
        "git".into(),
        SourceFields::from([("branch".into(), "detached".into())]),
    )]
    .into();
    let result = evaluate_widget(&definition, &inputs, "Git Worktree");
    assert_eq!(result.label, "detached");
    assert_eq!(result.level, StatusLevel::Warn);
}

#[test]
fn merging_a_new_widget_against_a_deleted_template_offers_two_valid_choices() {
    use daedalus_proto::workbench::ConflictChoice;
    use daedalusd::workbench::documents::validate_workspace;
    use daedalusd::workbench::merge::merge_workspace;

    let mut base = parse_workspace(WORKSPACE).unwrap();
    let mut added = base.widgets.remove(0);
    base.widgets.clear();
    let mut local = base.clone();
    added.id = "new-branch".into();
    local.widgets.push(added);
    let mut incoming = base.clone();
    incoming.name = "New workspace name".into();
    incoming.templates.clear();

    let (preview, conflicts) =
        merge_workspace(&base, &local, &incoming, &Default::default()).unwrap();
    validate_workspace(&preview).unwrap();
    assert_eq!(conflicts.len(), 1);
    let path = &conflicts[0].path;
    assert_eq!(path, "/widgets/new-branch/$template_reference");
    for choice in [ConflictChoice::Local, ConflictChoice::Incoming] {
        let (resolved, conflicts) =
            merge_workspace(&base, &local, &incoming, &[(path.clone(), choice)].into()).unwrap();
        validate_workspace(&resolved).unwrap();
        assert!(conflicts.is_empty());
        assert_eq!(resolved.name, incoming.name);
        assert_eq!(
            resolved.widgets.len(),
            usize::from(choice == ConflictChoice::Local)
        );
        assert_eq!(resolved.templates.len(), resolved.widgets.len());
    }
}

#[test]
fn deleting_a_source_conflicts_with_new_summary_fields_or_rules() {
    use daedalus_proto::workbench::{ConflictChoice, WidgetCondition, WidgetRule};
    use daedalusd::workbench::documents::validate_workspace;
    use daedalusd::workbench::merge::merge_workspace;

    let base = parse_workspace(WORKSPACE).unwrap();
    for rule in [false, true] {
        let mut local = base.clone();
        let definition = &mut local.templates[0].definition;
        let field = definition.display.summary[0].clone();
        if rule {
            definition.rules.push(WidgetRule {
                field,
                condition: WidgetCondition::Missing,
                level: daedalus_proto::StatusLevel::Warn,
            });
        } else {
            definition.display.details.push(field);
        }
        let mut incoming = base.clone();
        let definition = &mut incoming.templates[0].definition;
        definition.sources.clear();
        definition.display.summary.clear();
        let (preview, conflicts) =
            merge_workspace(&base, &local, &incoming, &Default::default()).unwrap();
        assert_eq!(preview, local);
        assert_eq!(conflicts.len(), 1);
        let path = &conflicts[0].path;
        assert_eq!(path, "/templates/git/definition/$source_references");
        for (choice, expected) in [
            (ConflictChoice::Local, &local),
            (ConflictChoice::Incoming, &incoming),
        ] {
            let (resolved, conflicts) =
                merge_workspace(&base, &local, &incoming, &[(path.clone(), choice)].into())
                    .unwrap();
            validate_workspace(&resolved).unwrap();
            assert!(conflicts.is_empty());
            assert_eq!(&resolved, expected);
        }
    }
}

#[test]
fn the_repo_can_restore_a_template_removed_locally_without_duplicating_it() {
    use daedalus_proto::workbench::ConflictChoice;
    use daedalusd::workbench::documents::validate_workspace;
    use daedalusd::workbench::merge::merge_workspace;

    let mut base = parse_workspace(WORKSPACE).unwrap();
    base.widgets.truncate(1);
    base.widgets[0].template = None;
    base.widgets[0].definition = Some(base.templates[0].definition.clone());
    let mut local = base.clone();
    local.templates.clear();

    let mut incoming = base.clone();
    incoming.widgets[0].template = Some("git".into());
    incoming.widgets[0].definition = None;
    let mut second = incoming.widgets[0].clone();
    second.id = "second".into();
    incoming.widgets.push(second);

    for document in [&base, &local, &incoming] {
        validate_workspace(document).unwrap();
    }
    let (proposed, conflicts) =
        merge_workspace(&base, &local, &incoming, &Default::default()).unwrap();
    assert_eq!(proposed, local);
    assert_eq!(
        conflicts
            .iter()
            .map(|conflict| conflict.path.as_str())
            .collect::<Vec<_>>(),
        [
            "/widgets/branch/$template_reference",
            "/widgets/second/$template_reference",
        ]
    );
    assert!(conflicts[1].local.is_none());
    assert!(conflicts[1].incoming.is_some());

    let resolutions = conflicts
        .into_iter()
        .map(|conflict| (conflict.path, ConflictChoice::Incoming))
        .collect();
    let (resolved, remaining) = merge_workspace(&base, &local, &incoming, &resolutions).unwrap();
    validate_workspace(&resolved).unwrap();
    assert!(remaining.is_empty());
    assert_eq!(resolved, incoming);
    assert_eq!(resolved.templates.len(), 1);
}

#[test]
fn divergent_widget_orders_can_resolve_independently_of_their_fields() {
    use daedalus_proto::workbench::ConflictChoice;
    use daedalus_proto::workbench::WorkspaceDocument;
    use daedalusd::workbench::merge::merge_workspace;

    let mut base = parse_workspace(WORKSPACE).unwrap();
    let mut third = base.widgets[0].clone();
    third.id = "status".into();
    base.widgets.push(third);

    let mut local = base.clone();
    local.widgets[0].name = "Edited locally".into();
    local.widgets.swap(0, 1);
    let mut added = base.widgets[0].clone();
    added.id = "local-only".into();
    local.widgets.push(added);

    let mut incoming = base.clone();
    incoming.widgets[0].name = "Edited in repo".into();
    incoming.widgets.rotate_right(1);
    let mut added = base.widgets[0].clone();
    added.id = "repo-only".into();
    incoming.widgets.push(added);

    let order = |doc: &WorkspaceDocument| {
        doc.widgets
            .iter()
            .map(|widget| widget.id.clone())
            .collect::<Vec<_>>()
    };
    let (preview, conflicts) =
        merge_workspace(&base, &local, &incoming, &Default::default()).unwrap();
    assert_eq!(
        order(&preview),
        ["review", "branch", "status", "local-only", "repo-only"]
    );
    assert_eq!(conflicts.len(), 2);
    assert_eq!(conflicts[0].path, "/widgets/$order");
    assert_eq!(
        conflicts[0].base.as_deref(),
        Some(r#"["branch","review","status"]"#)
    );
    assert_eq!(
        conflicts[0].incoming.as_deref(),
        Some(r#"["status","branch","review"]"#)
    );
    assert_eq!(conflicts[1].path, "/widgets/branch/name");
    assert_eq!(conflicts[1].local.as_deref(), Some(r#""Edited locally""#));

    let resolutions = [
        (conflicts[0].path.clone(), ConflictChoice::Incoming),
        (conflicts[1].path.clone(), ConflictChoice::Local),
    ]
    .into();
    let (resolved, remaining) = merge_workspace(&base, &local, &incoming, &resolutions).unwrap();
    assert!(remaining.is_empty());
    assert_eq!(
        order(&resolved),
        ["status", "branch", "review", "repo-only", "local-only"]
    );
    assert_eq!(resolved.widgets[1].name, "Edited locally");
}

#[test]
fn resolving_an_inline_source_conflict_preserves_an_independent_action() {
    use daedalus_proto::{
        StatusLevel,
        workbench::{ConflictChoice, NamedAction, WidgetAction, WidgetCondition, WidgetRule},
    };
    use daedalusd::workbench::documents::validate_workspace;
    use daedalusd::workbench::merge::merge_workspace;

    let mut base = parse_workspace(WORKSPACE).unwrap();
    base.widgets.truncate(1);
    base.widgets[0].template = None;
    base.widgets[0].definition = Some(base.templates.pop().unwrap().definition);

    let mut local = base.clone();
    let definition = local.widgets[0].definition.as_mut().unwrap();
    definition.sources.clear();
    definition.display.summary.clear();
    definition.actions.push(NamedAction {
        id: "diff".into(),
        label: "Diff".into(),
        action: WidgetAction::Diff,
    });
    let mut incoming = base.clone();
    let definition = incoming.widgets[0].definition.as_mut().unwrap();
    definition.rules.push(WidgetRule {
        field: definition.display.summary[0].clone(),
        condition: WidgetCondition::Missing,
        level: StatusLevel::Warn,
    });

    let (proposed, conflicts) =
        merge_workspace(&base, &local, &incoming, &Default::default()).unwrap();
    assert_eq!(proposed, local);
    assert_eq!(conflicts.len(), 1);
    let path = &conflicts[0].path;
    assert_eq!(path, "/widgets/branch/definition/$source_references");

    let (resolved, remaining) = merge_workspace(
        &base,
        &local,
        &incoming,
        &[(path.clone(), ConflictChoice::Incoming)].into(),
    )
    .unwrap();
    validate_workspace(&resolved).unwrap();
    assert!(remaining.is_empty());
    let definition = resolved.widgets[0].definition.as_ref().unwrap();
    assert_eq!(definition.sources[0].id, "git");
    assert_eq!(definition.rules.len(), 1);
    assert_eq!(definition.actions[0].id, "diff");
}
