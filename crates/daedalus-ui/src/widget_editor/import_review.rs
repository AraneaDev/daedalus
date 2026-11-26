use super::WidgetEditor;
use daedalus_proto::workbench::WidgetResult;

#[derive(Clone, Debug, Default)]
pub(super) struct ImportReview {
    pub(super) token: Option<uuid::Uuid>,
    lines: Vec<String>,
}

impl WidgetEditor {
    pub(crate) fn start_import_review(&mut self) -> uuid::Uuid {
        let token = uuid::Uuid::new_v4();
        self.import_review.token = Some(token);
        token
    }

    pub fn finish_import_review(&mut self, token: uuid::Uuid) -> bool {
        if !self.sharing || self.import_review.token != Some(token) {
            return false;
        }
        self.import_review.token = None;
        true
    }

    pub(crate) fn is_reviewing_import(&self) -> bool {
        self.import_review.token.is_some()
    }

    pub(crate) fn import_lines(&self) -> &[String] {
        &self.import_review.lines
    }

    pub fn show_import_review(&mut self, result: WidgetResult) {
        let lines = &mut self.import_review.lines;
        lines.clear();
        for (label, value) in &result.details {
            let rows = wrap_review_row(&format!("{label} · {value}"));
            if rows.len() > 1 {
                let value_rows = wrap_review_row(value);
                if value_rows.len() == 1 {
                    lines.extend(wrap_review_row(label));
                    lines.extend(value_rows);
                    continue;
                }
            }
            lines.extend(rows);
        }
        self.preview = Some(result);
        self.detail_page = 0;
    }

    pub fn clear_import_review(&mut self) {
        self.import = None;
        self.creation = None;
        self.import_review.token = None;
        self.import_review.lines.clear();
        self.resolutions.clear();
        self.detail_page = 0;
        self.clear_preview();
    }
}

fn wrap_review_row(text: &str) -> Vec<String> {
    // Fit long argument and environment values into readable settings rows.
    const MAX_COLUMNS: usize = 34;
    let mut printable = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_control() {
            printable.extend(character.escape_default());
        } else {
            printable.push(character);
        }
    }
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut columns = 0;
    for character in printable.chars() {
        let width = if character.is_ascii() && !matches!(character, 'M' | 'W' | 'm' | 'w' | '@') {
            1
        } else {
            2
        };
        if columns + width > MAX_COLUMNS {
            rows.push(std::mem::take(&mut row));
            columns = 0;
        }
        row.push(character);
        columns += width;
    }
    rows.push(row);
    rows
}
