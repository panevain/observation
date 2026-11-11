//! GitHub Actions workflow commands
//! (<https://docs.github.com/actions/reference/workflow-commands-for-github-actions>).

/// The level of an annotation on the run's summary page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
    Notice,
}

/// `::error::message` (or `::warning::`, `::notice::`), with the message escaped so that a
/// newline or `%` in it cannot end the command or start another.
pub fn annotation(level: Level, message: &str) -> String {
    let name = match level {
        Level::Error => "error",
        Level::Warning => "warning",
        Level::Notice => "notice",
    };
    let escaped = message
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A");
    format!("::{name}::{escaped}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations_escape_their_message() {
        assert_eq!(
            annotation(Level::Error, "no .wasm in dist/"),
            "::error::no .wasm in dist/"
        );
        assert_eq!(
            annotation(Level::Warning, "50% covered\r\n::error::x"),
            "::warning::50%25 covered%0D%0A::error::x"
        );
    }
}
