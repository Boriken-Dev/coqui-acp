//! Redaction: removing named values and patterns from a history, only when asked, and saying what
//! was removed.

use regex::{NoExpand, Regex, RegexBuilder};
use serde::Serialize;
use serde_json::Value;

use crate::header::{Header, Redacted};
use crate::settings::RedactionSettings;
use crate::{Body, Document, HistoryError, Line, strict};

/// Keys whose strings are identifiers the format pairs lines by, so they stay as they are.
const KEPT_KEYS: [&str; 3] = ["sessionUpdate", "toolCallId", "optionId"];

/// The largest compiled pattern, in bytes.
const PATTERN_SIZE_LIMIT: usize = 1 << 20;

/// Which list a rule came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub enum RuleKind {
    /// An entry of `values`.
    Value,
    /// An entry of `patterns`.
    Pattern,
}

/// What one rule removed. It names the rule by its place in the settings, never by its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct RuleReport {
    /// The list the rule is in.
    pub kind: RuleKind,
    /// Its position in that list.
    pub index: usize,
    /// How many removals it made.
    pub replaced: u64,
    /// In how many lines, the header counting as one.
    pub lines: u64,
}

/// What a redaction removed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct RedactionReport {
    /// Removals in all.
    pub replaced: u64,
    /// Lines changed, the header counting as one.
    pub lines: u64,
    /// Each rule, in the order the rules ran.
    pub rules: Vec<RuleReport>,
}

/// Removes what [`RedactionSettings`] names from a header and its lines. Its `Debug` output shows
/// how many rules it holds and none of their text.
///
/// Strings are changed wherever they are in a payload, except the three identifiers the format
/// pairs lines by. A removal that leaves a line the format refuses (a pattern that matches
/// `end_turn`, say) is an error and changes nothing in that line. Keys, the `method` and the time
/// of a line are never touched.
pub struct Redactor {
    values: Vec<String>,
    value_slots: Vec<usize>,
    patterns: Vec<Regex>,
    pattern_slots: Vec<usize>,
    replacement: String,
    report: RedactionReport,
    hits: Vec<u64>,
}

impl core::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Redactor")
            .field("values", &self.values.len())
            .field("patterns", &self.patterns.len())
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

impl Redactor {
    /// A redactor for `settings`. An empty entry of `values` removes nothing.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Pattern`] for a pattern that is not a regular expression, is too large, or
    /// matches the empty string (it would match everywhere).
    pub fn try_new(settings: &RedactionSettings) -> Result<Self, HistoryError> {
        let mut report = RedactionReport::default();
        let mut values = Vec::new();
        let mut value_slots = Vec::new();
        for (index, value) in settings.values.iter().enumerate() {
            if value.is_empty() {
                continue;
            }
            value_slots.push(report.rules.len());
            report.rules.push(RuleReport {
                kind: RuleKind::Value,
                index,
                replaced: 0,
                lines: 0,
            });
            values.push(value.clone());
        }
        let mut patterns = Vec::new();
        let mut pattern_slots = Vec::new();
        for (index, pattern) in settings.patterns.iter().enumerate() {
            let regex = RegexBuilder::new(pattern)
                .size_limit(PATTERN_SIZE_LIMIT)
                .nest_limit(64)
                .build()
                .map_err(|_| HistoryError::Pattern { index })?;
            if regex.is_match("") {
                return Err(HistoryError::Pattern { index });
            }
            pattern_slots.push(report.rules.len());
            report.rules.push(RuleReport {
                kind: RuleKind::Pattern,
                index,
                replaced: 0,
                lines: 0,
            });
            patterns.push(regex);
        }
        let hits = vec![0; report.rules.len()];
        Ok(Self {
            values,
            value_slots,
            patterns,
            pattern_slots,
            replacement: settings.replacement.clone(),
            report,
            hits,
        })
    }

    /// What has been removed so far.
    #[must_use]
    pub fn report(&self) -> &RedactionReport {
        &self.report
    }

    /// What the header says of the redaction so far.
    #[must_use]
    pub fn mark(&self) -> Redacted {
        Redacted::new(self.report.rules.len() as u64, self.report.replaced)
    }

    fn text(&mut self, text: &str) -> Option<String> {
        let mut current: Option<String> = None;
        for (rule, value) in self.values.iter().enumerate() {
            let slot = self.value_slots[rule];
            let haystack = current.as_deref().unwrap_or(text);
            let count = haystack.matches(value.as_str()).count() as u64;
            if count > 0 {
                current = Some(haystack.replace(value.as_str(), &self.replacement));
                self.hits[slot] += count;
            }
        }
        for (rule, regex) in self.patterns.iter().enumerate() {
            let slot = self.pattern_slots[rule];
            let haystack = current.as_deref().unwrap_or(text);
            let count = regex.find_iter(haystack).count() as u64;
            if count > 0 {
                current = Some(
                    regex
                        .replace_all(haystack, NoExpand(&self.replacement))
                        .into_owned(),
                );
                self.hits[slot] += count;
            }
        }
        current
    }

