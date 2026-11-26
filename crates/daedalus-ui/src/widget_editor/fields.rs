use daedalus_proto::workbench::WidgetField;
use std::collections::{HashMap, VecDeque};

const SYNTAX: &str =
    "Use source:field, for example main:branch. Quote field names containing commas.";

pub(super) fn format_fields(fields: &[WidgetField]) -> String {
    fields
        .iter()
        .map(|field| {
            let name = &field.field;
            let selector = if name.contains(',')
                || name.starts_with('"')
                || name.trim() != name
                || name.chars().any(char::is_control)
            {
                serde_json::Value::String(name.clone()).to_string()
            } else {
                name.clone()
            };
            format!("{}:{selector}", field.source)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn edit_fields(
    text: &str,
    previous: &[WidgetField],
) -> Result<Vec<WidgetField>, String> {
    if text == format_fields(previous) {
        return Ok(previous.to_vec());
    }

    let mut labels = HashMap::<_, HashMap<_, VecDeque<_>>>::new();
    for field in previous {
        labels
            .entry(field.source.as_str())
            .or_default()
            .entry(field.field.as_str())
            .or_default()
            .push_back(field.label.clone());
    }

    let mut fields = Vec::new();
    let mut remaining = text;
    loop {
        remaining = remaining.trim_start_matches(|c: char| c == ',' || c.is_whitespace());
        if remaining.is_empty() {
            return Ok(fields);
        }
        let (source, rest) = remaining.split_once(':').ok_or(SYNTAX)?;
        if source.contains(',') {
            return Err(SYNTAX.into());
        }
        let source = source.trim();
        let (field, tail) = parse_selector(rest.trim_start())?;
        let label = labels
            .get_mut(source)
            .and_then(|labels| labels.get_mut(field.as_str()))
            .and_then(VecDeque::pop_front)
            .flatten();
        fields.push(WidgetField {
            source: source.into(),
            field,
            label,
        });
        remaining = tail;
    }
}

fn parse_selector(input: &str) -> Result<(String, &str), String> {
    if input.starts_with('"') {
        let mut string = serde_json::Deserializer::from_str(input).into_iter::<String>();
        let field = string.next().ok_or(SYNTAX)?.map_err(|_| SYNTAX)?;
        let remaining = input[string.byte_offset()..].trim_start();
        if remaining.is_empty() || remaining.starts_with(',') {
            return Ok((field, remaining));
        }
        return Err(SYNTAX.into());
    }
    let (field, remaining) = input.split_once(',').unwrap_or((input, ""));
    Ok((field.trim().into(), remaining))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(selector: &str, label: Option<&str>) -> WidgetField {
        WidgetField {
            source: "main".into(),
            field: selector.into(),
            label: label.map(str::to_owned),
        }
    }

    #[test]
    fn edits_keep_existing_labels_by_selector_and_occurrence() {
        let previous = [
            field("branch", Some("Current branch")),
            field("json.a,b", Some("Report")),
            field("branch", Some("Branch copy")),
        ];
        let before = format_fields(&previous);
        assert_eq!(before, r#"main:branch, main:"json.a,b", main:branch"#);
        assert_eq!(edit_fields(&before, &previous).unwrap(), previous);
        let edited = edit_fields(
            r#"main:"json.a,b", main:branch, main:ahead, main:branch"#,
            &previous,
        )
        .unwrap();
        assert_eq!(
            edited,
            [
                previous[1].clone(),
                previous[0].clone(),
                field("ahead", None),
                previous[2].clone()
            ]
        );
    }
}
