use crate::error::Result;
use crate::storage::db::Database;
use rusqlite::params;

/// Strip characters that FTS5 treats as syntax so raw user input cannot
/// crash the MATCH expression.  Only ASCII alphanumerics, `_`, and `-` are
/// kept; every run of rejected characters is collapsed to a single space,
/// and leading/trailing whitespace is trimmed.
pub(crate) fn sanitize_fts5_query(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    let mut last_was_space = true;
    for ch in query.chars() {
        let keep = ch.is_ascii_alphanumeric() || ch == '_' || ch == '-';
        if keep {
            out.push(ch);
            last_was_space = false;
        } else if !last_was_space {
            out.push(' ');
            last_was_space = true;
        }
    }
    out.trim().to_string()
}

/// Rebuild the FTS index from the chunks table. Use after bulk inserts
/// that bypassed the triggers (e.g. a full corpus reimport).
pub fn rebuild(db: &Database, corpus_id: &str) -> Result<()> {
    // Delete existing FTS rows for this corpus by joining on rowid.
    db.conn().execute_batch(&format!(
        "INSERT INTO chunks_fts(chunks_fts) VALUES ('delete-all');
         INSERT INTO chunks_fts(rowid, content)
             SELECT rowid, content FROM chunks WHERE corpus_id = '{corpus_id}';"
    ))?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct FtsResult {
    pub location_uri: String,
    pub snippet: String,
    pub rank: f64,
}

/// Full-text keyword search across chunks for a corpus.
pub fn search(db: &Database, corpus_id: &str, query: &str, limit: usize) -> Result<Vec<FtsResult>> {
    let sanitized = sanitize_fts5_query(query);
    if sanitized.is_empty() {
        return Ok(vec![]);
    }
    // Map the FTS rowid back to the chunks table to get the location_uri and corpus_id filter.
    let mut stmt = db.conn().prepare(
        "SELECT c.location_uri,
                snippet(chunks_fts, 0, '...', '...', '...', 20) AS snip,
                fts.rank
         FROM chunks_fts fts
         JOIN chunks c ON c.rowid = fts.rowid
         WHERE chunks_fts MATCH ?1
           AND c.corpus_id = ?2
         ORDER BY fts.rank
         LIMIT ?3",
    )?;
    let mut results = vec![];
    let rows = stmt.query_map(params![&sanitized, corpus_id, limit as i64], |row| {
        Ok(FtsResult {
            location_uri: row.get(0)?,
            snippet: row.get(1)?,
            rank: row.get(2)?,
        })
    })?;
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::sanitize_fts5_query;

    #[test]
    fn strips_double_colon() {
        let result = sanitize_fts5_query("SendFeed::dispatch");
        assert_eq!(result, "SendFeed dispatch");
        assert!(!result.contains(':'));
    }

    #[test]
    fn preserves_plain_identifier() {
        assert_eq!(
            sanitize_fts5_query("NewsletterService"),
            "NewsletterService"
        );
    }

    #[test]
    fn strips_assorted_metacharacters() {
        let result = sanitize_fts5_query("foo.bar/baz:qux(quux)*\"yes\"");
        // All word tokens should survive; metacharacters become spaces
        assert!(result.contains("foo"));
        assert!(result.contains("bar"));
        assert!(result.contains("baz"));
        assert!(result.contains("qux"));
        assert!(result.contains("quux"));
        assert!(result.contains("yes"));
        assert!(!result.contains('.'));
        assert!(!result.contains('/'));
        assert!(!result.contains(':'));
        assert!(!result.contains('('));
        assert!(!result.contains('*'));
        assert!(!result.contains('"'));
    }

    #[test]
    fn collapses_whitespace_and_trims() {
        assert_eq!(sanitize_fts5_query("  foo::   bar  "), "foo bar");
    }

    #[test]
    fn empty_when_only_metacharacters() {
        assert_eq!(sanitize_fts5_query(":::///..."), "");
    }

    #[test]
    fn preserves_underscore_and_hyphen() {
        assert_eq!(sanitize_fts5_query("my_var-name"), "my_var-name");
    }
}
