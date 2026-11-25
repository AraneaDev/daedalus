use daedalus_proto::workbench::{ConflictChoice, MergeConflict, WorkspaceDocument};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub fn merge_workspace(
    base: &WorkspaceDocument,
    local: &WorkspaceDocument,
    incoming: &WorkspaceDocument,
    resolutions: &BTreeMap<String, ConflictChoice>,
) -> Result<(WorkspaceDocument, Vec<MergeConflict>), serde_json::Error> {
    let base = serde_json::to_value(base)?;
    let local = serde_json::to_value(local)?;
    let incoming = serde_json::to_value(incoming)?;
    let mut merge = Merge {
        resolutions,
        conflicts: Vec::new(),
    };
    let value = merge.value("", Some(&base), Some(&local), Some(&incoming));
    Ok((
        serde_json::from_value(value.unwrap_or(Value::Null))?,
        merge.conflicts,
    ))
}

struct Merge<'a> {
    resolutions: &'a BTreeMap<String, ConflictChoice>,
    conflicts: Vec<MergeConflict>,
}

impl Merge<'_> {
    fn value(
        &mut self,
        path: &str,
        base: Option<&Value>,
        local: Option<&Value>,
        incoming: Option<&Value>,
    ) -> Option<Value> {
        if local == incoming || base == incoming {
            return local.cloned();
        }
        if base == local {
            return incoming.cloned();
        }
        if let (
            Some(Value::Object(base)),
            Some(Value::Object(local)),
            Some(Value::Object(incoming)),
        ) = (base, local, incoming)
        {
            return Some(Value::Object(self.object(path, base, local, incoming)));
        }
        if let (Some(Value::Array(base)), Some(Value::Array(local)), Some(Value::Array(incoming))) =
            (base, local, incoming)
            && is_id_collection(path)
            && let (Some(base), Some(local), Some(incoming)) = (
                Collection::new(base),
                Collection::new(local),
                Collection::new(incoming),
            )
        {
            return Some(Value::Array(self.collection(path, base, local, incoming)));
        }
        match self.resolutions.get(path) {
            Some(ConflictChoice::Local) => local.cloned(),
            Some(ConflictChoice::Incoming) => incoming.cloned(),
            None => {
                self.conflicts.push(MergeConflict {
                    path: path.into(),
                    base: base.map(Value::to_string),
                    local: local.map(Value::to_string),
                    incoming: incoming.map(Value::to_string),
                });
                local.cloned()
            }
        }
    }

    fn object(
        &mut self,
        path: &str,
        base: &Map<String, Value>,
        local: &Map<String, Value>,
        incoming: &Map<String, Value>,
    ) -> Map<String, Value> {
        let keys: BTreeSet<_> = base
            .keys()
            .chain(local.keys())
            .chain(incoming.keys())
            .collect();
        keys.into_iter()
            .filter_map(|key| {
                let value = self.value(
                    &child(path, key),
                    base.get(key),
                    local.get(key),
                    incoming.get(key),
                )?;
                Some((key.clone(), value))
            })
            .collect()
    }

    fn collection(
        &mut self,
        path: &str,
        base: Collection<'_>,
        local: Collection<'_>,
        incoming: Collection<'_>,
    ) -> Vec<Value> {
        let common: BTreeSet<_> = base
            .order
            .iter()
            .copied()
            .filter(|id| local.values.contains_key(id) && incoming.values.contains_key(id))
            .collect();
        let common_order = |collection: &Collection<'_>| {
            Value::Array(
                collection
                    .order
                    .iter()
                    .filter(|id| common.contains(*id))
                    .map(|id| Value::String((*id).into()))
                    .collect(),
            )
        };
        let base_order = common_order(&base);
        let local_order = common_order(&local);
        let incoming_order = common_order(&incoming);
        let order = self.value(
            &child(path, "$order"),
            Some(&base_order),
            Some(&local_order),
            Some(&incoming_order),
        );
        let prefer_incoming =
            order.as_ref() == Some(&incoming_order) && incoming_order != local_order;
        let (first, second) = if prefer_incoming {
            (&incoming, &local)
        } else {
            (&local, &incoming)
        };
        let mut seen = BTreeSet::new();
        first
            .order
            .iter()
            .chain(&second.order)
            .chain(&base.order)
            .filter_map(|id| {
                if !seen.insert(*id) {
                    return None;
                }
                self.value(
                    &child(path, id),
                    base.values.get(id).copied(),
                    local.values.get(id).copied(),
                    incoming.values.get(id).copied(),
                )
            })
            .collect()
    }
}

struct Collection<'a> {
    order: Vec<&'a str>,
    values: BTreeMap<&'a str, &'a Value>,
}

impl<'a> Collection<'a> {
    fn new(items: &'a [Value]) -> Option<Self> {
        let mut collection = Self {
            order: Vec::with_capacity(items.len()),
            values: BTreeMap::new(),
        };
        for item in items {
            let id = item.get("id")?.as_str()?;
            collection.order.push(id);
            collection.values.insert(id, item);
        }
        Some(collection)
    }
}

fn is_id_collection(path: &str) -> bool {
    matches!(
        path.rsplit('/').next(),
        Some("widgets" | "templates" | "assignments" | "sources" | "actions")
    )
}

fn child(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}
