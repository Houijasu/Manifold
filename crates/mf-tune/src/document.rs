//! A deliberately small subset of TOML, used for both the tuning config and the
//! checkpoint.
//!
//! The workspace has no runtime dependencies outside itself, so a config format means a
//! parser. Rather than write two (a config reader and a checkpoint reader), both file
//! kinds are expressed in the same subset and share this one parser: top-level
//! `key = value` lines plus `[[section]]` arrays of tables. That is exactly enough for
//! "some run-wide settings and a list of parameters", and every construct it does not
//! support is rejected with a line number rather than silently ignored — a tuning run
//! that reads its own config wrongly wastes hours before anyone notices.

use std::fmt::Write as _;

/// A scalar. Integers are kept distinct from decimals so a spin bound reads back as the
/// exact integer the engine advertised.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    Integer(i64),
    Decimal(f64),
}

/// An ordered set of key/value pairs, tagged with where it came from for error messages.
#[derive(Clone, Debug)]
pub struct Table {
    context: String,
    entries: Vec<(String, Value)>,
}

impl Table {
    pub fn new(context: impl Into<String>) -> Self {
        Self {
            context: context.into(),
            entries: Vec::new(),
        }
    }

    pub fn context(&self) -> &str {
        &self.context
    }

    pub fn set_text(&mut self, key: &str, value: impl Into<String>) {
        self.set(key, Value::Text(value.into()));
    }

    pub fn set_integer(&mut self, key: &str, value: i64) {
        self.set(key, Value::Integer(value));
    }

    pub fn set_decimal(&mut self, key: &str, value: f64) {
        self.set(key, Value::Decimal(value));
    }

    fn set(&mut self, key: &str, value: Value) {
        if let Some(existing) = self.entries.iter_mut().find(|(name, _)| name == key) {
            existing.1 = value;
        } else {
            self.entries.push((key.to_string(), value));
        }
    }

    fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name.as_str())
    }

    pub fn text(&self, key: &str) -> Result<&str, String> {
        match self.require(key)? {
            Value::Text(text) => Ok(text),
            other => Err(self.wrong_kind(key, "a quoted string", other)),
        }
    }

    pub fn integer(&self, key: &str) -> Result<i64, String> {
        match self.require(key)? {
            Value::Integer(value) => Ok(*value),
            other => Err(self.wrong_kind(key, "an integer", other)),
        }
    }

    /// Reads a decimal, accepting an integer literal: `r_end = 1` should not be an error
    /// just because the author left the point off.
    pub fn decimal(&self, key: &str) -> Result<f64, String> {
        match self.require(key)? {
            Value::Decimal(value) => Ok(*value),
            Value::Integer(value) => Ok(*value as f64),
            other => Err(self.wrong_kind(key, "a number", other)),
        }
    }

    pub fn optional_text(&self, key: &str) -> Result<Option<&str>, String> {
        self.get(key).map(|_| self.text(key)).transpose()
    }

    pub fn optional_integer(&self, key: &str) -> Result<Option<i64>, String> {
        self.get(key).map(|_| self.integer(key)).transpose()
    }

    pub fn optional_decimal(&self, key: &str) -> Result<Option<f64>, String> {
        self.get(key).map(|_| self.decimal(key)).transpose()
    }

    fn require(&self, key: &str) -> Result<&Value, String> {
        self.get(key)
            .ok_or_else(|| format!("{}: missing required key '{key}'", self.context))
    }

    fn wrong_kind(&self, key: &str, expected: &str, found: &Value) -> String {
        let found = match found {
            Value::Text(_) => "a quoted string",
            Value::Integer(_) => "an integer",
            Value::Decimal(_) => "a decimal",
        };
        format!(
            "{}: key '{key}' should be {expected} but is {found}",
            self.context
        )
    }
}

/// A parsed document: the top-level table plus the `[[section]]` tables in file order.
#[derive(Clone, Debug)]
pub struct Document {
    pub root: Table,
    pub sections: Vec<(String, Table)>,
}

