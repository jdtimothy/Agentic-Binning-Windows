//! Sorting analyzed items into categories from their text: prompt copy, chunk planning, the reply
//! grammar and schema, parsing and validation. Ported from the Android app's `sorting` package.

use crate::analysis::{ActionEntry, ItemKind};
use crate::names::{clean_name, take_chars};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

/// Changing any prompt text or reply shape requires bumping this.
pub const SORT_PROMPT_VERSION: &str = "sort-v4";

pub const MAX_NEW_CATEGORIES: usize = 12;
pub const MAX_CATEGORY_CHARS: usize = 40;
pub const MAX_NAME_CHARS: usize = 60;
pub const MAX_DESCRIPTION_CHARS: usize = 160;

/// A category with the number of items currently in it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryInfo {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub created_by: String,
    pub items: i64,
}

/// A category the model proposed, not written yet.
#[derive(Debug, Clone, PartialEq)]
pub struct NewCategoryEntity {
    pub name: String,
    pub description: String,
}

/// The sorting model's decision for one item: a category by exact name, and a friendly name.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub item_id: i64,
    pub category_name: String,
    pub friendly_name: String,
}

/// One analyzed item as the sorting model sees it: text only.
#[derive(Debug, Clone, PartialEq)]
pub struct SortItem {
    pub id: i64,
    pub kind: ItemKind,
    pub suggested_name: String,
    pub tags: Vec<String>,
    pub description: String,
    /// Video actions as `m:ss text`; empty for photos.
    pub actions: Vec<String>,
    /// The user chose this item's name, so a model rename is never applied.
    pub name_set_by_user: bool,
}

impl SortItem {
    pub fn actions_from_json(actions_json: &str) -> Vec<String> {
        serde_json::from_str::<Vec<ActionEntry>>(actions_json)
            .map(|a| a.into_iter().map(|e| format!("{} {}", e.t, e.text)).collect())
            .unwrap_or_default()
    }
}

/// What one sorting backend can take per call.
#[derive(Debug, Clone, Copy)]
pub struct ChunkBudget {
    pub context_tokens: usize,
    pub max_chunk: usize,
    pub propose_sample: usize,
    /// The most new categories one chunk may add once categories exist.
    pub max_new_with_categories: usize,
}

pub const BUDGET_CLAUDE: ChunkBudget =
    ChunkBudget { context_tokens: 200_000, max_chunk: 200, propose_sample: 2000, max_new_with_categories: MAX_NEW_CATEGORIES };
/// LFM3-1.7B makes a long tail of one-item categories, so after the first ones it adds few.
pub const BUDGET_LFM3_1_7B: ChunkBudget =
    ChunkBudget { context_tokens: 16384, max_chunk: 10, propose_sample: 20, max_new_with_categories: 3 };

fn ws_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\s+").unwrap())
}

/// Tabs and newlines would break the line format, so every field is flattened to single spaces.
fn flatten(s: &str) -> String {
    ws_re().replace_all(s, " ").trim().to_string()
}

fn cut_on_word(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if chars[max] == ' ' {
        return chars[..max].iter().collect();
    }
    match chars[..=max].iter().rposition(|c| *c == ' ') {
        Some(space) if space > 0 => chars[..space].iter().collect(),
        _ => chars[..max].iter().collect(),
    }
}

pub mod prompt {
    use super::*;

    pub const MAX_DESCRIPTION_CHARS: usize = 240;
    pub const MAX_ACTIONS_CHARS: usize = 160;

    pub const SYSTEM: &str = "You sort a personal photo and video library into categories, using only the text given for each item.\n\
Rules:\n\
- Put items into the existing categories. Add a new category only when nothing existing fits well.\n\
- Add at most 12 new categories in one reply.\n\
- Never rename or merge existing categories. A \"locked\" category was set by the user: it can receive items, nothing else.\n\
- Avoid one-item categories unless an item truly fits nowhere else.\n\
- No catch-all category such as \"Misc\" or \"Other\", unless a handful of items fit nowhere.\n\
- A new category name is one general word that makes a good folder name, such as Animals, People, Food or Sunsets: never two subjects joined, and at most 40 characters. The specific subject (a turtle, a seal, a dish) belongs in the friendly name, not the category.\n\
- A category describes what is in the media (its subject, place or event), never the file type.\n\
- Each new category gets a one-line description.\n\
- Give every item a friendly name of 2-6 words that describes it. No dates unless they matter, and no file extension.\n\
Reply with one JSON object and nothing else:\n\
{\"new_categories\":[{\"name\":\"...\",\"description\":\"...\"}],\"assignments\":[{\"id\":<item id>,\"category\":\"<category name>\",\"name\":\"<friendly name>\"}]}\n\
Assign every listed item exactly once, using the ids shown. \"category\" is the exact name of an existing category or of one you list in \"new_categories\".\n\
When asked only to propose categories, reply with just {\"new_categories\":[...]}.";

