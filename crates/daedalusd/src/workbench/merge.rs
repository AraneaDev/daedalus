mod references;

use daedalus_proto::workbench::{
    ConflictChoice, MergeConflict, WidgetAppearance, WorkspaceDocument,
};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub fn merge_workspace(
    base: &WorkspaceDocument,
    local: &WorkspaceDocument,
    incoming: &WorkspaceDocument,
    resolutions: &BTreeMap<String, ConflictChoice>,
) -> Result<(WorkspaceDocument, Vec<MergeConflict>), serde_json::Error> {
    let mut merge = Merge {
        resolutions,
        conflicts: Vec::new(),
    };
    let value = merge.value(
        "",
        Some(&serde_json::to_value(base)?),
        Some(&serde_json::to_value(local)?),
        Some(&serde_json::to_value(incoming)?),
    );
    let mut document = serde_json::from_value(value.unwrap_or(Value::Null))?;
    merge.references(&mut document, base, local, incoming);
    Ok((document, merge.conflicts))
}

struct Merge<'a> {
    resolutions: &'a BTreeMap<String, ConflictChoice>,
    conflicts: Vec<MergeConflict>,
}

impl Merge<'_> {
    fn side<T: Serialize>(
        &mut self,
        path: String,
        base: Option<&T>,
        local: Option<&T>,
        incoming: Option<&T>,
    ) -> ConflictChoice {
        if let Some(choice) = self.resolutions.get(&path) {
            return *choice;
        }
        let display = |value: Option<&T>| value.and_then(|value| serde_json::to_string(value).ok());
        self.conflicts.push(MergeConflict {
            path,
            base: display(base),
            local: display(local),
            incoming: display(incoming),
        });
        ConflictChoice::Local
    }

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
        // Portable definitions omit the default appearance. Restore it for
        // merging so individual edits can combine even across the default.
        if path.ends_with("/display/appearance") {
            let default = serde_json::json!(WidgetAppearance::default());
            if let (Value::Object(base), Value::Object(local), Value::Object(incoming)) = (
                base.unwrap_or(&default),
                local.unwrap_or(&default),
                incoming.unwrap_or(&default),
            ) {
                return Some(Value::Object(self.object(path, base, local, incoming)));
            }
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
        match self.side(path.into(), base, local, incoming) {
            ConflictChoice::Local => local.cloned(),
            ConflictChoice::Incoming => incoming.cloned(),
        }
    }

    fn object(
        &mut self,
        path: &str,
        base: &Map<String, Value>,
        local: &Map<String, Value>,
        incoming: &Map<String, Value>,
    ) -> Map<String, Value> {
        if let (Some(local_kind), Some(incoming_kind), Some(_)) = (
            local.get("kind").and_then(Value::as_str),
            incoming.get("kind").and_then(Value::as_str),
            base.get("kind").and_then(Value::as_str),
        ) && local_kind != incoming_kind
        {
            return self.tagged_variant(path, base, local, incoming);
        }
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

    fn tagged_variant(
        &mut self,
        path: &str,
        base: &Map<String, Value>,
        local: &Map<String, Value>,
        incoming: &Map<String, Value>,
    ) -> Map<String, Value> {
        // An enum tag and its payload must come from the same side. The identity
        // and label of a flattened named source or action can still merge separately.
        const COMMON: [&str; 2] = ["id", "label"];
        fn payload(record: &Map<String, Value>) -> Map<String, Value> {
            record
                .iter()
                .filter(|(key, _)| !COMMON.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        }
        let (base_payload, local_payload, incoming_payload) =
            (payload(base), payload(local), payload(incoming));
        let mut merged = if base_payload == incoming_payload {
            local_payload
        } else if base_payload == local_payload {
            incoming_payload
        } else {
            match self.side(
                child(path, "$variant"),
                Some(&base_payload),
                Some(&local_payload),
                Some(&incoming_payload),
            ) {
                ConflictChoice::Local => local_payload,
                ConflictChoice::Incoming => incoming_payload,
            }
        };
        for key in COMMON {
            if let Some(value) = self.value(
                &child(path, key),
                base.get(key),
                local.get(key),
                incoming.get(key),
            ) {
                merged.insert(key.into(), value);
            }
        }
        merged
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
        let mut values: BTreeMap<_, _> = first
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
                .map(|value| (*id, value))
            })
            .collect();
        merge_collection_order(&first.order, &second.order, &base.order, &values)
            .into_iter()
            .filter_map(|id| values.remove(id))
            .collect()
    }
}

fn merge_collection_order<'a>(
    first: &[&'a str],
    second: &[&'a str],
    base: &[&'a str],
    values: &BTreeMap<&str, Value>,
) -> Vec<&'a str> {
    let surviving = |id: &&str| values.contains_key(id);
    let mut order: Vec<_> = first.iter().copied().filter(surviving).collect();
    for side in [second, base] {
        let ordered: BTreeSet<_> = order.iter().copied().collect();
        let mut before = BTreeMap::new();
        let mut additions = Vec::new();
        for id in side.iter().copied().filter(surviving) {
            if ordered.contains(id) {
                if !additions.is_empty() {
                    before.insert(id, std::mem::take(&mut additions));
                }
            } else {
                additions.push(id);
            }
        }
        // Preserve the chosen order of existing entries. An insertion stays
        // before its next surviving neighbor, and an appended entry stays last.
        order = order
            .into_iter()
            .flat_map(|id| before.remove(id).into_iter().flatten().chain([id]))
            .chain(additions)
            .collect();
    }
    order
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn action_variant_and_independent_label_changes_merge_without_conflict() {
        let base = json!({"id": "open", "label": "Open", "kind": "diff"});
        let rename = json!({"id": "open", "label": "Inspect", "kind": "diff"});
        let variant = json!({
            "id": "open", "label": "Open", "kind": "open_url", "url": "https://example.invalid"
        });
        let expected = json!({
            "id": "open", "label": "Inspect", "kind": "open_url", "url": "https://example.invalid"
        });
        for (local, incoming) in [(&rename, &variant), (&variant, &rename)] {
            let mut merge = Merge {
                resolutions: &BTreeMap::new(),
                conflicts: Vec::new(),
            };
            assert_eq!(
                merge.value("/actions/open", Some(&base), Some(local), Some(incoming)),
                Some(expected.clone())
            );
            assert!(merge.conflicts.is_empty());
        }
    }
}
