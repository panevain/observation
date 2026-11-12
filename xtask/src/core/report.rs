//! Markdown for job summaries and reports, written the way the scripts wrote it.

/// How a column's cells align.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// A Markdown table: `| a | b |` lines under a `| --- | ---: |` rule. Cells are written as
/// given (escaping is the caller's), each as ` cell |`, so an empty one is `|  |`, as the
/// scripts' f-strings write it. The one exception is an empty header cell, written `| |` as
/// check-wasm-size.py wrote its verdict column's.
#[derive(Debug, Clone, Default)]
pub struct Table {
    header: Vec<String>,
    align: Vec<Align>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(columns: &[(&str, Align)]) -> Self {
        Self {
            header: columns.iter().map(|(h, _)| (*h).to_owned()).collect(),
            align: columns.iter().map(|&(_, a)| a).collect(),
            rows: Vec::new(),
        }
    }

    pub fn row<S: Into<String>>(&mut self, cells: impl IntoIterator<Item = S>) -> &mut Self {
        let row: Vec<String> = cells.into_iter().map(Into::into).collect();
        debug_assert_eq!(row.len(), self.header.len(), "row width: {row:?}");
        self.rows.push(row);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The table's lines, without line endings.
    pub fn lines(&self) -> Vec<String> {
        let rule = self.align.iter().map(|a| match a {
            Align::Left => "---",
            Align::Right => "---:",
        });
        let mut out = vec![line(&self.header, true), line(rule, false)];
        out.extend(self.rows.iter().map(|r| line(r, false)));
        out
    }
}

/// `| a | b |`; with `collapse_empty`, an empty cell is `| |` rather than `|  |`.
fn line<S: AsRef<str>>(cells: impl IntoIterator<Item = S>, collapse_empty: bool) -> String {
    let mut s = String::from("|");
    for c in cells {
        let c = c.as_ref();
        if !(collapse_empty && c.is_empty()) {
            s.push(' ');
            s.push_str(c);
        }
        s.push_str(" |");
    }
    s
}

/// `n` with a comma between thousands, as Python's `{:,}`.
pub fn thousands(n: impl Into<i128>) -> String {
    let n: i128 = n.into();
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_match_the_scripts() {
        let mut t = Table::new(&[
            ("module", Align::Left),
            ("raw", Align::Right),
            ("", Align::Left),
        ]);
        assert!(t.is_empty());
        t.row(["`observation-client`", "9.87 MiB", "ok"]);
        t.row(["`worker`", "", "ok"]);
        assert_eq!(
            t.lines(),
            [
                "| module | raw | |",
                "| --- | ---: | --- |",
                "| `observation-client` | 9.87 MiB | ok |",
                "| `worker` |  | ok |",
            ]
        );
    }

    #[test]
    fn thousands_separators() {
        let cases = [
            (0, "0"),
            (999, "999"),
            (1000, "1,000"),
            (12_345_678, "12,345,678"),
        ];
        for (n, want) in cases {
            assert_eq!(thousands(n), want);
        }
    }
}
