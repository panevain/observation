//! The scene's change log: the agent proposals the user accepted (issue
//! #259), with when. An optional, additive block like `provenance`: files
//! without it load, older apps ignore it, and a malformed block or entry is
//! dropped rather than failing the file. The evidence sheet can cite it.

use crate::util::time::{format_utc, parse_utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Most entries kept; the oldest go first.
pub const MAX_AGENT_LOG: usize = 50;
/// Longest summary kept, characters. Summaries come from the agent and are
/// drawn as text, never as markup.
pub const MAX_LOG_SUMMARY_CHARS: usize = 200;

/// One accepted proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLogEntry {
    /// The proposal's summary, as the agent wrote it (tidied).
    pub summary: String,
    /// When the user accepted it: Unix seconds in memory, an ISO-8601 UTC
    /// string in the file.
    #[serde(rename = "accepted_at", with = "utc")]
    pub accepted_unix: u64,
}

impl AgentLogEntry {
    /// An entry for a proposal accepted at `accepted_unix`, with its summary
    /// tidied; `None` if nothing is left of the summary.
    pub fn new(summary: &str, accepted_unix: u64) -> Option<Self> {
        let summary = tidy_summary(summary)?;
        Some(Self {
            summary,
            accepted_unix,
        })
    }
}

/// `summary` trimmed, with its whitespace runs (line breaks included) made
/// single spaces, and cut to [`MAX_LOG_SUMMARY_CHARS`]; `None` if empty.
pub fn tidy_summary(summary: &str) -> Option<String> {
    let flat = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = flat.chars().take(MAX_LOG_SUMMARY_CHARS).collect();
    let cut = cut.trim_end();
    (!cut.is_empty()).then(|| cut.to_owned())
}

/// Appends `entry`, dropping the oldest entries beyond [`MAX_AGENT_LOG`].
pub fn push_entry(log: &mut Vec<AgentLogEntry>, entry: AgentLogEntry) {
    log.push(entry);
    if log.len() > MAX_AGENT_LOG {
        let extra = log.len() - MAX_AGENT_LOG;
        log.drain(..extra);
    }
}

/// Tidies a log read from a file: blank summaries are dropped, the others
/// tidied, and only the newest [`MAX_AGENT_LOG`] entries are kept.
pub(crate) fn sanitise(log: &mut Vec<AgentLogEntry>) {
    log.retain_mut(|e| match tidy_summary(&e.summary) {
        Some(s) => {
            e.summary = s;
            true
        }
        None => false,
    });
    if log.len() > MAX_AGENT_LOG {
        let extra = log.len() - MAX_AGENT_LOG;
        log.drain(..extra);
    }
}

mod utc {
    use super::{Deserialize, Deserializer, Serializer, format_utc, parse_utc};

    pub fn serialize<S: Serializer>(t: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format_utc(*t))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let text = String::deserialize(d)?;
        parse_utc(&text).ok_or_else(|| serde::de::Error::custom("not an ISO-8601 UTC time"))
    }
}

/// A value of type `T`, or anything else (ignored).
#[derive(Deserialize)]
#[serde(untagged)]
enum Lenient<T> {
    Value(T),
    Other(serde::de::IgnoredAny),
}

/// Reads the optional log: a block that is not a list reads as empty, and
/// an entry that cannot be read is dropped, so a hand-edited log never costs
/// the scene its cameras.
pub(crate) fn lenient_log<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<AgentLogEntry>, D::Error> {
    Ok(
        match Lenient::<Vec<Lenient<AgentLogEntry>>>::deserialize(d)? {
            Lenient::Value(entries) => entries
                .into_iter()
                .filter_map(|e| match e {
                    Lenient::Value(e) => Some(e),
                    Lenient::Other(_) => None,
                })
                .collect(),
            Lenient::Other(_) => Vec::new(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_are_flattened_trimmed_and_cut() {
        assert_eq!(tidy_summary("  a\n\n b\tc  ").as_deref(), Some("a b c"));
        assert_eq!(tidy_summary(" \n "), None);
        let long = format!("{} tail", "x".repeat(MAX_LOG_SUMMARY_CHARS - 1));
        assert_eq!(
            tidy_summary(&long).map(|s| s.chars().count()),
            Some(MAX_LOG_SUMMARY_CHARS - 1),
            "the cut leaves no trailing space"
        );
    }

    #[test]
    fn the_log_keeps_the_newest_entries() {
        let mut log = Vec::new();
        for i in 0..(MAX_AGENT_LOG as u64 + 5) {
            push_entry(&mut log, AgentLogEntry::new(&format!("p{i}"), i).unwrap());
        }
        assert_eq!(log.len(), MAX_AGENT_LOG);
        assert_eq!(log[0].summary, "p5");
        assert_eq!(log.last().unwrap().accepted_unix, MAX_AGENT_LOG as u64 + 4);
    }
}