    /// One item as a tab-separated line: id, kind, suggested name, tags, description, and for videos the actions.
    pub fn item_line(item: &SortItem) -> String {
        let mut cols = vec![
            item.id.to_string(),
            if item.kind == ItemKind::Video { "video".into() } else { "photo".into() },
            flatten(&item.suggested_name),
            item.tags.iter().map(|t| flatten(t)).collect::<Vec<_>>().join(", "),
            cut_on_word(&flatten(&item.description), MAX_DESCRIPTION_CHARS),
        ];
        if item.kind == ItemKind::Video {
            let joined = item.actions.iter().map(|a| flatten(a)).collect::<Vec<_>>().join("; ");
            cols.push(cut_on_word(&joined, MAX_ACTIONS_CHARS));
        }
        cols.join("\t")
    }

    /// The proposal pass sends the suggested name and tags only.
    pub fn propose_line(item: &SortItem) -> String {
        format!("{}\t{}", flatten(&item.suggested_name), item.tags.iter().map(|t| flatten(t)).collect::<Vec<_>>().join(", "))
    }

    /// Name, item count, `locked` (the user's) or `open`, description.
    pub fn category_line(c: &CategoryInfo) -> String {
        [flatten(&c.name), c.items.to_string(), if c.created_by == "model" { "open".into() } else { "locked".into() }, flatten(&c.description)]
            .join("\t")
    }

    pub fn propose_user(items: &[SortItem]) -> String {
        let mut s = String::from("Propose categories for this library. Reply with only new_categories: broad groups named with one general word each, at most 12, each with a one-line description.\n\nItems (suggested name, tags):\n");
        for i in items {
            s.push_str(&propose_line(i));
            s.push('\n');
        }
        s.trim_end().to_string()
    }

    /// `max_new_categories` below the system prompt's 12 is stated in this prompt.
    pub fn assign_user(items: &[SortItem], categories: &[CategoryInfo], feedback: &[String], max_new_categories: usize) -> String {
        let mut s = String::from("Existing categories (name, items, locked or open, description):\n");
        if categories.is_empty() {
            s.push_str("(none yet)\n");
        } else {
            for c in categories {
                s.push_str(&category_line(c));
                s.push('\n');
            }
        }
        s.push_str("\nItems to sort (id, kind, suggested name, tags, description, actions for videos):\n");
        for i in items {
            s.push_str(&item_line(i));
            s.push('\n');
        }
        if !feedback.is_empty() {
            s.push_str("\nYour previous reply was rejected. Fix these problems:\n");
            for f in feedback {
                s.push_str(&format!("- {f}\n"));
            }
        }
        s.push('\n');
        if max_new_categories < MAX_NEW_CATEGORIES {
            s.push_str(&format!("Add at most {max_new_categories} new categories in this reply.\n"));
        }
        s.push_str("Reply with the JSON object only.");
        s
    }
}

pub mod planner {
    use super::*;

    /// A call's prompt and reply must fit this share of the context.
    pub const FIT_FRACTION: f64 = 0.85;
    pub const REPLY_TOKENS_PER_ITEM: usize = 30;
    pub const REPLY_TOKENS_BASE: usize = 200;
    pub const PROPOSE_REPLY_TOKENS: usize = 400;
    /// The user message's own words around the item and category lines.
    const FRAME_TOKENS: usize = 100;

    /// `chars / 3.5`, rounded up. The measured ratio on real item lines is about 4.1, so this runs high.
    pub fn estimate_tokens(text: &str) -> usize {
        (2 * text.chars().count() + 6) / 7
    }

    fn system_tokens() -> usize {
        estimate_tokens(prompt::SYSTEM)
    }

    /// The prompt tokens of `categories` as the assignment prompt lists them, one line each.
    pub fn category_tokens(categories: &[CategoryInfo]) -> usize {
        categories.iter().map(|c| estimate_tokens(&prompt::category_line(c)) + 1).sum()
    }

    fn fixed_assign_tokens(category_reserve: usize) -> usize {
        system_tokens() + FRAME_TOKENS + category_reserve + REPLY_TOKENS_BASE
    }

    fn fixed_propose_tokens() -> usize {
        system_tokens() + FRAME_TOKENS + PROPOSE_REPLY_TOKENS
    }

    fn item_tokens(item: &SortItem) -> usize {
        estimate_tokens(&prompt::item_line(item)) + 1 + REPLY_TOKENS_PER_ITEM
    }

    /// The estimated tokens of an assignment call for `items` beside a category list of `category_tokens`.
    pub fn chunk_tokens(items: &[SortItem], category_tokens: usize) -> usize {
        fixed_assign_tokens(category_tokens) + items.iter().map(item_tokens).sum::<usize>()
    }

    /// The estimated tokens of a proposal call for `items`.
    pub fn propose_tokens(items: &[SortItem]) -> usize {
        fixed_propose_tokens() + items.iter().map(|i| estimate_tokens(&prompt::propose_line(i)) + 1).sum::<usize>()
    }

    fn limit_of(budget: &ChunkBudget) -> usize {
        (FIT_FRACTION * budget.context_tokens as f64) as usize
    }

    /// The length of the longest prefix of `items` (at most `max_chunk`) whose assignment prompt,
    /// with a category list of `category_tokens`, fits the budget. Never 0 for non-empty `items`.
    pub fn next_chunk_len(items: &[SortItem], budget: &ChunkBudget, category_tokens: usize) -> usize {
        let limit = limit_of(budget);
        let mut used = fixed_assign_tokens(category_tokens);
        let mut n = 0;
        for item in items {
            let cost = item_tokens(item);
            if n > 0 && (n >= budget.max_chunk || used + cost > limit) {
                break;
            }
            used += cost;
            n += 1;
        }
        n
    }