    fn walk(&mut self, root: &mut Value) {
        let mut stack = vec![root];
        while let Some(value) = stack.pop() {
            match value {
                Value::String(text) => {
                    if let Some(changed) = self.text(text) {
                        *text = changed;
                    }
                }
                Value::Array(items) => stack.extend(items.iter_mut()),
                Value::Object(map) => {
                    for (key, item) in map.iter_mut() {
                        if !(KEPT_KEYS.contains(&key.as_str()) && item.is_string()) {
                            stack.push(item);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Commits the removals counted since the last commit to the report.
    fn settle(&mut self) {
        let mut changed = false;
        for (slot, hits) in self.hits.iter_mut().enumerate() {
            if *hits > 0 {
                let rule = &mut self.report.rules[slot];
                rule.replaced += *hits;
                rule.lines += 1;
                self.report.replaced += *hits;
                *hits = 0;
                changed = true;
            }
        }
        if changed {
            self.report.lines += 1;
        }
    }

    /// Drops the removals counted since the last commit.
    fn forget(&mut self) {
        self.hits.iter_mut().for_each(|hits| *hits = 0);
    }

    fn touched(&self) -> bool {
        self.hits.iter().any(|hits| *hits > 0)
    }

    /// Redacts the header's text fields (not its `format`, `id`, time, digest or harness name).
    /// A changed header loses its digest; [`Document::seal`] makes a new one.
    ///
    /// # Errors
    ///
    /// The refusal a reader would give the redacted header; the header is left as it was.
    pub fn header(&mut self, header: &mut Header) -> Result<(), HistoryError> {
        header.to_line()?;
        let Ok(mut whole) = serde_json::to_value(&*header) else {
            return Ok(());
        };
        let kept = [
            "format",
            "version",
            "id",
            "createdAt",
            "digest",
            "acpVersion",
            "redacted",
        ];
        if let Value::Object(map) = &mut whole {
            for (key, item) in map.iter_mut() {
                if kept.contains(&key.as_str()) {
                    continue;
                }
                if let (true, Value::Object(origin)) = (key == "origin", &mut *item) {
                    for (field, text) in origin.iter_mut() {
                        if field != "harness" {
                            self.walk(text);
                        }
                    }
                } else {
                    self.walk(item);
                }
            }
        }
        if !self.touched() {
            return Ok(());
        }
        let checked =
            strict::parse(&whole.to_string(), 1, u64::MAX).and_then(|v| Header::from_value(&v));
        match checked {
            Ok(mut changed) => {
                changed.digest = None;
                *header = changed;
                self.settle();
                Ok(())
            }
            Err(error) => {
                self.forget();
                Err(error)
            }
        }
    }

    /// Redacts a line, the one numbered `number`.
    ///
    /// # Errors
    ///
    /// The refusal a reader would give the line as redacted; the line is left as it was and its
    /// removals are not counted.
    pub fn line(&mut self, number: u64, line: &mut Line) -> Result<(), HistoryError> {
        if line.payload_depth() > 128 {
            return line.to_json().map(|_| ());
        }
        let mut copy = line.clone();
        match &mut copy.body {
            Body::Prompt { prompt } => prompt.iter_mut().for_each(|block| self.walk(block)),
            Body::Update { update } => self.walk(update),
            Body::PermissionRequest { tool_call, options } => {
                self.walk(tool_call);
                options.iter_mut().for_each(|option| self.walk(option));
            }
            Body::PermissionAnswer { outcome, .. } => self.walk(outcome),
            Body::TurnEnd {
                error: Some(text), ..
            }
            | Body::Compaction {
                summary: Some(text),
                ..
            } => {
                if let Some(changed) = self.text(text) {
                    *text = changed;
                }
            }
            _ => {}
        }
        if let Some(meta) = &mut copy.meta {
            self.walk(meta);
        }
        if !self.touched() {
            return Ok(());
        }
        let checked = copy
            .to_json()
            .and_then(|text| strict::parse(&text, number, u64::MAX))
            .and_then(|value| Line::from_value(&value, number));
        match checked {
            Ok(_) => {
                *line = copy;
                self.settle();
                Ok(())
            }
            Err(error) => {
                self.forget();
                Err(error)
            }
        }
    }

    /// Redacts a whole document: its header, then each line, and marks the header with what was
    /// removed. The digest is dropped; [`Document::seal`] makes a new one. On an error the
    /// document is left as it was.
    ///
    /// # Errors
    ///
    /// The first line whose redaction the format refuses.
    pub fn document(&mut self, document: &mut Document) -> Result<RedactionReport, HistoryError> {
        let mut header = document.header.clone();
        self.header(&mut header)?;
        let mut lines = document.lines.clone();
        for (at, line) in lines.iter_mut().enumerate() {
            self.line(at as u64 + 2, line)?;
        }
        header.digest = None;
        header.redacted = Some(self.mark());
        document.header = header;
        document.lines = lines;
        Ok(self.report.clone())
    }
}
