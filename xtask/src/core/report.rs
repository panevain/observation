//! Markdown for job summaries and reports, written the way the scripts wrote it.

/// How a column's cells align.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// A Markdown table: `| a | b |` lines under a `| --- | ---: |` rule. Cells are written as
/// given (escaping is the caller's), and an empty cell is a single space (`| |`).
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
        self.rows.push(cells.into_iter().map(Into::into).collect());
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
        let mut out = vec![line(&self.header), line(rule)];
        out.extend(self.rows.iter().map(line));
        out
    }
}

fn line<S: AsRef<str>>(cells: impl IntoIterator<Item = S>) -> String {
    let mut s = String::from("|");
    for c in cells {
        let c = c.as_ref();
        if !c.is_empty() {
            s.push(' ');
            s.push_str(c);
        }
        s.push_str(" |");
    }
    s
}

/// `n` with a comma between thousands, as Python's `{:,}`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
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
        assert_eq!(
            t.lines(),
            [
                "| module | raw | |",
                "| --- | ---: | --- |",
                "| `observation-client` | 9.87 MiB | ok |",
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
