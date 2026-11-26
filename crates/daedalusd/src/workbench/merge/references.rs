use super::{Merge, child};
use daedalus_proto::workbench::{
    ConflictChoice, NamedSource, WidgetDefinition, WidgetField, WidgetRule, WorkspaceDocument,
};
use serde::Serialize;
use std::collections::BTreeSet;

impl Merge<'_> {
    pub(super) fn references(
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
        let known: BTreeSet<_> = definition
            .sources
            .iter()
            .map(|source| source.id.as_str())
            .collect();
        let dangling: BTreeSet<_> = definition
            .display
            .summary
            .iter()
            .chain(&definition.display.details)
            .chain(definition.rules.iter().map(|rule| &rule.field))
            .filter(|field| !known.contains(field.source.as_str()))
            .map(|field| field.source.clone())
            .collect();
        if dangling.is_empty() {
            return;
        }

        let affected = |id: &str| dangling.contains(id);
        let base = base.map(|definition| SourceReferences::affected(definition, affected));
        let local = local.map(|definition| SourceReferences::affected(definition, affected));
        let incoming = incoming.map(|definition| SourceReferences::affected(definition, affected));
        let chosen = match self.side(
            child(&path, "$source_references"),
            base.as_ref(),
            local.as_ref(),
            incoming.as_ref(),
        ) {
            ConflictChoice::Local => local,
            ConflictChoice::Incoming => incoming,
        }
        .unwrap_or_default();
        definition
            .sources
            .extend(chosen.sources.into_iter().cloned());
        replace_affected(&mut definition.display.summary, chosen.summary, |field| {
            affected(&field.source)
        });
        replace_affected(&mut definition.display.details, chosen.details, |field| {
            affected(&field.source)
        });
        replace_affected(&mut definition.rules, chosen.rules, |rule| {
            affected(&rule.field.source)
        });
    }
}

#[derive(Default, Serialize)]
struct SourceReferences<'a> {
    sources: Vec<&'a NamedSource>,
    summary: Vec<&'a WidgetField>,
    details: Vec<&'a WidgetField>,
    rules: Vec<&'a WidgetRule>,
}

impl<'a> SourceReferences<'a> {
    fn affected(definition: &'a WidgetDefinition, includes: impl Fn(&str) -> bool) -> Self {
        Self {
            sources: definition
                .sources
                .iter()
                .filter(|source| includes(&source.id))
                .collect(),
            summary: definition
                .display
                .summary
                .iter()
                .filter(|field| includes(&field.source))
                .collect(),
            details: definition
                .display
                .details
                .iter()
                .filter(|field| includes(&field.source))
                .collect(),
            rules: definition
                .rules
                .iter()
                .filter(|rule| includes(&rule.field.source))
                .collect(),
        }
    }
}

fn replace_affected<'a, T: Clone + 'a>(
    items: &mut Vec<T>,
    replacement: Vec<&'a T>,
    affected: impl Fn(&T) -> bool,
) {
    let mut replacement = replacement.into_iter();
    items.retain_mut(|item| {
        if !affected(item) {
            return true;
        }
        if let Some(selected) = replacement.next() {
            item.clone_from(selected);
            true
        } else {
            false
        }
    });
    items.extend(replacement.cloned());
}
