use super::WidgetDraft;
use daedalus_proto::workbench::{WidgetElement, WidgetField};

#[derive(Clone, Debug)]
pub(crate) struct ElementDraft {
    pub element: WidgetElement,
    pub maximum: String,
    pub caption: String,
}

impl ElementDraft {
    pub fn new(element: WidgetElement) -> Self {
        let maximum = match &element {
            WidgetElement::Progress { max, .. } => max.to_string(),
            _ => String::new(),
        };
        let caption = element
            .value_field()
            .and_then(|field| field.label.clone())
            .unwrap_or_default();
        Self {
            element,
            maximum,
            caption,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self.element {
            WidgetElement::Label { .. } => "Label",
            WidgetElement::Status => "Status",
            WidgetElement::Counter { .. } => "Counter",
            WidgetElement::Progress { .. } => "Progress",
            WidgetElement::Button { .. } => "Button",
        }
    }

    pub fn build(&self) -> Result<WidgetElement, String> {
        let mut element = self.element.clone();
        if let Some(field) = value_field_mut(&mut element)
            && field.label.as_deref().unwrap_or_default() != self.caption
        {
            field.label = (!self.caption.is_empty()).then(|| self.caption.clone());
        }
        if let WidgetElement::Progress { max, .. } = &mut element {
            *max = self
                .maximum
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|value| (1..=i64::MAX as u64).contains(value))
                .ok_or_else(|| format!("Enter a progress maximum between 1 and {}.", i64::MAX))?;
        }
        Ok(element)
    }

    pub fn value_field_mut(&mut self) -> Option<&mut WidgetField> {
        value_field_mut(&mut self.element)
    }
}

fn value_field_mut(element: &mut WidgetElement) -> Option<&mut WidgetField> {
    match element {
        WidgetElement::Counter { field } | WidgetElement::Progress { field, .. } => Some(field),
        _ => None,
    }
}

impl WidgetDraft {
    pub(crate) fn selected_element(&self) -> Option<&ElementDraft> {
        self.elements.get(self.editing_element?)
    }

    pub(crate) fn selected_element_mut(&mut self) -> Option<&mut ElementDraft> {
        self.elements.get_mut(self.editing_element?)
    }

    pub(crate) fn add_element(&mut self, element: WidgetElement) {
        self.editing_element = Some(self.elements.len());
        self.elements.push(ElementDraft::new(element));
    }
}
