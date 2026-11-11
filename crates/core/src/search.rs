//! Turns what the user types in the Project Media search field into a safe FTS5 MATCH expression.

/// More words than this add nothing to a search of short descriptions; the rest are dropped.
pub const MAX_WORDS: usize = 12;

/// None when there is nothing to search for (blank, or only punctuation): show every item.
///
/// Each word becomes a quoted phrase, so FTS5 syntax the user types (`AND`, `-x`, `a:b`, `(`,
/// `*`, a stray `"`) is matched as text instead of failing the query. Words are ANDed. Only the
/// last word gets a prefix `*`, because it is the one still being typed: a prefix on every word
/// lets porter stems over-match (`"resting"*` also finds "restaurant").
pub fn fts_query(input: &str) -> Option<String> {
    let words: Vec<&str> = input.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric)).take(MAX_WORDS).collect();
    if words.is_empty() {
        return None;
    }
    // A trailing space means the last word is finished, so it gets no prefix either.
    let last_is_partial = input.chars().last().is_some_and(|c| !c.is_whitespace());
    let last = words.len() - 1;
    Some(
        words
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let phrase = format!("\"{}\"", w.replace('"', "\"\""));
                if i == last && last_is_partial {
                    format!("{phrase}*")
                } else {
                    phrase
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

#[cfg(test)]
mod tests {
    use super::fts_query;

    #[test]
    fn builds_safe_queries() {
        assert_eq!(fts_query("  "), None);
        assert_eq!(fts_query("()*"), None);
        assert_eq!(fts_query("sea tur").as_deref(), Some("\"sea\" \"tur\"*"));
        assert_eq!(fts_query("sea turtle ").as_deref(), Some("\"sea\" \"turtle\""));
        assert_eq!(fts_query("a\"b AND -x").as_deref(), Some("\"a\"\"b\" \"AND\" \"-x\"*"));
    }
}