impl Document {
    pub fn new(context: impl Into<String>) -> Self {
        Self {
            root: Table::new(context),
            sections: Vec::new(),
        }
    }

    pub fn push_section(&mut self, name: &str, table: Table) {
        self.sections.push((name.to_string(), table));
    }

    /// Every table declared as `[[name]]`, in file order.
    pub fn section(&self, name: &str) -> impl Iterator<Item = &Table> {
        self.sections
            .iter()
            .filter(move |(section, _)| section == name)
            .map(|(_, table)| table)
    }

    pub fn parse(text: &str, context: &str) -> Result<Self, String> {
        let mut document = Document::new(context.to_string());
        let mut current: Option<(String, Table)> = None;

        for (index, raw) in text.lines().enumerate() {
            let line_number = index + 1;
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }

            if let Some(rest) = line.strip_prefix("[[") {
                let name = rest.strip_suffix("]]").ok_or_else(|| {
                    format!(
                        "{context}:{line_number}: expected a '[[section]]' header, got '{line}'"
                    )
                })?;
                let name = name.trim();
                if name.is_empty() {
                    return Err(format!("{context}:{line_number}: empty section name"));
                }
                if let Some((section, table)) = current.take() {
                    document.push_section(&section, table);
                }
                let ordinal = document.section(name).count() + 1;
                current = Some((
                    name.to_string(),
                    Table::new(format!("{context}: [[{name}]] #{ordinal}")),
                ));
                continue;
            }
            if line.starts_with('[') {
                return Err(format!(
                    "{context}:{line_number}: single-bracket tables are not supported; use '[[{}]]'",
                    line.trim_matches(['[', ']'].as_slice())
                ));
            }

            let (key, value) = line.split_once('=').ok_or_else(|| {
                format!("{context}:{line_number}: expected 'key = value', got '{line}'")
            })?;
            let key = key.trim();
            if key.is_empty() {
                return Err(format!("{context}:{line_number}: empty key"));
            }
            let value = parse_value(value.trim())
                .ok_or_else(|| format!("{context}:{line_number}: unparseable value for '{key}'"))?;

            let table = match current.as_mut() {
                Some((_, table)) => table,
                None => &mut document.root,
            };
            if table.contains(key) {
                return Err(format!("{context}:{line_number}: duplicate key '{key}'"));
            }
            table.set(key, value);
        }

        if let Some((section, table)) = current.take() {
            document.push_section(&section, table);
        }
        Ok(document)
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        render_table(&mut out, &self.root);
        for (name, table) in &self.sections {
            if !out.is_empty() {
                out.push('\n');
            }
            let _ = writeln!(out, "[[{name}]]");
            render_table(&mut out, table);
        }
        out
    }
}

fn render_table(out: &mut String, table: &Table) {
    for (key, value) in &table.entries {
        let _ = match value {
            Value::Text(text) => writeln!(out, "{key} = \"{text}\""),
            Value::Integer(value) => writeln!(out, "{key} = {value}"),
            // `{:?}` is f64's shortest round-tripping form, and always renders a point,
            // so a decimal never reads back as an integer.
            Value::Decimal(value) => writeln!(out, "{key} = {value:?}"),
        };
    }
}

fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    for (index, character) in line.char_indices() {
        match character {
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..index],
            _ => {}
        }
    }
    line
}