    fn even_indices(size: usize, n: usize) -> Vec<usize> {
        (0..n).map(|i| i * size / n).collect()
    }

    /// The sample for a proposal pass, or None when none is needed: up to `propose_sample` items
    /// taken at even steps, cut back until the prompt fits.
    pub fn proposal_sample(items: &[SortItem], has_categories: bool, budget: &ChunkBudget) -> Option<Vec<SortItem>> {
        if has_categories || items.len() <= budget.max_chunk {
            return None;
        }
        let limit = limit_of(budget);
        let mut n = budget.propose_sample.min(items.len());
        let line_tokens: Vec<usize> = items.iter().map(|i| estimate_tokens(&prompt::propose_line(i)) + 1).collect();
        while n > 1 {
            let total: usize = even_indices(items.len(), n).iter().map(|i| line_tokens[*i]).sum();
            if fixed_propose_tokens() + total <= limit {
                break;
            }
            n -= 1;
        }
        Some(even_indices(items.len(), n.max(1)).into_iter().map(|i| items[i].clone()).collect())
    }
}

pub mod schema {
    use super::*;

    fn new_category_item() -> Value {
        json!({"type":"object","properties":{"name":{"type":"string"},"description":{"type":"string"}},"required":["name","description"],"additionalProperties":false})
    }

    fn new_categories() -> Value {
        json!({"type":"array","items":new_category_item(),"maxItems":MAX_NEW_CATEGORIES})
    }

    pub fn assign_json_schema() -> Value {
        json!({
            "type":"object",
            "properties":{
                "new_categories": new_categories(),
                "assignments":{"type":"array","items":{"type":"object","properties":{"id":{"type":"integer"},"category":{"type":"string"},"name":{"type":"string"}},"required":["id","category","name"],"additionalProperties":false}}
            },
            "required":["new_categories","assignments"],
            "additionalProperties":false
        })
    }

    pub fn propose_json_schema() -> Value {
        json!({"type":"object","properties":{"new_categories": new_categories()},"required":["new_categories"],"additionalProperties":false})
    }

    /// A new category name's character: one word, so no space and no joiner.
    const WORD_CHAR: &str = r#"[^"\\\x7F\x00-\x20&+/_\x2D]"#;
    /// Any character a JSON string may hold unescaped.
    const CHAR: &str = r#"[^"\\\x7F\x00-\x1F]"#;
    const NEWCAT: &str = r#"newcat   ::= "{" ws "\"name\":" ws catname "," ws "\"description\":" ws desc ws "}""#;

    fn shared() -> String {
        format!(
            "catname  ::= \"\\\"\" {WORD_CHAR}{{1,{MAX_CATEGORY_CHARS}}} \"\\\"\"\nfname    ::= \"\\\"\" {CHAR}{{1,{MAX_NAME_CHARS}}} \"\\\"\"\ndesc     ::= \"\\\"\" {CHAR}{{1,{MAX_DESCRIPTION_CHARS}}} \"\\\"\"\nws       ::= [ \\t\\n]{{0,4}}"
        )
    }

    /// A GBNF literal matching `name` as a JSON string, quotes included.
    fn literal_of(name: &str) -> String {
        let json_string = serde_json::to_string(name).unwrap_or_else(|_| "\"\"".into());
        format!("\"{}\"", json_string.replace('\\', "\\\\").replace('"', "\\\""))
    }

