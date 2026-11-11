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

    // The Android app's `FtsQueryTest`.

    fn q(input: &str) -> String {
        fts_query(input).unwrap()
    }

    #[test]
    fn blank_or_punctuation_only_is_none() {
        for input in ["", "   ", " - * ( \" "] {
            assert_eq!(fts_query(input), None, "{input:?}");
        }
    }

    #[test]
    fn last_word_is_a_prefix_earlier_words_are_exact() {
        assert_eq!(q("turt"), r#""turt"*"#);
        assert_eq!(q("sea turt"), r#""sea" "turt"*"#);
        // A trailing space ends the prefix.
        assert_eq!(q("sea turtle "), r#""sea" "turtle""#);
    }

    #[test]
    fn fts_syntax_is_quoted_as_text() {
        assert_eq!(q("AND"), r#""AND"*"#);
        assert_eq!(q("sea OR"), r#""sea" "OR"*"#);
        assert_eq!(q("-x"), r#""-x"*"#);
        assert_eq!(q("a:b"), r#""a:b"*"#);
        assert_eq!(q("(x"), r#""(x"*"#);
        assert_eq!(q("NEAR("), r#""NEAR("*"#);
    }

    #[test]
    fn double_quotes_are_escaped() {
        assert_eq!(q("say \"hi\""), r#""say" """hi"""*"#);
        assert_eq!(q("turtle\""), r#""turtle"""*"#);
    }

    #[test]
    fn punctuation_only_words_are_dropped_and_marks_stay_inside_words() {
        assert_eq!(q("sea - turtle"), r#""sea" "turtle"*"#);
        assert_eq!(q("mama's"), r#""mama's"*"#);
        assert_eq!(q("café hoʻokipa"), r#""café" "hoʻokipa"*"#);
    }

    #[test]
    fn word_count_is_capped() {
        let input = (1..=30).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ");
        assert_eq!(q(&input).split(' ').count(), super::MAX_WORDS);
    }

    /// Every query the builder makes must be one SQLite accepts.
    #[test]
    fn built_queries_never_fail_in_sqlite() {
        let (_d, db) = crate::db::tests::temp_db();
        for input in ["AND", "sea OR", "-x", "a:b", "(x", "NEAR(", "say \"hi\"", "turtle\"", "* ^ :", "café hoʻokipa", "a NOT b", "{x}", "x^2", "col: y"] {
            if let Some(query) = fts_query(input) {
                db.search(&query).unwrap_or_else(|e| panic!("{input:?} became {query:?}: {e}"));
            }
        }
    }
}