fn parse_value(text: &str) -> Option<Value> {
    if let Some(body) = text.strip_prefix('"') {
        return body.strip_suffix('"').map(|s| Value::Text(s.to_string()));
    }
    if let Ok(value) = text.parse::<i64>() {
        return Some(Value::Integer(value));
    }
    match text.parse::<f64>() {
        Ok(value) if value.is_finite() => Some(Value::Decimal(value)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Document, Table, Value};

    #[test]
    fn a_document_round_trips_through_render_and_parse() {
        let mut document = Document::new("original");
        document
            .root
            .set_text("engine", "target/release/manifold.exe");
        document.root.set_integer("iterations", 1200);
        document.root.set_decimal("alpha", 0.602);
        let mut param = Table::new("param");
        param.set_text("name", "LmrCoefficient");
        param.set_integer("value", 2872);
        param.set_decimal("r_end", 0.016);
        document.push_section("param", param);

        let reparsed = Document::parse(&document.render(), "reparsed").expect("valid document");
        assert_eq!(
            reparsed.root.text("engine").unwrap(),
            "target/release/manifold.exe"
        );
        assert_eq!(reparsed.root.integer("iterations").unwrap(), 1200);
        assert_eq!(reparsed.root.decimal("alpha").unwrap(), 0.602);
        let params: Vec<&Table> = reparsed.section("param").collect();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].text("name").unwrap(), "LmrCoefficient");
        assert_eq!(params[0].integer("value").unwrap(), 2872);
        assert_eq!(params[0].decimal("r_end").unwrap(), 0.016);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored_but_hashes_inside_strings_survive() {
        let document = Document::parse(
            "# leading comment\n\nengine = \"a#b\"  # trailing\niterations = 3\n",
            "test",
        )
        .expect("valid document");
        assert_eq!(document.root.text("engine").unwrap(), "a#b");
        assert_eq!(document.root.integer("iterations").unwrap(), 3);
    }

    #[test]
    fn sections_keep_file_order_and_are_numbered_in_error_messages() {
        let document = Document::parse(
            "[[param]]\nname = \"A\"\n[[param]]\nname = \"B\"\n",
            "config.toml",
        )
        .expect("valid document");
        let names: Vec<&str> = document
            .section("param")
            .map(|table| table.text("name").unwrap())
            .collect();
        assert_eq!(names, vec!["A", "B"]);
        let missing = document
            .section("param")
            .nth(1)
            .unwrap()
            .integer("value")
            .unwrap_err();
        assert!(
            missing.contains("[[param]] #2") && missing.contains("value"),
            "error should locate the offending table: {missing}"
        );
    }

    #[test]
    fn malformed_input_is_rejected_with_a_line_number() {
        for (text, expected) in [
            ("engine\n", "expected 'key = value'"),
            ("engine = \n", "unparseable value"),
            ("[param]\nname = \"A\"\n", "single-bracket tables"),
            ("[[param\nname = \"A\"\n", "expected a '[[section]]' header"),
            ("a = 1\na = 2\n", "duplicate key 'a'"),
            ("a = nan\n", "unparseable value"),
        ] {
            let error = Document::parse(text, "bad.toml").expect_err("should be rejected");
            assert!(
                error.contains(expected),
                "expected {expected:?} in {error:?} for input {text:?}"
            );
        }
    }

    #[test]
    fn a_key_read_as_the_wrong_kind_names_the_key_and_both_kinds() {
        let document = Document::parse("iterations = \"many\"\n", "config.toml").unwrap();
        let error = document.root.integer("iterations").unwrap_err();
        assert!(error.contains("iterations"), "{error}");
        assert!(error.contains("an integer"), "{error}");
        assert!(error.contains("a quoted string"), "{error}");
    }

    #[test]
    fn a_decimal_key_accepts_an_integer_literal_but_not_the_reverse() {
        let document = Document::parse("r_end = 1\nc_end = 1.5\n", "config.toml").unwrap();
        assert_eq!(document.root.decimal("r_end").unwrap(), 1.0);
        assert!(document.root.integer("c_end").is_err());
    }

    #[test]
    fn negative_and_exponent_numbers_parse() {
        let document = Document::parse("lmr_base = -1024\ntiny = 1e-3\n", "config.toml").unwrap();
        assert_eq!(document.root.integer("lmr_base").unwrap(), -1024);
        assert_eq!(document.root.decimal("tiny").unwrap(), 0.001);
        assert_eq!(
            document.root.optional_text("absent").unwrap(),
            None::<&str>,
            "an absent optional key is None, not an error"
        );
        assert!(matches!(
            super::parse_value("\"quoted\""),
            Some(Value::Text(_))
        ));
    }
}