    /// Grammar for one assignment chunk: `ids` are listed in order, each as a literal, and
    /// `category` is one of `existing_names` (as literals) or a new name of at most 40 chars.
    pub fn assign_grammar(ids: &[i64], existing_names: &[String]) -> String {
        let entries: Vec<String> = ids.iter().map(|id| format!(r#""{{" ws "\"id\":" ws "{id}" tail"#)).collect();
        let assigns = if ids.is_empty() {
            r#""[" ws "]""#.to_string()
        } else {
            format!(r#""[" ws {} ws "]""#, entries.join(r#" "," ws "#))
        };
        let mut seen = HashSet::new();
        let mut cats: Vec<String> = existing_names.iter().filter(|n| seen.insert(n.as_str())).map(|n| literal_of(n)).collect();
        cats.push("catname".into());
        [
            r#"root     ::= "{" ws "\"new_categories\":" ws newcats "," ws "\"assignments\":" ws assigns ws "}""#.to_string(),
            format!(r#"newcats  ::= "[" ws "]" | "[" ws newcat ( "," ws newcat ){{0,{}}} ws "]""#, MAX_NEW_CATEGORIES - 1),
            NEWCAT.to_string(),
            format!("assigns  ::= {assigns}"),
            r#"tail     ::= "," ws "\"category\":" ws category "," ws "\"name\":" ws fname ws "}""#.to_string(),
            format!("category ::= {}", cats.join(" | ")),
            shared(),
        ]
        .join("\n")
    }

    /// Grammar for the proposal pass: only `new_categories`, one to twelve of them.
    pub fn propose_grammar() -> String {
        [
            r#"root     ::= "{" ws "\"new_categories\":" ws newcats ws "}""#.to_string(),
            format!(r#"newcats  ::= "[" ws newcat ( "," ws newcat ){{0,{}}} ws "]""#, MAX_NEW_CATEGORIES - 1),
            NEWCAT.to_string(),
            shared(),
        ]
        .join("\n")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewCategory {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawAssignment {
    pub id: i64,
    pub category: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SortReply {
    pub new_categories: Vec<NewCategory>,
    pub assignments: Vec<RawAssignment>,
}

fn str_of(o: &Value, key: &str) -> String {
    o.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn long_of(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    if let Some(i) = v.as_i64() {
        return Some(i);
    }
    // The model sometimes writes an id as a string ("3"), which the Android parser also reads.
    let d = match v.as_str() {
        Some(s) => match s.trim().parse::<i64>() {
            Ok(i) => return Some(i),
            Err(_) => s.trim().parse::<f64>().ok()?,
        },
        None => v.as_f64()?,
    };
    if d.fract() == 0.0 && d.abs() < 9e15 {
        Some(d as i64)
    } else {
        None
    }
}

/// Reads the outermost `{...}` of `text` (so code fences and a line of prose don't matter).
/// None when no JSON object with `new_categories` or `assignments` can be read. An assignment
/// whose id is unreadable is skipped, which the validator then reports as a missing id.
pub fn parse_reply(text: &str) -> Option<SortReply> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    let root: Value = serde_json::from_str(&text[start..=end]).ok()?;
    let obj = root.as_object()?;
    if !obj.contains_key("new_categories") && !obj.contains_key("assignments") {
        return None;
    }
    let empty = Vec::new();
    let new_categories = obj
        .get("new_categories")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter(|e| e.is_object())
        .map(|e| NewCategory { name: str_of(e, "name"), description: str_of(e, "description") })
        .collect();
    let assignments = obj
        .get("assignments")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter(|e| e.is_object())
        .filter_map(|e| Some(RawAssignment { id: long_of(e.get("id"))?, category: str_of(e, "category"), name: str_of(e, "name") }))
        .collect();
    Some(SortReply { new_categories, assignments })
}

#[derive(Debug, Clone, PartialEq)]
pub enum Validation {
    /// Apply these. `warnings` were only logged. `left_out` maps the ids of items that were not
    /// assigned to the problem with each; it is only filled on a retry.
    Ok { new_categories: Vec<NewCategoryEntity>, assignments: Vec<Assignment>, warnings: Vec<String>, left_out: Vec<(i64, String)> },
    /// Nothing may be applied; the problems go back to the model for the retry.
    Rejected(Vec<String>),
}

pub mod validator {
    use super::*;

    pub const MAX_CATEGORY_NAME: usize = 40;
    pub const MAX_FRIENDLY_NAME: usize = 60;
    pub const MAX_CATEGORY_DESCRIPTION: usize = 160;
    /// A catch-all bin may hold at most max(5, 1/20 of the project).
    pub const CATCH_ALL_MIN: i64 = 5;
    pub const CATCH_ALL_DIVISOR: i64 = 20;
    const SINGLETON_MIN: usize = 2;
    const SINGLETON_DIVISOR: usize = 5;
    const MAX_CATEGORY_WORDS: usize = 1;
    const MAX_FRIENDLY_WORDS: usize = 8;

    const CATCH_ALL_WORDS: [&str; 16] = [
        "misc", "miscellaneous", "other", "general", "uncategorized", "various", "random", "unsorted", "photo", "video", "image",
        "picture", "clip", "footage", "media", "file",
    ];
    const DANGLING: [&str; 7] = ["but", "and", "or", "with", "of", "the", "is"];
    const PROMPT_PHRASES: [&str; 7] =
        ["items to sort", "new categories", "existing categories", "category", "items", "suggested name", "friendly name"];

    fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
        cell.get_or_init(|| Regex::new(pat).unwrap())
    }

    fn singular(w: &str) -> String {
        let n = w.chars().count();
        if n > 3 && w.ends_with("ies") {
            return format!("{}y", &w[..w.len() - 3]);
        }
        if n > 3 && (w.ends_with("ches") || w.ends_with("shes") || w.ends_with("xes") || w.ends_with("ses")) {
            return w[..w.len() - 2].to_string();
        }
        if n > 1 && w.ends_with('s') && !w.ends_with("ss") {
            return w[..w.len() - 1].to_string();
        }
        w.to_string()
    }

    /// Lowercase, no diacritics or punctuation, the articles dropped (unless nothing else is left;
    /// a last "a" is kept, as in "Vitamin A"), single spaces, and the last word made singular.
    pub fn normalize(name: &str) -> String {
        static MARKS: OnceLock<Regex> = OnceLock::new();
        static NOT_ALNUM: OnceLock<Regex> = OnceLock::new();
        let decomposed: String = name.nfd().collect();
        let no_marks = re(&MARKS, r"\p{Mn}+").replace_all(&decomposed, "").to_lowercase();
        let plain = re(&NOT_ALNUM, r"[^\p{L}\p{N}]+").replace_all(&no_marks, " ").trim().to_string();
        if plain.is_empty() {
            return String::new();
        }
        let words: Vec<&str> = plain.split(' ').collect();
        let last = words.len() - 1;
        let mut kept: Vec<&str> =
            words.iter().enumerate().filter(|(i, w)| !["the", "a", "an"].contains(w) || (**w == "a" && *i == last)).map(|(_, w)| *w).collect();
        if kept.is_empty() {
            kept = words;
        }
        let tail = singular(kept[kept.len() - 1]);
        let mut out: Vec<String> = kept[..kept.len() - 1].iter().map(|s| s.to_string()).collect();
        out.push(tail);
        out.join(" ")
    }

    pub fn is_catch_all(name: &str) -> bool {
        let n = normalize(name);
        CATCH_ALL_WORDS.iter().any(|w| normalize(w) == n)
    }

    /// Why `raw` is not usable as a new category name, or None when it is.
    pub fn category_name_problem(raw: &str) -> Option<String> {
        static REASONING: OnceLock<Regex> = OnceLock::new();
        static WORD_BREAK: OnceLock<Regex> = OnceLock::new();
        let text = ws_re().replace_all(raw, " ").trim().to_string();
        let lower = text.to_lowercase();
        let words: Vec<&str> =
            re(&WORD_BREAK, r"[^\p{L}\p{N}'’]+").split(&text).filter(|w| w.chars().any(char::is_alphanumeric)).collect();
        let last_word = words.last().map(|w| w.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()).to_string());
        if text.contains("...") || text.contains('…') {
            return Some("it has '...'".into());
        }
        if re(&REASONING, r"\blet['’]s\b|\blets see\b").is_match(&lower) {
            return Some("it reads like reasoning, not a name".into());
        }
        if words.len() > MAX_CATEGORY_WORDS {
            return Some(format!("it has {} words; use one general word", words.len()));
        }
        if text.trim_end_matches('.').ends_with(',') || last_word.as_deref().is_some_and(|w| DANGLING.contains(&w)) {
            return Some("it stops mid-phrase".into());
        }
        let n = normalize(&text);
        if PROMPT_PHRASES.iter().any(|p| normalize(p) == n) {
            return Some("it repeats the prompt's wording".into());
        }
        None
    }

    /// A friendly name the model wrote as a sentence or with an ellipsis is dropped, not retried.
    fn friendly_name_usable(raw: &str) -> bool {
        !raw.contains("...") && !raw.contains('…') && raw.split_whitespace().count() <= MAX_FRIENDLY_WORDS
    }

    /// Names already taken, found by exact text (any case) and then by [`normalize`].
    #[derive(Default)]
    struct NameTable {
        by_lower: HashMap<String, String>,
        by_normal: HashMap<String, String>,
    }

    impl NameTable {
        fn of(existing: &[CategoryInfo]) -> Self {
            let mut t = Self::default();
            for c in existing {
                t.add(&c.name);
            }
            t
        }
        fn add(&mut self, name: &str) {
            self.by_lower.entry(name.to_lowercase()).or_insert_with(|| name.to_string());
            let n = normalize(name);
            if !n.is_empty() {
                self.by_normal.entry(n).or_insert_with(|| name.to_string());
            }
        }
        fn find(&self, name: &str) -> Option<String> {
            if let Some(n) = self.by_lower.get(&name.to_lowercase()) {
                return Some(n.clone());
            }
            let n = normalize(name);
            if n.is_empty() {
                None
            } else {
                self.by_normal.get(&n).cloned()
            }
        }
    }

    fn clean_description(d: &str) -> String {
        let flat = ws_re().replace_all(d, " ").trim().to_string();
        take_chars(&flat, MAX_CATEGORY_DESCRIPTION).trim_end().to_string()
    }

    fn new_entity(name: &str, description: &str) -> NewCategoryEntity {
        NewCategoryEntity { name: name.to_string(), description: clean_description(description) }
    }

    /// The categories a proposal pass adds: names cleaned, near-duplicates merged into the first,
    /// and anything that matches an existing category dropped. Unusable names are skipped.
    pub fn merge_proposals(proposals: &[NewCategory], existing: &[CategoryInfo]) -> Vec<NewCategoryEntity> {
        let mut table = NameTable::of(existing);
        let mut out = Vec::new();
        for p in proposals {
            let name = clean_name(&p.name, MAX_CATEGORY_NAME);
            if name.is_empty() || table.find(&name).is_some() || category_name_problem(&p.name).is_some() {
                continue;
            }
            table.add(&name);
            out.push(new_entity(&name, &p.description));
            if out.len() == MAX_NEW_CATEGORIES {
                break;
            }
        }
        out
    }

    fn join_ids<'a>(ids: impl IntoIterator<Item = &'a i64>) -> String {
        ids.into_iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ")
    }

    /// Sets or replaces `id`'s problem, keeping first-insertion order.
    fn put(problems: &mut Vec<(i64, String)>, id: i64, msg: String) {
        match problems.iter_mut().find(|(i, _)| *i == id) {
            Some(e) => e.1 = msg,
            None => problems.push((id, msg)),
        }
    }

    /// Checks a parsed reply against the chunk it answers before anything is written. Every problem
    /// rejects the first attempt, so the retry hears about all of them. The retry is never rejected:
    /// what validates is applied and only the items with a problem are left out.
    pub fn validate(
        reply: &SortReply,
        chunk: &[SortItem],
        existing: &[CategoryInfo],
        project_item_count: i64,
        is_retry: bool,
        max_new_categories: usize,
    ) -> Validation {
        let mut problems: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        let by_id: HashMap<i64, &SortItem> = chunk.iter().map(|i| (i.id, i)).collect();

        // Ids: every chunk id exactly once, nothing else.
        let mut seen = HashSet::new();
        let mut duplicates = BTreeSet::new();
        let mut foreign = BTreeSet::new();
        for a in &reply.assignments {
            if !by_id.contains_key(&a.id) {
                foreign.insert(a.id);
            } else if !seen.insert(a.id) {
                duplicates.insert(a.id);
            }
        }
        let missing: Vec<i64> = chunk.iter().map(|i| i.id).filter(|id| !seen.contains(id)).collect();

        let mut soft: Vec<String> = Vec::new();
        let mut item_problems: Vec<(i64, String)> = Vec::new();

        if !is_retry {
            if !missing.is_empty() {
                problems.push(format!("Missing ids (assign every listed item): {}", join_ids(&missing)));
            }
            if !duplicates.is_empty() {
                problems.push(format!("Duplicate ids (each id exactly once): {}", join_ids(&duplicates)));
            }
            if !foreign.is_empty() {
                problems.push(format!("Ids not in this chunk (use only the listed ids): {}", join_ids(&foreign)));
            }
        } else {
            for id in &missing {
                put(&mut item_problems, *id, format!("Item {id} was missing from the reply."));
            }
            for id in &duplicates {
                put(&mut item_problems, *id, format!("Item {id} was assigned more than once."));
            }
            if !foreign.is_empty() {
                warnings.push(format!("Ignored ids not in this chunk: {}", join_ids(&foreign)));
            }
        }

        // Categories: existing ones win, then the first new one with a matching name.
        let mut table = NameTable::of(existing);
        let mut created: Vec<NewCategoryEntity> = Vec::new();
        for nc in &reply.new_categories {
            let name = clean_name(&nc.name, MAX_CATEGORY_NAME);
            if name.is_empty() {
                soft.push(format!("A new category has an empty name (given '{}'). Use one general word.", take_chars(&nc.name, 40)));
                continue;
            }
            if table.find(&name).is_some() || category_name_problem(&nc.name).is_some() {
                continue;
            }
            table.add(&name);
            created.push(new_entity(&name, &nc.description));
        }

        // New names that are not names, with the items given them (one message per name).
        let mut bad_names: Vec<(String, String, Vec<i64>)> = Vec::new();
        // Category name -> ids, in first-use order.
        let mut used: Vec<(String, Vec<i64>)> = Vec::new();
        let mut assignments: Vec<Assignment> = Vec::new();
        let mut done = HashSet::new();
        for a in &reply.assignments {
            let Some(item) = by_id.get(&a.id) else { continue };
            if is_retry && duplicates.contains(&a.id) {
                continue;
            }
            if !done.insert(a.id) {
                continue;
            }
            // The exact name first, so an existing category longer than 40 chars still matches.
            let trimmed = a.category.trim();
            let mut resolved = if trimmed.is_empty() { None } else { table.find(trimmed) };
            if resolved.is_none() {
                let name = clean_name(&a.category, MAX_CATEGORY_NAME);
                if name.is_empty() {
                    put(
                        &mut item_problems,
                        a.id,
                        format!("Item {} has no usable category (given '{}'). Use an existing category or one general word.", a.id, take_chars(trimmed, 40)),
                    );
                    continue;
                }
                resolved = table.find(&name);
                if resolved.is_none() {
                    if let Some(why) = category_name_problem(&a.category) {
                        match bad_names.iter_mut().find(|(n, _, _)| *n == name) {
                            Some(b) => b.2.push(a.id),
                            None => bad_names.push((name, why, vec![a.id])),
                        }
                        continue;
                    }
                    // Not listed in new_categories: adopt it.
                    table.add(&name);
                    created.push(new_entity(&name, &name));
                    resolved = Some(name);
                }
            }
            let resolved = resolved.unwrap();
            match used.iter_mut().find(|(n, _)| *n == resolved) {
                Some(u) => u.1.push(a.id),
                None => used.push((resolved.clone(), vec![a.id])),
            }
            let friendly = if item.name_set_by_user || !friendly_name_usable(&a.name) {
                item.suggested_name.clone()
            } else {
                let c = clean_name(&a.name, MAX_FRIENDLY_NAME);
                if c.is_empty() {
                    item.suggested_name.clone()
                } else {
                    c
                }
            };
            assignments.push(Assignment { item_id: a.id, category_name: resolved, friendly_name: friendly });
        }
        for (name, why, ids) in &bad_names {
            let msg = format!(
                "'{name}' is not a usable category name ({why}). Items {} need an existing category or one general word.",
                join_ids(ids)
            );
            for id in ids {
                put(&mut item_problems, *id, msg.clone());
            }
        }

        // Catch-all bins stay small. On the retry a full one takes what fits, in assignment order.
        let limit = CATCH_ALL_MIN.max(project_item_count / CATCH_ALL_DIVISOR);
        let mut overflow = HashSet::new();
        for (name, ids) in used.iter_mut() {
            if !is_catch_all(name) {
                continue;
            }
            let already: i64 = existing.iter().filter(|c| c.name == *name).map(|c| c.items).sum();
            let total = already + ids.len() as i64;
            if total <= limit {
                continue;
            }
            if !is_retry {
                problems.push(format!(
                    "Too many items in a catch-all category '{name}' ({total}, at most {limit} allowed). Put them in specific categories."
                ));
                continue;
            }
            let room = (limit - already).max(0) as usize;
            let msg = format!("Catch-all category '{name}' is full (at most {limit} items).");
            for id in ids.iter().skip(room) {
                put(&mut item_problems, *id, msg.clone());
                overflow.insert(*id);
            }
            ids.truncate(room);
        }
        if !overflow.is_empty() {
            assignments.retain(|a| !overflow.contains(&a.item_id));
        }

        let count_of = |used: &Vec<(String, Vec<i64>)>, name: &str| used.iter().find(|(n, _)| n == name).map(|(_, i)| i.len()).unwrap_or(0);

        // New categories nobody uses are dropped.
        let mut new_used: Vec<NewCategoryEntity> = created.into_iter().filter(|c| count_of(&used, &c.name) > 0).collect();

        // At most max_new_categories new ones. On the retry the largest are kept (the first listed on a tie).
        if new_used.len() > max_new_categories {
            if !is_retry {
                problems.push(format!(
                    "Too many new categories ({}; at most {max_new_categories} allowed). Use existing categories and fewer, broader new ones.",
                    new_used.len()
                ));
            } else {
                let mut ranked: Vec<(usize, usize)> = new_used.iter().enumerate().map(|(i, c)| (i, count_of(&used, &c.name))).collect();
                ranked.sort_by(|a, b| b.1.cmp(&a.1)); // stable: first listed wins a tie
                let kept: HashSet<usize> = ranked.iter().take(max_new_categories).map(|(i, _)| *i).collect();
                let mut dropped = HashSet::new();
                for (i, c) in new_used.iter().enumerate() {
                    if kept.contains(&i) {
                        continue;
                    }
                    if let Some(pos) = used.iter().position(|(n, _)| *n == c.name) {
                        for id in &used[pos].1 {
                            put(&mut item_problems, *id, format!("New category '{}' was over the limit of {max_new_categories} new categories.", c.name));
                            dropped.insert(*id);
                        }
                        used.remove(pos);
                    }
                }
                assignments.retain(|a| !dropped.contains(&a.item_id));
                let mut idx = 0;
                new_used.retain(|_| {
                    let keep = kept.contains(&idx);
                    idx += 1;
                    keep
                });
            }
        }

        // Singletons among the new categories are limited.
        let singles: Vec<&NewCategoryEntity> = new_used.iter().filter(|c| count_of(&used, &c.name) == 1).collect();
        if singles.len() > SINGLETON_MIN.max(new_used.len() / SINGLETON_DIVISOR) {
            let msg = format!(
                "{} new categories hold only one item ({}). Fold them into broader categories.",
                singles.len(),
                singles.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
            );
            if is_retry {
                warnings.push(msg);
            } else {
                problems.push(msg);
            }
        }

        let mut soft_problems = soft.clone();
        for (_, msg) in &item_problems {
            if !soft_problems.contains(msg) {
                soft_problems.push(msg.clone());
            }
        }
        if !problems.is_empty() {
            problems.extend(soft_problems);
            return Validation::Rejected(problems);
        }
        if !is_retry && !soft_problems.is_empty() {
            return Validation::Rejected(soft_problems);
        }
        warnings.extend(soft);
        Validation::Ok { new_categories: new_used, assignments, warnings, left_out: item_problems }
    }
}

#[cfg(test)]
#[path = "sorting_android_cases.rs"]
mod android_cases;

#[cfg(test)]
mod tests {
    use super::validator::*;
    use super::*;

    fn item(id: i64) -> SortItem {
        SortItem {
            id,
            kind: ItemKind::Photo,
            suggested_name: format!("Item {id}"),
            tags: vec!["sea".into()],
            description: "A thing".into(),
            actions: vec![],
            name_set_by_user: false,
        }
    }

    fn cat(name: &str, items: i64) -> CategoryInfo {
        CategoryInfo { id: 1, name: name.into(), description: "d".into(), created_by: "model".into(), items }
    }

    fn assign(id: i64, c: &str) -> RawAssignment {
        RawAssignment { id, category: c.into(), name: format!("Name {id}") }
    }

    #[test]
    fn normalizes() {
        assert_eq!(normalize("The Beaches"), "beach");
        assert_eq!(normalize("Café  Parties!"), "cafe party");
        assert_eq!(normalize("Vitamin A"), "vitamin a");
        assert_eq!(normalize("Glass"), "glass");
        assert!(is_catch_all("Miscellaneous"));
        assert!(is_catch_all("Videos"));
        assert!(!is_catch_all("Animals"));
    }

    #[test]
    fn name_problems() {
        assert!(category_name_problem("Animals").is_none());
        assert!(category_name_problem("Sea Turtles").unwrap().contains("2 words"));
        assert!(category_name_problem("Nature... ").unwrap().contains("..."));
        assert!(category_name_problem("Items").unwrap().contains("prompt"));
    }

    #[test]
    fn parses_replies() {
        let r = parse_reply("```json\n{\"new_categories\":[{\"name\":\"Animals\",\"description\":\"x\"}],\"assignments\":[{\"id\":1,\"category\":\"Animals\",\"name\":\"A\"},{\"id\":2.0,\"category\":\"B\",\"name\":\"C\"},{\"id\":\"x\"}]}\n```").unwrap();
        assert_eq!(r.new_categories.len(), 1);
        assert_eq!(r.assignments.len(), 2);
        assert_eq!(r.assignments[1].id, 2);
        assert!(parse_reply("{\"other\":1}").is_none());
        assert!(parse_reply("nope").is_none());
    }

    #[test]
    fn validates_happy_path_and_adopts() {
        let chunk = vec![item(1), item(2), item(3)];
        let reply = SortReply {
            new_categories: vec![NewCategory { name: "Animals".into(), description: "Living things".into() }],
            assignments: vec![assign(1, "animals"), assign(2, "Animal"), assign(3, "Beach")],
        };
        match validate(&reply, &chunk, &[cat("Beaches", 4)], 100, false, 12) {
            Validation::Ok { new_categories, assignments, left_out, .. } => {
                assert_eq!(new_categories.len(), 1);
                assert_eq!(assignments[0].category_name, "Animals");
                assert_eq!(assignments[1].category_name, "Animals");
                assert_eq!(assignments[2].category_name, "Beaches");
                assert_eq!(assignments[0].friendly_name, "Name 1");
                assert!(left_out.is_empty());
            }
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn rejects_then_leaves_out_on_retry() {
        let chunk = vec![item(1), item(2), item(3)];
        let reply = SortReply { new_categories: vec![], assignments: vec![assign(1, "Animals"), assign(1, "Food"), assign(9, "Food")] };
        match validate(&reply, &chunk, &[], 100, false, 12) {
            Validation::Rejected(p) => {
                assert!(p.iter().any(|m| m.starts_with("Missing ids") && m.ends_with("2, 3")));
                assert!(p.iter().any(|m| m.starts_with("Duplicate ids")));
                assert!(p.iter().any(|m| m.starts_with("Ids not in this chunk")));
            }
            v => panic!("{v:?}"),
        }
        let reply = SortReply { new_categories: vec![], assignments: vec![assign(1, "Animals"), assign(2, "Two Words")] };
        match validate(&reply, &chunk, &[], 100, true, 12) {
            Validation::Ok { assignments, left_out, new_categories, .. } => {
                assert_eq!(assignments.len(), 1);
                assert_eq!(new_categories.len(), 1);
                assert_eq!(left_out.len(), 2);
            }
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn catch_all_limit() {
        let chunk: Vec<SortItem> = (1..=7).map(item).collect();
        let reply = SortReply { new_categories: vec![], assignments: (1..=7).map(|i| assign(i, "Misc")).collect() };
        assert!(matches!(validate(&reply, &chunk, &[], 40, false, 12), Validation::Rejected(_)));
        match validate(&reply, &chunk, &[], 40, true, 12) {
            Validation::Ok { assignments, left_out, .. } => {
                assert_eq!(assignments.len(), 5);
                assert_eq!(left_out.len(), 2);
            }
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn user_named_item_keeps_name() {
        let mut it = item(1);
        it.name_set_by_user = true;
        let reply = SortReply { new_categories: vec![], assignments: vec![assign(1, "Animals")] };
        match validate(&reply, &[it], &[], 10, false, 12) {
            Validation::Ok { assignments, .. } => assert_eq!(assignments[0].friendly_name, "Item 1"),
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn grammar_lists_ids_and_names() {
        let g = schema::assign_grammar(&[5, 7], &["Sea \"Life\"".to_string()]);
        assert!(g.contains(r#""\"id\":" ws "5" tail"#));
        assert!(g.contains(r#"category ::= "\"Sea \\\"Life\\\"\"" | catname"#));
        assert!(g.contains("catname  ::= \"\\\"\" [^\"\\\\\\x7F\\x00-\\x20&+/_\\x2D]{1,40} \"\\\"\""));
        assert!(schema::propose_grammar().starts_with("root     ::="));
    }

    #[test]
    fn chunk_planning() {
        let items: Vec<SortItem> = (1..=25).map(item).collect();
        assert_eq!(planner::next_chunk_len(&items, &BUDGET_LFM3_1_7B, 0), 10);
        assert_eq!(planner::proposal_sample(&items, false, &BUDGET_LFM3_1_7B).unwrap().len(), 20);
        assert!(planner::proposal_sample(&items, true, &BUDGET_LFM3_1_7B).is_none());
        assert!(prompt::assign_user(&items[..2], &[], &[], 3).contains("Add at most 3 new categories"));
        assert_eq!(cut_on_word("hello world again", 8), "hello");
    }
}
