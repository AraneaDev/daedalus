use daedalus_proto::{PaneId, PaneRole, WorkspaceId, default_config_file_path};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub id: String,
    pub name: String,
    pub role: PaneRole,
    pub argv: Vec<String>,
}

#[derive(Deserialize)]
struct StoredAssignments {
    assignments: Vec<Assignment>,
}

#[derive(Serialize)]
struct AssignmentList<'a> {
    assignments: &'a [Assignment],
}

fn workspace_path(id: WorkspaceId) -> PathBuf {
    default_config_file_path()
        .with_file_name("assignments")
        .join(format!("{id}.toml"))
}

fn validate(assignments: &[Assignment]) -> Result<(), String> {
    let mut ids = HashSet::new();
    for assignment in assignments {
        if assignment.id.trim().is_empty() || !ids.insert(&assignment.id) {
            return Err("Each assignment needs a unique, nonempty ID".into());
        }
        if assignment.name.trim().is_empty() {
            return Err("Each assignment needs a name".into());
        }
        if assignment
            .argv
            .first()
            .is_none_or(|arg| arg.trim().is_empty())
        {
            return Err("Each assignment needs an executable".into());
        }
        if assignment.argv.iter().any(|arg| arg.contains('\0')) {
            return Err("Assignment arguments cannot contain a NUL character".into());
        }
    }
    Ok(())
}

pub fn load(workspace_id: WorkspaceId) -> Result<Vec<Assignment>, String> {
    load_or(workspace_id, Vec::new())
}

pub fn load_or(
    workspace_id: WorkspaceId,
    defaults: Vec<Assignment>,
) -> Result<Vec<Assignment>, String> {
    load_from(&workspace_path(workspace_id), defaults)
}

fn load_from(path: &Path, defaults: Vec<Assignment>) -> Result<Vec<Assignment>, String> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(defaults),
        Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
    };
    let stored: StoredAssignments = toml::from_str(&contents)
        .map_err(|error| format!("Invalid assignments in {}: {error}", path.display()))?;
    validate(&stored.assignments)?;
    Ok(stored.assignments)
}

pub fn save(workspace_id: WorkspaceId, assignments: &[Assignment]) -> Result<(), String> {
    save_to(&workspace_path(workspace_id), assignments)
}

fn save_to(path: &Path, assignments: &[Assignment]) -> Result<(), String> {
    validate(assignments)?;
    let contents = toml::to_string(&AssignmentList { assignments })
        .map_err(|error| format!("Could not encode assignments: {error}"))?;
    write_atomic(path, &contents)
}

pub fn bind(workspace_id: WorkspaceId, pane_id: PaneId, assignment_id: &str) -> Result<(), String> {
    if assignment_id.trim().is_empty() {
        return Err("A pane binding needs an assignment ID".into());
    }
    let path = workspace_path(workspace_id)
        .with_extension("bindings")
        .join(format!("{pane_id}.id"));
    write_atomic(&path, assignment_id)
}

pub fn load_bindings(workspace_id: WorkspaceId) -> Result<HashMap<PaneId, String>, String> {
    read_bindings(&workspace_path(workspace_id).with_extension("bindings"))
}

fn read_bindings(directory: &Path) -> Result<HashMap<PaneId, String>, String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(format!("Could not read {}: {error}", directory.display())),
    };
    let mut bindings = HashMap::new();
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("id") {
            continue;
        }
        let pane = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or_else(|| format!("Invalid pane binding file {}", path.display()))?;
        let assignment = fs::read_to_string(&path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        if assignment.trim().is_empty() {
            return Err(format!("Empty pane binding in {}", path.display()));
        }
        bindings.insert(PaneId(pane), assignment);
    }
    Ok(bindings)
}

pub(crate) fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let directory = path.parent().ok_or("Missing assignments directory")?;
    fs::create_dir_all(directory)
        .map_err(|error| format!("Could not create {}: {error}", directory.display()))?;
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| format!("Could not create {}: {error}", temporary.display()))?;
    let written = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written.and_then(|()| fs::rename(&temporary, path)) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("Could not save {}: {error}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assignment() -> Assignment {
        Assignment {
            id: "one".into(),
            name: "Check workspace".into(),
            role: PaneRole::Runner,
            argv: vec!["cargo".into(), "a b".into(), "".into()],
        }
    }

    #[test]
    fn saves_replaces_and_clears_a_workspace_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("assignments/workspace.toml");
        assert!(load_from(&path, Vec::new()).unwrap().is_empty());
        let first = assignment();
        assert_eq!(
            load_from(&path, vec![first.clone()]).unwrap(),
            vec![first.clone()]
        );
        save_to(&path, std::slice::from_ref(&first)).unwrap();
        assert_eq!(load_from(&path, Vec::new()).unwrap(), vec![first]);
        save_to(&path, &[]).unwrap();
        assert!(load_from(&path, vec![assignment()]).unwrap().is_empty());
        assert_ne!(
            workspace_path(WorkspaceId::new()),
            workspace_path(WorkspaceId::new())
        );
    }

    #[test]
    fn independent_panes_keep_their_latest_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let a = PaneId::new();
        let b = PaneId::new();
        let path = |id: PaneId| dir.path().join(format!("{id}.id"));
        write_atomic(&path(a), "first").unwrap();
        write_atomic(&path(b), "other").unwrap();
        write_atomic(&path(a), "latest").unwrap();
        fs::write(dir.path().join(".pending.tmp"), "partial").unwrap();
        assert_eq!(
            read_bindings(dir.path()).unwrap(),
            HashMap::from([(a, "latest".into()), (b, "other".into())])
        );
    }

    #[test]
    fn rejects_invalid_changes_and_reports_corrupt_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.toml");
        let original = assignment();
        save_to(&path, std::slice::from_ref(&original)).unwrap();
        assert!(save_to(&path, &[original.clone(), original.clone()]).is_err());
        for argv in [vec![], vec![" ".into()], vec!["bad\0arg".into()]] {
            assert!(
                save_to(
                    &path,
                    &[Assignment {
                        argv,
                        ..original.clone()
                    }]
                )
                .is_err()
            );
        }
        assert_eq!(load_from(&path, Vec::new()).unwrap(), vec![original]);
        fs::write(&path, "broken").unwrap();
        assert!(load_from(&path, Vec::new()).is_err());
    }
}
