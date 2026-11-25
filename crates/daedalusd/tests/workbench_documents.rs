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
