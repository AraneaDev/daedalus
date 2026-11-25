use daedalus_proto::workbench::{
    ConflictChoice, MergeConflict, WidgetDefinition, WorkspaceDocument,
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

    fn references(
        &mut self,
        document: &mut WorkspaceDocument,
        base: &WorkspaceDocument,
        local: &WorkspaceDocument,
        incoming: &WorkspaceDocument,
    ) {
        let widget_path = |id: &str| child("/widgets", id);
        // Reconcile template references per widget. Run in original order so the
        // same missing reference always yields the same choice in preview and apply.
        let available_templates: BTreeSet<_> =
            document.templates.iter().map(|t| &t.id).cloned().collect();
        let needs_repair =
            |widget: &daedalus_proto::workbench::WidgetInstance| match &widget.template {
                Some(id) => !available_templates.contains(id),
                None => widget.definition.is_none(),
            };
        document.widgets.retain_mut(|widget| {
            if !needs_repair(widget) {
                return true;
            }
            let path = child(&widget_path(&widget.id), "$template_reference");
            let base_widget = base.widgets.iter().find(|w| w.id == widget.id);
            let local_widget = local.widgets.iter().find(|w| w.id == widget.id);
            let incoming_widget = incoming.widgets.iter().find(|w| w.id == widget.id);
            let (side, replacement) =
                match self.side(path, base_widget, local_widget, incoming_widget) {
                    ConflictChoice::Local => (local, local_widget),
                    ConflictChoice::Incoming => (incoming, incoming_widget),
                };
            let Some(replacement) = replacement else {
                return false;
            };
            widget.template.clone_from(&replacement.template);
            if widget.template.is_none() {
                widget.definition.clone_from(&replacement.definition);
            }
            if let Some(id) = &widget.template
                && !document.templates.iter().any(|template| template.id == *id)
                && let Some(template) = side.templates.iter().find(|template| template.id == *id)
            {
                document.templates.push(template.clone());
            }
            true
        });

        for template in &mut document.templates {
            let lookup = |document: &'_ WorkspaceDocument| {
                document.templates.iter().position(|t| t.id == template.id)
            };
            self.source_references(
                child(&child("/templates", &template.id), "definition"),
                &mut template.definition,
                lookup(base).map(|i| &base.templates[i].definition),
                lookup(local).map(|i| &local.templates[i].definition),
                lookup(incoming).map(|i| &incoming.templates[i].definition),
            );
        }
        for widget in &mut document.widgets {
            let Some(definition) = &mut widget.definition else {
                continue;
            };
            let lookup = |document: &'_ WorkspaceDocument| {
                document.widgets.iter().position(|w| w.id == widget.id)
            };
            self.source_references(
                child(&widget_path(&widget.id), "definition"),
                definition,
                lookup(base).and_then(|i| base.widgets[i].definition.as_ref()),
                lookup(local).and_then(|i| local.widgets[i].definition.as_ref()),
                lookup(incoming).and_then(|i| incoming.widgets[i].definition.as_ref()),
            );
        }
    }

    fn source_references(
        &mut self,
        path: String,
        definition: &mut WidgetDefinition,
        base: Option<&WidgetDefinition>,
        local: Option<&WidgetDefinition>,
        incoming: Option<&WidgetDefinition>,
    ) {
        let has_dangling = definition
            .display
            .summary
            .iter()
            .chain(&definition.display.details)
            .chain(definition.rules.iter().map(|rule| &rule.field))
            .any(|field| {
                !definition
                    .sources
                    .iter()
                    .any(|source| source.id == field.source)
            });
        if !has_dangling {
            return;
        }
        let chosen = match self.side(child(&path, "$source_references"), base, local, incoming) {
            ConflictChoice::Local => local,
            ConflictChoice::Incoming => incoming,
        };
        if let Some(chosen) = chosen {
            definition.sources.clone_from(&chosen.sources);
            definition.display.clone_from(&chosen.display);
            definition.rules.clone_from(&chosen.rules);
        } else {
            let known = |field: &daedalus_proto::workbench::WidgetField| {
                definition
                    .sources
                    .iter()
                    .any(|source| source.id == field.source)
            };
            definition.display.summary.retain(&known);
            definition.display.details.retain(&known);
            definition.rules.retain(|rule| known(&rule.field));
        }
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
