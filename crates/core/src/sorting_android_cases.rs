//! The Android app's test cases for the `sorting` package, run against this port:
//! `SortValidatorTest`, `ChunkPlannerTest`, `SortParserTest`, `SortPromptTest` and `SortSchemaTest`.

use super::validator::{category_name_problem, is_catch_all, merge_proposals, normalize, validate};
use super::*;

fn item(id: i64) -> SortItem {
    SortItem { id, kind: ItemKind::Photo, suggested_name: format!("Item {id}"), tags: vec!["a".into(), "b".into()], description: format!("A description of item {id}."), actions: vec![], name_set_by_user: false }
}

fn named(id: i64, name: &str) -> SortItem {
    SortItem { suggested_name: name.into(), ..item(id) }
}

fn items(n: i64) -> Vec<SortItem> {
    (1..=n).map(item).collect()
}

fn category(name: &str, count: i64) -> CategoryInfo {
    CategoryInfo { id: 1, name: name.into(), description: String::new(), created_by: "model".into(), items: count }
}

fn users(name: &str, count: i64) -> CategoryInfo {
    CategoryInfo { created_by: "user".into(), ..category(name, count) }
}

/// Builds a reply as the model would write it and reads it back with the real parser.
fn reply(new_categories: &[(&str, &str)], assignments: &[(i64, &str, &str)]) -> SortReply {
    let text = json!({
        "new_categories": new_categories.iter().map(|(n, d)| json!({"name": n, "description": d})).collect::<Vec<_>>(),
        "assignments": assignments.iter().map(|(i, c, n)| json!({"id": i, "category": c, "name": n})).collect::<Vec<_>>(),
    })
    .to_string();
    parse_reply(&text).unwrap()
}

/// Every item of `chunk` into `name`, each named "N".
fn all_into(chunk: &[SortItem], name: &str) -> Vec<(i64, String, String)> {
    chunk.iter().map(|i| (i.id, name.to_string(), "N".to_string())).collect()
}

fn borrowed(v: &[(i64, String, String)]) -> Vec<(i64, &str, &str)> {
    v.iter().map(|(i, c, n)| (*i, c.as_str(), n.as_str())).collect()
}

struct Okay {
    new_categories: Vec<NewCategoryEntity>,
    assignments: Vec<Assignment>,
    warnings: Vec<String>,
    left_out: Vec<(i64, String)>,
}

impl Okay {
    fn new_names(&self) -> Vec<&str> {
        self.new_categories.iter().map(|c| c.name.as_str()).collect()
    }
    fn assigned(&self) -> Vec<i64> {
        self.assignments.iter().map(|a| a.item_id).collect()
    }
    fn categories(&self) -> Vec<&str> {
        self.assignments.iter().map(|a| a.category_name.as_str()).collect()
    }
    fn friendly(&self) -> Vec<&str> {
        self.assignments.iter().map(|a| a.friendly_name.as_str()).collect()
    }
    fn left(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.left_out.iter().map(|(i, _)| *i).collect();
        ids.sort();
        ids
    }
    fn why(&self, id: i64) -> &str {
        &self.left_out.iter().find(|(i, _)| *i == id).unwrap().1
    }
}

fn ok(v: Validation) -> Okay {
    match v {
        Validation::Ok { new_categories, assignments, warnings, left_out } => Okay { new_categories, assignments, warnings, left_out },
        other => panic!("expected Ok but was {other:?}"),
    }
}

fn rejected(v: Validation) -> Vec<String> {
    match v {
        Validation::Rejected(problems) => problems,
        other => panic!("expected Rejected but was {other:?}"),
    }
}

fn chunk3() -> Vec<SortItem> {
    vec![named(1, "Eggs"), named(2, "Toast"), named(3, "Boat")]
}

/// The first attempt on the usual chunk, in a project of 100 items.
fn first(r: &SortReply) -> Validation {
    validate(r, &chunk3(), &[], 100, false, MAX_NEW_CATEGORIES)
}

fn check(r: &SortReply, chunk: &[SortItem], existing: &[CategoryInfo], count: i64, is_retry: bool) -> Validation {
    validate(r, chunk, existing, count, is_retry, MAX_NEW_CATEGORIES)
}

// ---------------------------------------------------------------------------------------------
// SortValidatorTest
// ---------------------------------------------------------------------------------------------

#[test]
fn normalize_table() {
    for (input, expected) in [
        ("Sea lion", "sea lion"),
        ("sea lions", "sea lion"),
        ("Sea-Lions", "sea lion"),
        ("  Café   Scenes ", "cafe scene"),
        ("Parties", "party"),
        ("Beaches", "beach"),
        ("Boxes", "box"),
        ("Buses", "bus"),
        ("Glass", "glass"),
        ("Cats & Dogs", "cats dog"),
        ("Miscellaneous", "miscellaneou"),
    ] {
        assert_eq!(normalize(input), expected, "{input}");
    }
}

#[test]
fn validate_accepts_clean_reply() {
    let r = reply(&[("Food", "Meals"), ("Boats", "On water")], &[(1, "Food", "Fried eggs"), (2, "Food", "Toast"), (3, "Boats", "Boat at dock")]);
    let v = ok(first(&r));
    assert_eq!(v.new_names(), vec!["Food", "Boats"]);
    assert_eq!(v.new_categories[0].description, "Meals");
    assert_eq!(v.assignments.len(), 3);
    assert_eq!(v.assignments[0].friendly_name, "Fried eggs");
}

#[test]
fn validate_rejects_missing_duplicate_foreign_ids() {
    let r = reply(&[("Food", "Meals")], &[(1, "Food", "A"), (1, "Food", "B"), (9, "Food", "C")]);
    let problems = rejected(first(&r));
    assert!(problems.contains(&"Missing ids (assign every listed item): 2, 3".to_string()), "{problems:?}");
    assert!(problems.contains(&"Duplicate ids (each id exactly once): 1".to_string()), "{problems:?}");
    assert!(problems.contains(&"Ids not in this chunk (use only the listed ids): 9".to_string()), "{problems:?}");
}

#[test]
fn validate_adopts_unknown_category_as_new_category() {
    let r = reply(&[("Food", "Meals")], &[(1, "Food", "A"), (2, "Drinks", "B"), (3, "Drinks", "C")]);
    let v = ok(first(&r));
    assert_eq!(v.new_names(), vec!["Food", "Drinks"]);
    // No description was given.
    assert_eq!(v.new_categories[1].description, "Drinks");
    assert_eq!(v.categories(), vec!["Food", "Drinks", "Drinks"]);
    assert!(v.left_out.is_empty());
}

#[test]
fn validate_adopted_categories_are_cleaned_and_merged_existing_and_listed_win() {
    let r = reply(
        &[("Boats", "On water")],
        &[
            (1, "sea lion", "A"),  // the existing user category wins
            (2, "boat", "B"),      // the listed new category wins
            (3, "Harbors:", "C"),  // adopted, cleaned
            (4, "harbor", "D"),    // a near-duplicate of the adopted one
            (5, "Boats", "E"),
            (6, "Sea Lions", "F"),
        ],
    );
    let v = ok(check(&r, &items(6), &[users("Sea Lions", 9)], 100, false));
    assert_eq!(v.new_names(), vec!["Boats", "Harbors"]);
    assert_eq!(v.categories(), vec!["Sea Lions", "Boats", "Harbors", "Harbors", "Boats", "Sea Lions"]);
}

fn lettered(n: usize) -> Vec<String> {
    "abcdefghijklmnopqrst".chars().take(n).map(|c| format!("Category{c}")).collect()
}

#[test]
fn validate_adopted_categories_count_toward_the_twelve_cap() {
    let chunk = items(13);
    let names = lettered(13);
    let assignments: Vec<(i64, &str, &str)> = chunk.iter().zip(&names).map(|(i, n)| (i.id, n.as_str(), "N")).collect();
    let problems = rejected(check(&reply(&[], &assignments), &chunk, &[], 100, false));
    assert!(problems.iter().any(|p| p.contains("12") && p.contains("new categories")), "{problems:?}");
}

/// I3: on the retry the 12 largest new categories are kept and the others' items left out.
#[test]
fn validate_too_many_new_categories_on_retry_keeps_the_twelve_largest() {
    let chunk = items(20);
    // "Big" holds 8 items, then 12 one-item categories; the last of those (by order) loses.
    let names = lettered(12);
    let assignments: Vec<(i64, &str, &str)> = chunk.iter().map(|i| (i.id, if i.id <= 8 { "Big" } else { names[(i.id - 9) as usize].as_str() }, "N")).collect();
    let retry = ok(check(&reply(&[], &assignments), &chunk, &[], 100, true));
    let mut expected = vec!["Big"];
    expected.extend(names[..11].iter().map(String::as_str));
    assert_eq!(retry.new_names(), expected);
    assert_eq!(retry.left(), vec![20]);
    assert!(retry.why(20).contains("12"));
    assert_eq!(retry.assigned(), (1..=19).collect::<Vec<_>>());
}

/// I3: id problems on the retry leave those items out instead of sinking the chunk.
#[test]
fn validate_id_problems_on_retry_leave_only_those_items_out() {
    let r = reply(&[("Food", "Meals")], &[(1, "Food", "A"), (2, "Food", "B"), (2, "Boats", "B2"), (3, "Food", "C"), (99, "Food", "X")]);
    let retry = ok(check(&r, &items(5), &[], 100, true));
    assert_eq!(retry.assigned(), vec![1, 3]);
    assert_eq!(retry.left(), vec![2, 4, 5]);
    assert!(retry.why(2).contains("more than once"));
    assert!(retry.why(4).to_lowercase().contains("missing"));
    assert!(retry.warnings.iter().any(|w| w.contains("99")), "{:?}", retry.warnings);
    // "Boats" was only used by the duplicate, so it isn't created.
    assert_eq!(retry.new_names(), vec!["Food"]);
}

/// A small fixed-seed generator, so the round that fails can be run again.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % below
    }
    fn pick<'a>(&mut self, pool: &'a [String]) -> &'a str {
        &pool[self.next(pool.len())]
    }
}

/// I3: whatever the retry's reply, every item of the chunk ends assigned or left out, never both.
#[test]
fn validate_retry_every_item_ends_assigned_or_left_out() {
    let mut random = Lcg(2026);
    let mut pool: Vec<String> = [
        "Food", "Boats", "Misc", "Photos", "", "///", "Nature no... let's see", "Sea Lions", "sea lion", "Hiking Trails", "One two three four five six", "Items to sort", "Beach",
        "Sunset Over the Sea",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    pool.extend((1..=20).map(|i| format!("Topic{i}")));
    for round in 0..400 {
        let chunk = items(1 + random.next(29) as i64);
        let ids: Vec<i64> = chunk.iter().map(|i| i.id).collect();
        let mut assignments: Vec<(i64, String, String)> = Vec::new();
        for id in &ids {
            match random.next(10) {
                0 => {} // missing
                1 => {
                    for _ in 0..2 {
                        assignments.push((*id, random.pick(&pool).to_string(), "N".into())); // duplicate
                    }
                }
                _ => assignments.push((*id, random.pick(&pool).to_string(), format!("Name {id}"))),
            }
        }
        for _ in 0..random.next(3) {
            assignments.push((100 + random.next(50) as i64, random.pick(&pool).to_string(), "X".into())); // foreign
        }
        for i in (1..assignments.len()).rev() {
            assignments.swap(i, random.next(i + 1));
        }
        let listed: Vec<(String, String)> = (0..random.next(4)).map(|_| (random.pick(&pool).to_string(), "d".to_string())).filter(|(n, _)| !n.is_empty()).collect();
        let existing = [category("Beach", random.next(8) as i64), CategoryInfo { id: 2, ..category("Misc", random.next(8) as i64) }];
        let max_new = if random.next(2) == 0 { 12 } else { 3 };
        let r = reply(&listed.iter().map(|(n, d)| (n.as_str(), d.as_str())).collect::<Vec<_>>(), &borrowed(&assignments));
        let okay = match validate(&r, &chunk, &existing, 40, true, max_new) {
            Validation::Ok { new_categories, assignments, warnings, left_out } => Okay { new_categories, assignments, warnings, left_out },
            other => panic!("round {round}: the retry was {other:?}"),
        };
        let assigned = okay.assigned();
        let unique: HashSet<i64> = assigned.iter().copied().collect();
        assert_eq!(assigned.len(), unique.len(), "round {round}: assigned once");
        let left: HashSet<i64> = okay.left().into_iter().collect();
        assert!(unique.is_disjoint(&left), "round {round}: assigned and left out");
        assert_eq!(unique.union(&left).copied().collect::<HashSet<_>>(), ids.iter().copied().collect::<HashSet<_>>(), "round {round}: every item");
        assert!(okay.new_categories.len() <= max_new, "round {round}: at most {max_new} new");
        let known: HashSet<&str> = existing.iter().map(|c| c.name.as_str()).chain(okay.new_names()).collect();
        assert!(okay.categories().iter().all(|c| known.contains(c)), "round {round}: known categories");
    }
}

fn misc_reply(name: &str, n: i64, listed: bool) -> SortReply {
    let assignments = all_into(&items(n), name);
    let listing = [(name, "d")];
    reply(if listed { &listing } else { &[] }, &borrowed(&assignments))
}

#[test]
fn validate_adopted_catch_all_counts_toward_the_limit() {
    let problems = rejected(check(&misc_reply("Misc", 6, false), &items(6), &[], 40, false));
    assert!(problems.iter().any(|p| p.starts_with("Too many items in a catch-all category")), "{problems:?}");
    let retry = ok(check(&misc_reply("Misc", 6, false), &items(6), &[], 40, true));
    assert_eq!(retry.assigned(), vec![1, 2, 3, 4, 5]);
    assert_eq!(retry.left(), vec![6]);
}

/// Six items, each in its own new category.
fn singletons() -> (Vec<SortItem>, SortReply) {
    let names = ["Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot"];
    let chunk = items(6);
    let assignments: Vec<(i64, &str, &str)> = chunk.iter().zip(names).map(|(i, n)| (i.id, n, "N")).collect();
    let listed: Vec<(&str, &str)> = names.iter().map(|n| (*n, "d")).collect();
    (chunk, reply(&listed, &assignments))
}

#[test]
fn validate_too_many_singletons_rejected_once_listed_or_adopted() {
    let (chunk, listed) = singletons();
    let adopted = SortReply { new_categories: vec![], assignments: listed.assignments.clone() };
    for r in [listed, adopted] {
        let problems = rejected(check(&r, &chunk, &[], 100, false));
        assert!(problems.iter().any(|p| p.contains("one item") && p.contains("broader")), "{problems:?}");
        let retry = ok(check(&r, &chunk, &[], 100, true));
        assert_eq!(retry.new_categories.len(), 6);
        assert!(retry.warnings.iter().any(|w| w.contains("one item")));
    }
}

#[test]
fn validate_singleton_problem_is_reported_alongside_other_problems() {
    let (chunk, r) = singletons();
    let bad = SortReply { new_categories: r.new_categories.clone(), assignments: r.assignments[1..].to_vec() };
    let problems = rejected(check(&bad, &chunk, &[], 100, false));
    assert!(problems.iter().any(|p| p.starts_with("Missing ids")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("one item")), "{problems:?}");
}

#[test]
fn validate_few_singletons_allowed() {
    let r = reply(&[("Alpha", "d"), ("Bravo", "d"), ("Charlie", "d")], &[(1, "Alpha", "N"), (2, "Bravo", "N"), (3, "Charlie", "N"), (4, "Charlie", "N"), (5, "Charlie", "N"), (6, "Charlie", "N")]);
    ok(check(&r, &items(6), &[], 100, false));
}

#[test]
fn validate_existing_categories_never_count_as_singletons() {
    let existing = [category("A", 0), category("B", 0), category("C", 0), category("D", 0)];
    let r = reply(&[], &[(1, "A", "N"), (2, "B", "N"), (3, "C", "N")]);
    ok(check(&r, &items(3), &existing, 100, false));
}

#[test]
fn validate_unusable_category_rejects_first_then_leaves_only_that_item_out() {
    let r = reply(&[("Food", "Meals")], &[(1, "Food", "A"), (2, "Food", "B"), (3, "///", "C")]);
    let problems = rejected(first(&r));
    assert!(problems.iter().any(|p| p.contains('3') && p.contains("usable category")), "{problems:?}");
    let retry = ok(check(&r, &chunk3(), &[], 100, true));
    assert_eq!(retry.assigned(), vec![1, 2]);
    assert_eq!(retry.left(), vec![3]);
    assert!(retry.why(3).contains("usable category"));
    assert_eq!(retry.new_names(), vec!["Food"]);
}

#[test]
fn validate_missing_ids_reject_the_first_attempt_and_are_left_out_on_retry() {
    let r = reply(&[("Food", "Meals")], &[(1, "Food", "A"), (2, "///", "B")]);
    assert!(rejected(first(&r)).iter().any(|p| p.starts_with("Missing ids")));
    let retry = ok(check(&r, &chunk3(), &[], 100, true));
    assert_eq!(retry.assigned(), vec![1]);
    assert_eq!(retry.left(), vec![2, 3]);
}

#[test]
fn validate_matches_existing_category_case_insensitively() {
    let r = reply(&[], &[(1, "food", "N1"), (2, "food", "N2"), (3, "food", "N3")]);
    let v = ok(check(&r, &chunk3(), &[category("Food", 5)], 100, false));
    assert!(v.new_categories.is_empty());
    assert_eq!(v.categories(), vec!["Food"; 3]);
}

#[test]
fn normalize_merges_near_duplicates_into_existing() {
    let r = reply(&[("Sea lion", "x"), ("sea lions", "y"), ("Sea-Lions", "z")], &[(1, "Sea lion", "A"), (2, "sea lions", "B"), (3, "Sea-Lions", "C")]);
    let v = ok(check(&r, &chunk3(), &[users("Sea Lions", 9)], 100, false));
    assert!(v.new_categories.is_empty());
    assert_eq!(v.categories(), vec!["Sea Lions"; 3]);
}

#[test]
fn normalize_merges_new_duplicates_into_the_first() {
    let r = reply(&[("Seal", "x"), ("seals", "y"), ("SEALS.", "z")], &[(1, "Seal", "A"), (2, "seals", "B"), (3, "SEALS.", "C")]);
    let v = ok(first(&r));
    assert_eq!(v.new_names(), vec!["Seal"]);
    assert_eq!(v.categories(), vec!["Seal"; 3]);
}

#[test]
fn validate_drops_new_categories_nobody_uses() {
    let r = reply(&[("Food", "Meals"), ("Unused", "None")], &[(1, "Food", "N1"), (2, "Food", "N2"), (3, "Food", "N3")]);
    assert_eq!(ok(first(&r)).new_names(), vec!["Food"]);
}

#[test]
fn validate_category_names_are_cleaned_and_capped() {
    let long = "Supercalifragilisticexpialidociousmountainranges";
    let r = reply(&[(" Animals. ", "d"), (long, "d")], &[(1, " Animals. ", "A"), (2, long, "B"), (3, " Animals. ", "C")]);
    let v = ok(first(&r));
    assert_eq!(v.new_names()[0], "Animals");
    assert_eq!(v.new_names()[1].chars().count(), 40);
}

#[test]
fn validate_empty_category_name_rejected() {
    let r = reply(&[("///", "d")], &[(1, "Food", "N"), (2, "Food", "N"), (3, "///", "N")]);
    let existing = [category("Food", 1)];
    let problems = rejected(check(&r, &chunk3(), &existing, 100, false));
    assert!(problems.iter().any(|p| p.contains("empty")), "{problems:?}");
    // On the retry the unusable listing is only a warning and item 3 alone is left out.
    let retry = ok(check(&r, &chunk3(), &existing, 100, true));
    assert_eq!(retry.assigned(), vec![1, 2]);
    assert_eq!(retry.left(), vec![3]);
    assert!(retry.warnings.iter().any(|w| w.contains("empty")), "{:?}", retry.warnings);
}

#[test]
fn validate_empty_friendly_name_falls_back_to_suggested() {
    let r = reply(&[("Food", "Meals")], &[(1, "Food", "  "), (2, "Food", "///"), (3, "Food", "Fine name")]);
    assert_eq!(ok(first(&r)).friendly(), vec!["Eggs", "Toast", "Fine name"]);
}

#[test]
fn validate_friendly_name_capped_at_60() {
    let long = "Abcdefghij".repeat(9);
    let r = reply(&[("Food", "d")], &[(1, "Food", &long), (2, "Food", "one two three four five six seven  "), (3, "Food", &"word ".repeat(30))]);
    let v = ok(first(&r));
    assert!(v.friendly().iter().all(|n| n.chars().count() <= 60 && !n.ends_with(' ')), "{:?}", v.friendly());
    assert_eq!(v.friendly()[0].chars().count(), 60);
    // Thirty words is a sentence, not a name.
    assert_eq!(v.friendly()[2], "Boat");
}

#[test]
fn validate_keeps_user_named_item_name() {
    let chunk = [SortItem { name_set_by_user: true, ..named(1, "Eggs") }, named(2, "Toast")];
    let r = reply(&[("Food", "d")], &[(1, "Food", "Model rename"), (2, "Food", "Model toast")]);
    assert_eq!(ok(check(&r, &chunk, &[], 100, false)).friendly(), vec!["Eggs", "Model toast"]);
}

#[test]
fn validate_catch_all_limit() {
    for n in [4, 5] {
        ok(check(&misc_reply("Misc", n, true), &items(n), &[], 40, false));
    }
    for n in [6, 12] {
        let problems = rejected(check(&misc_reply("Misc", n, true), &items(n), &[], 40, false));
        assert!(problems.iter().any(|p| p.starts_with("Too many items in a catch-all category")), "{problems:?}");
    }
}

#[test]
fn media_kind_names_are_catch_all() {
    let kinds = ["photo", "video", "image", "picture", "clip", "footage", "media", "file"];
    for k in kinds {
        assert!(is_catch_all(k), "{k}");
        let plural = format!("{}{}s", k[..1].to_uppercase(), &k[1..]);
        assert!(is_catch_all(&plural), "{plural}");
    }
    for k in ["VIDEOS", "Pictures", "Files"] {
        assert!(is_catch_all(k), "{k}");
    }
    for k in ["Photography", "Beach", "Video Games"] {
        assert!(!is_catch_all(k), "{k}");
    }
}

#[test]
fn validate_media_kind_category_falls_under_the_catch_all_limit() {
    ok(check(&misc_reply("Photos", 5, false), &items(5), &[], 40, true));
    let problems = rejected(check(&misc_reply("Photos", 6, false), &items(6), &[], 40, false));
    assert!(problems.iter().any(|p| p.starts_with("Too many items in a catch-all category 'Photos'")), "{problems:?}");
    rejected(check(&misc_reply("Video", 6, true), &items(6), &[], 40, false));
}

#[test]
fn validate_catch_all_limit_scales_with_project() {
    ok(check(&misc_reply("Other", 20, true), &items(20), &[], 400, false));
    rejected(check(&misc_reply("Other", 21, true), &items(21), &[], 400, false));
}

#[test]
fn validate_catch_all_counts_existing_items_and_plurals() {
    let r = misc_reply("Miscellaneous", 3, false);
    rejected(check(&r, &items(3), &[category("Miscellaneous", 3)], 40, false));
    ok(check(&r, &items(3), &[category("Miscellaneous", 2)], 40, false));
    rejected(check(&misc_reply("Others", 6, true), &items(6), &[], 40, false));
}

#[test]
fn validate_finds_existing_category_longer_than_40_chars_by_exact_name() {
    let long = "A category name that is far longer than forty characters";
    let existing = [category(long, 4)];
    let v = ok(check(&reply(&[], &borrowed(&all_into(&items(3), long))), &items(3), &existing, 100, false));
    assert_eq!(v.categories(), vec![long; 3]);
    let shouty = format!("  {} ", long.to_uppercase());
    let v = ok(check(&reply(&[], &borrowed(&all_into(&items(3), &shouty))), &items(3), &existing, 100, false));
    assert_eq!(v.categories(), vec![long; 3]);
}

#[test]
fn validate_rejects_more_than_twelve_new_categories() {
    let names = lettered(13);
    for (n, too_many) in [(13usize, true), (12, false)] {
        let chunk = items(n as i64);
        let listed: Vec<(&str, &str)> = names[..n].iter().map(|n| (n.as_str(), "d")).collect();
        let assignments: Vec<(i64, &str, &str)> = chunk.iter().zip(&names).map(|(i, n)| (i.id, n.as_str(), "N")).collect();
        let problems = rejected(check(&reply(&listed, &assignments), &chunk, &[], 100, false));
        assert_eq!(problems.iter().any(|p| p.contains("at most 12 allowed") && p.contains("new categories")), too_many, "{problems:?}");
    }
}

#[test]
fn merge_proposals_cleans_dedupes_and_drops_existing() {
    let proposals: Vec<NewCategory> =
        [("Seal", "x"), ("seals", "dup"), ("Food", "already there"), ("///", "empty"), ("Boats", "y")].iter().map(|(n, d)| NewCategory { name: n.to_string(), description: d.to_string() }).collect();
    let out = merge_proposals(&proposals, &[category("Foods", 2)]);
    assert_eq!(out.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["Seal", "Boats"]);
}

#[test]
fn merge_proposals_keeps_at_most_twelve() {
    let proposals: Vec<NewCategory> = lettered(20).into_iter().map(|name| NewCategory { name, description: "d".into() }).collect();
    assert_eq!(merge_proposals(&proposals, &[]).len(), 12);
}

#[test]
fn merge_proposals_skips_leaked_and_multi_word_names() {
    let of = |names: &[&str]| names.iter().map(|n| NewCategory { name: n.to_string(), description: "x".into() }).collect::<Vec<_>>();
    let names = |out: Vec<NewCategoryEntity>| out.into_iter().map(|c| c.name).collect::<Vec<_>>();
    assert_eq!(names(merge_proposals(&of(&["Nature forest is natural elements, but", "Items to sort", "Boats"]), &[])), vec!["Boats"]);
    assert_eq!(names(merge_proposals(&of(&["Sea Turtles and Seals", "Animals"]), &[])), vec!["Animals"]);
}

/// Names that are not names: the small model leaks its reasoning or the prompt's own wording.
#[test]
fn bad_category_names_are_flagged() {
    for n in [
        "Nature no... let's see existing Nature",
        "Nature forest is natural elements, but",
        "Items to sort",
        "Beach…",
        "Sunny days, ",
        "Dogs and",
        "Pictures of",
        "Sunset with",
        "Beach is",
        "Lunch or",
        "Walk the",
        "Lets see Nature",
        "One two three four five six",
        "Sea Turtles",
        "New Categories",
        "existing categories",
        "Category",
    ] {
        assert!(category_name_problem(n).is_some(), "{n}");
    }
    for n in ["Animals", "Sunsets", "Mama's", "Food", "Photo"] {
        assert_eq!(category_name_problem(n), None, "{n}");
    }
}

#[test]
fn validate_leaked_category_name_rejects_first_then_leaves_its_items_out() {
    let bad = "Nature no... let's see existing Nature";
    let r = reply(&[("Food", "Meals"), (bad, "d")], &[(1, "Food", "A"), (2, bad, "B"), (3, "Food", "C"), (4, "Items to sort", "D"), (5, "Food", "E")]);
    let problems = rejected(check(&r, &items(5), &[], 100, false));
    assert!(problems.iter().any(|p| p.contains('2') && p.contains("not a usable category name")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains('4') && p.contains("Items to sort")), "{problems:?}");
    let retry = ok(check(&r, &items(5), &[], 100, true));
    assert_eq!(retry.assigned(), vec![1, 3, 5]);
    assert_eq!(retry.left(), vec![2, 4]);
    assert_eq!(retry.new_names(), vec!["Food"]);
}

/// An existing category is matched before any name rule: the user's names are theirs.
#[test]
fn validate_leaked_name_matching_an_existing_category_is_not_a_problem() {
    let r = reply(&[], &borrowed(&all_into(&chunk3(), "Items to sort")));
    let v = ok(check(&r, &chunk3(), &[users("Items to sort", 2)], 100, false));
    assert_eq!(v.categories(), vec!["Items to sort"; 3]);
}

#[test]
fn validate_unused_leaked_listing_does_not_reject() {
    let r = reply(&[("Food", "Meals"), ("Dogs and", "d")], &borrowed(&all_into(&chunk3(), "Food")));
    assert_eq!(ok(first(&r)).new_names(), vec!["Food"]);
}

#[test]
fn validate_bad_friendly_name_falls_back_to_suggested_without_retry() {
    let r = reply(
        &[("Food", "Meals")],
        &[(1, "Food", "Simple images of a photo subject like basketball, bus stop,"), (2, "Food", "Toast... let me think"), (3, "Food", "One two three four five six seven eight")],
    );
    let v = ok(first(&r));
    assert_eq!(v.friendly(), vec!["Eggs", "Toast", "One two three four five six seven eight"]);
    assert!(v.left_out.is_empty());
    let ellipsis = reply(&[("Food", "Meals")], &[(1, "Food", "Boat at dock…"), (2, "Food", "Boat at dock…"), (3, "Food", "Boat at dock…")]);
    assert_eq!(ok(first(&ellipsis)).friendly(), vec!["Eggs", "Toast", "Boat"]);
}

/// A full catch-all must not sink the chunk on the retry.
#[test]
fn validate_full_catch_all_on_retry_takes_what_fits_and_leaves_the_overflow_out() {
    let chunk = items(10);
    let assignments: Vec<(i64, &str, &str)> = chunk.iter().map(|i| (i.id, if [2, 4, 6, 8].contains(&i.id) { "Misc" } else { "Food" }, "N")).collect();
    let r = reply(&[("Food", "Meals")], &assignments);
    let existing = [category("Misc", 4)]; // the limit is max(5, 40/20) = 5: one slot left
    let problems = rejected(check(&r, &chunk, &existing, 40, false));
    assert!(problems.iter().any(|p| p.starts_with("Too many items in a catch-all category 'Misc'")), "{problems:?}");
    let retry = ok(check(&r, &chunk, &existing, 40, true));
    assert_eq!(retry.assignments.len(), 7);
    assert_eq!(retry.assignments.iter().filter(|a| a.category_name == "Misc").map(|a| a.item_id).collect::<Vec<_>>(), vec![2]);
    assert_eq!(retry.left(), vec![4, 6, 8]);
    assert!(retry.left_out.iter().all(|(_, why)| why.contains("Misc")));
}

#[test]
fn validate_catch_all_room_on_retry_new_takes_what_fits_full_existing_takes_none() {
    let chunk = items(8);
    let assignments: Vec<(i64, &str, &str)> = chunk.iter().map(|i| (i.id, if i.id <= 6 { "Food" } else { "Clip" }, "N")).collect();
    let r = reply(&[], &assignments);
    // "Clip" is a new catch-all: 2 items with a limit of 5 fit, so both are taken.
    let retry = ok(check(&r, &chunk, &[category("Videos", 5)], 40, true));
    assert_eq!(retry.new_names(), vec!["Food", "Clip"]);
    let full = ok(check(&r, &chunk, &[category("Clips", 5)], 40, true));
    assert_eq!(full.left(), vec![7, 8]);
    assert_eq!(full.new_names(), vec!["Food"]);
}

/// Articles don't make a new category.
#[test]
fn normalize_drops_articles() {
    assert_eq!(normalize("Sunset Over Sea"), normalize("Sunset Over the Sea"));
    assert_eq!(normalize("A Day at the Beach"), "day at beach");
    assert_eq!(normalize("An Apple Tree"), "apple tree");
    assert_eq!(normalize("Theater"), "theater");
    assert_eq!(normalize("Anchors"), "anchor");
    assert_eq!(normalize("The"), "the");
    // A trailing "a" is a name, not an article.
    assert_eq!(normalize("Vitamin A"), "vitamin a");
    assert_ne!(normalize("Vitamin A"), normalize("Vitamin"));
    assert_ne!(normalize("Group A"), normalize("Group"));
}

#[test]
fn validate_merges_names_that_differ_only_by_articles() {
    let r = reply(&[], &borrowed(&all_into(&chunk3(), "Sunset Over the Sea")));
    let v = ok(check(&r, &chunk3(), &[category("Sunset Over Sea", 4)], 100, false));
    assert!(v.new_categories.is_empty());
    assert_eq!(v.categories(), vec!["Sunset Over Sea"; 3]);
}

/// M9: the feedback states the same word limit the rule enforces.
#[test]
fn category_name_problem_word_limit_feedback_matches_the_rule() {
    assert_eq!(category_name_problem("Sea Turtles").as_deref(), Some("it has 2 words; use one general word"));
    let problems = rejected(first(&reply(&[], &borrowed(&all_into(&chunk3(), "Sea Turtles"))))).join(" | ");
    assert!(problems.contains("use one general word"), "{problems}");
    assert!(problems.contains("need an existing category or one general word"), "{problems}");
}

/// A new category is one general word; the specific subject stays in the tags and the item's name.
#[test]
fn category_name_problem_words_joined_by_punctuation_count_as_words() {
    for n in ["Turtles&Seals", "Sea-Life", "Food/Drink", "Water_Sports", "Surf+Sail"] {
        assert_eq!(category_name_problem(n).as_deref(), Some("it has 2 words; use one general word"), "{n}");
    }
}

#[test]
fn validate_existing_multi_word_categories_still_take_items() {
    let r = reply(&[], &[(1, "Sea Lions", "N"), (2, "Sunset Over Sea", "N"), (3, "Sunset Over Sea", "N")]);
    let v = ok(check(&r, &chunk3(), &[users("Sea Lions", 2), category("Sunset Over Sea", 4)], 100, false));
    assert!(v.new_categories.is_empty());
    assert_eq!(v.categories(), vec!["Sea Lions", "Sunset Over Sea", "Sunset Over Sea"]);
}

// ---------------------------------------------------------------------------------------------
// ChunkPlannerTest
// ---------------------------------------------------------------------------------------------

fn library(n: i64) -> Vec<SortItem> {
    (1..=n)
        .map(|i| SortItem {
            id: i,
            kind: if i % 3 == 0 { ItemKind::Video } else { ItemKind::Photo },
            suggested_name: format!("Sea lions basking on the rocks {i}"),
            tags: ["sea lions", "rocks", "sunny", "wildlife", "coast", "animals"].iter().map(|s| s.to_string()).collect(),
            description: "Several sea lions rest on a rocky shoreline while waves roll in behind them, with a few birds nearby. ".repeat(3),
            actions: if i % 3 == 0 { vec!["0:00 Pan right across the rocks".into(), "0:04 One slides into the water".into()] } else { vec![] },
            name_set_by_user: false,
        })
        .collect()
}

fn budget(context_tokens: usize, max_chunk: usize, propose_sample: usize) -> ChunkBudget {
    ChunkBudget { context_tokens, max_chunk, propose_sample, max_new_with_categories: MAX_NEW_CATEGORIES }
}

fn limit(b: &ChunkBudget) -> usize {
    (planner::FIT_FRACTION * b.context_tokens as f64) as usize
}

/// The chunks the categorizer would take, one after another, beside an empty category list.
fn chunks(all: &[SortItem], b: &ChunkBudget) -> Vec<Vec<i64>> {
    let mut out = Vec::new();
    let mut rest = all;
    while !rest.is_empty() {
        let n = planner::next_chunk_len(rest, b, 0);
        assert!(n >= 1 && n <= b.max_chunk);
        assert!(n == 1 || planner::chunk_tokens(&rest[..n], 0) <= limit(b));
        out.push(rest[..n].iter().map(|i| i.id).collect());
        rest = &rest[n..];
    }
    out
}

#[test]
fn budgets_are_the_spike_values() {
    assert_eq!((BUDGET_CLAUDE.context_tokens, BUDGET_CLAUDE.max_chunk, BUDGET_CLAUDE.propose_sample, BUDGET_CLAUDE.max_new_with_categories), (200_000, 200, 2000, 12));
    assert_eq!((BUDGET_LFM3_1_7B.context_tokens, BUDGET_LFM3_1_7B.max_chunk, BUDGET_LFM3_1_7B.propose_sample, BUDGET_LFM3_1_7B.max_new_with_categories), (16384, 10, 20, 3));
}

#[test]
fn chunk_plan_fits_context_for_1600_items() {
    let all = library(1600);
    let plan = chunks(&all, &BUDGET_LFM3_1_7B);
    assert_eq!(plan.concat(), all.iter().map(|i| i.id).collect::<Vec<_>>());
    assert_eq!(plan.len(), 160);
}

#[test]
fn chunk_plan_claude_budget_uses_chunks_of_200() {
    let plan = chunks(&library(1600), &BUDGET_CLAUDE);
    assert_eq!(plan.len(), 8);
    assert!(plan.iter().all(|c| c.len() == 200));
}

#[test]
fn chunk_plan_small_context_shrinks_chunks_below_max_chunk() {
    let all = library(200);
    let plan = chunks(&all, &budget(4096, 100, 50));
    assert!(plan.iter().all(|c| c.len() < 100));
    assert_eq!(plan.concat(), all.iter().map(|i| i.id).collect::<Vec<_>>());
}

#[test]
fn chunk_plan_one_huge_item_still_gets_its_own_chunk() {
    let huge = named(1, &"x ".repeat(100_000));
    assert_eq!(chunks(&[huge, item(2)], &budget(4096, 10, 10)), vec![vec![1], vec![2]]);
    assert_eq!(planner::next_chunk_len(&[], &BUDGET_LFM3_1_7B, 0), 0);
}

#[test]
fn chunk_plan_proposal_only_on_first_batch_over_chunk() {
    let b = BUDGET_LFM3_1_7B;
    assert!(planner::proposal_sample(&library(500), true, &b).is_none(), "has categories");
    assert!(planner::proposal_sample(&library(10), false, &b).is_none(), "fits in one chunk");
    assert!(planner::proposal_sample(&[], false, &b).is_none());
    assert_eq!(planner::proposal_sample(&library(500), false, &b).unwrap().len(), b.propose_sample);
}

#[test]
fn chunk_plan_proposal_sample_is_even_and_ordered() {
    let p = planner::proposal_sample(&library(100), false, &budget(200_000, 10, 10)).unwrap();
    let ids: Vec<i64> = p.iter().map(|i| i.id).collect();
    assert_eq!(ids, vec![1, 11, 21, 31, 41, 51, 61, 71, 81, 91]);
}

#[test]
fn chunk_plan_proposal_takes_everything_when_sample_exceeds_items() {
    assert_eq!(planner::proposal_sample(&library(30), false, &budget(200_000, 10, 2000)).unwrap().len(), 30);
}

#[test]
fn chunk_plan_proposal_stays_inside_budget() {
    let b = budget(4096, 10, 2000);
    let p = planner::proposal_sample(&library(800), false, &b).unwrap();
    assert!((1..800).contains(&p.len()));
    assert!(planner::propose_tokens(&p) <= limit(&b));
}

/// I2: the category list is sized from the categories themselves, not a fixed reserve.
#[test]
fn category_tokens_count_every_category_line() {
    let categories: Vec<CategoryInfo> = (1..=3).map(|i| CategoryInfo { description: format!("About topic {i}"), ..category(&format!("Topic {i}"), i) }).collect();
    let expected: usize = categories.iter().map(|c| planner::estimate_tokens(&prompt::category_line(c)) + 1).sum();
    assert_eq!(planner::category_tokens(&categories), expected);
    assert_eq!(planner::category_tokens(&[]), 0);
}

#[test]
fn next_chunk_shrinks_as_the_category_list_grows() {
    let all = library(50);
    let b = BUDGET_LFM3_1_7B;
    assert_eq!(planner::next_chunk_len(&all, &b, 0), 10);
    // Grow the list until five items no longer fit beside it.
    let mut many: Vec<CategoryInfo> = Vec::new();
    while planner::chunk_tokens(&all[..5], planner::category_tokens(&many)) <= limit(&b) {
        let n = many.len();
        many.push(CategoryInfo { description: format!("A long description of topic {n} ").repeat(4), ..category(&format!("Topic number {n}"), 3) });
    }
    let tokens = planner::category_tokens(&many);
    let n = planner::next_chunk_len(&all, &b, tokens);
    assert!((1..10).contains(&n), "{n}");
    assert!(planner::chunk_tokens(&all[..n], tokens) <= limit(&b));
    assert!(planner::chunk_tokens(&all[..n + 1], tokens) > limit(&b));
    // The one-item rule stands: a list too big for any item still yields a chunk of one.
    assert_eq!(planner::next_chunk_len(&all, &b, b.context_tokens), 1);
}

#[test]
fn estimate_tokens_is_chars_over_3_point_5_rounded_up() {
    for (chars, tokens) in [(0, 0), (1, 1), (4, 2), (7, 2), (8, 3)] {
        assert_eq!(planner::estimate_tokens(&"a".repeat(chars)), tokens, "{chars}");
    }
}

// ---------------------------------------------------------------------------------------------
// SortParserTest
// ---------------------------------------------------------------------------------------------

#[test]
fn parse_reads_valid_reply() {
    let r = parse_reply(r#"{"new_categories":[{"name":"Food","description":"Meals"}],"assignments":[{"id":1,"category":"Food","name":"Eggs"},{"id":2,"category":"Food","name":"Toast"}]}"#).unwrap();
    assert_eq!(r.new_categories, vec![NewCategory { name: "Food".into(), description: "Meals".into() }]);
    assert_eq!(
        r.assignments,
        vec![RawAssignment { id: 1, category: "Food".into(), name: "Eggs".into() }, RawAssignment { id: 2, category: "Food".into(), name: "Toast".into() }]
    );
}

#[test]
fn parse_returns_none_on_garbage() {
    for text in ["I cannot do that.", "{\"assignments\": [ {\"id\":1", "{}", "", "} {"] {
        assert!(parse_reply(text).is_none(), "{text}");
    }
}

#[test]
fn parse_proposal_reply_has_no_assignments() {
    let r = parse_reply(r#"{"new_categories":[{"name":"Food","description":"Meals"}]}"#).unwrap();
    assert_eq!(r.new_categories.len(), 1);
    assert!(r.assignments.is_empty());
}

#[test]
fn parse_tolerates_fences_and_prose() {
    let r = parse_reply("Here:\n```json\n{\"new_categories\":[],\"assignments\":[{\"id\":5,\"category\":\"A\",\"name\":\"B\"}]}\n```").unwrap();
    assert_eq!(r.assignments[0].id, 5);
}

#[test]
fn parse_skips_entries_without_readable_id() {
    let r = parse_reply(r#"{"new_categories":[],"assignments":[{"id":"x","category":"A","name":"B"},{"id":2,"category":"A","name":"B"},{"id":2.5,"category":"A","name":"B"},{"category":"A","name":"B"}]}"#).unwrap();
    assert_eq!(r.assignments.iter().map(|a| a.id).collect::<Vec<_>>(), vec![2]);
}

#[test]
fn parse_accepts_ids_written_as_strings_or_decimals() {
    let r = parse_reply(r#"{"new_categories":[],"assignments":[{"id":"3","category":"A","name":"B"},{"id":4.0,"category":"A","name":"B"}]}"#).unwrap();
    assert_eq!(r.assignments.iter().map(|a| a.id).collect::<Vec<_>>(), vec![3, 4]);
}

// ---------------------------------------------------------------------------------------------
// SortPromptTest
// ---------------------------------------------------------------------------------------------

#[test]
fn item_line_cuts_description_on_word() {
    let words = (1..=80).map(|i| format!("word{i}")).collect::<Vec<_>>().join(" ");
    let line = prompt::item_line(&SortItem { description: words.clone(), tags: vec!["x".into(), "y".into()], ..item(7) });
    let cols: Vec<&str> = line.split('\t').collect();
    assert_eq!(cols[..4], ["7", "photo", "Item 7", "x, y"]);
    assert_eq!(cols.len(), 5);
    let desc = cols[4];
    assert!(desc.len() <= 240 && desc.len() > 200);
    assert!(words.starts_with(desc) && words.as_bytes()[desc.len()] == b' ', "ends on a whole word: {desc}");
}

#[test]
fn item_line_short_description_kept_and_whitespace_flattened() {
    assert_eq!(prompt::item_line(&SortItem { description: "Short.".into(), ..item(1) }).split('\t').nth(4), Some("Short."));
    let line = prompt::item_line(&SortItem { description: "one\ntwo\tthree".into(), ..named(1, "A\tB") });
    let cols: Vec<&str> = line.split('\t').collect();
    assert_eq!(cols.len(), 5);
    assert_eq!((cols[2], cols[4]), ("A B", "one two three"));
}

#[test]
fn item_line_video_includes_actions() {
    let actions: Vec<String> = (1..=30).map(|i| format!("0:{i:02} Something happens number {i}")).collect();
    let line = prompt::item_line(&SortItem { kind: ItemKind::Video, actions, ..item(9) });
    let cols: Vec<&str> = line.split('\t').collect();
    assert_eq!(cols.len(), 6);
    assert_eq!(cols[1], "video");
    assert!(cols[5].starts_with("0:01 Something happens number 1; 0:02 "));
    assert!(cols[5].len() <= 160);
    // Without actions the last column is there and empty.
    let bare = prompt::item_line(&SortItem { kind: ItemKind::Video, ..item(9) });
    assert_eq!(bare.split('\t').count(), 6);
    assert!(bare.ends_with('\t'));
}

#[test]
fn category_line_shows_locked_or_open() {
    assert_eq!(prompt::category_line(&CategoryInfo { description: "Pinnipeds".into(), ..users("Sea Lions", 12) }), "Sea Lions\t12\tlocked\tPinnipeds");
    assert_eq!(prompt::category_line(&CategoryInfo { description: "Meals".into(), ..category("Food", 3) }), "Food\t3\topen\tMeals");
}

#[test]
fn system_states_the_rules() {
    let s = prompt::SYSTEM;
    let lower = s.to_lowercase();
    for needle in ["existing categor", "never rename or merge", "one-item", "catch-all", "one general word", "40", "2-6 words", "description"] {
        assert!(lower.contains(needle), "the system prompt lacks '{needle}'");
    }
    for needle in [
        "what is in the media (its subject, place or event), never the file type",
        "Add at most 12 new categories in one reply.",
        "A new category name is one general word",
        "never two subjects joined",
        "The specific subject (a turtle, a seal, a dish) belongs in the friendly name",
    ] {
        assert!(s.contains(needle), "the system prompt lacks '{needle}'");
    }
    assert!(!s.contains("3 words"));
    assert_eq!(SORT_PROMPT_VERSION, "sort-v4");
}

#[test]
fn assign_user_lists_items_categories_and_feedback() {
    let text = prompt::assign_user(&[item(1), item(2)], &[category("Food", 2)], &["Unknown category 'X'".to_string()], MAX_NEW_CATEGORIES);
    for needle in ["Food\t2\topen", "1\tphoto\tItem 1", "2\tphoto\tItem 2", "Unknown category 'X'", "rejected"] {
        assert!(text.contains(needle), "{needle}");
    }
    assert!(!prompt::assign_user(&[item(1)], &[], &[], MAX_NEW_CATEGORIES).contains("rejected"));
}

/// I2: a tighter cap than the system prompt's 12 is stated in the chunk's own prompt.
#[test]
fn assign_user_states_a_tighter_new_category_cap() {
    let capped = prompt::assign_user(&[item(1)], &[category("Food", 2)], &[], 3);
    assert!(capped.contains("Add at most 3 new categories in this reply."), "{capped}");
    let default = prompt::assign_user(&[item(1)], &[category("Food", 2)], &[], MAX_NEW_CATEGORIES);
    assert!(!default.contains("new categories in this reply"), "{default}");
}

#[test]
fn propose_user_sends_name_and_tags_only() {
    let text = prompt::propose_user(&[SortItem { tags: vec!["boat".into(), "sea".into()], description: "SECRETDESC".into(), ..named(1, "Sails") }]);
    assert!(text.contains("Sails\tboat, sea"));
    assert!(!text.contains("SECRETDESC"));
    assert!(text.contains("broad groups named with one general word each"));
}

#[test]
fn sort_item_actions_are_read_from_their_json() {
    assert_eq!(SortItem::actions_from_json(r#"[{"t":"0:00","text":"Pan right"},{"t":"0:04","text":"Dives"}]"#), vec!["0:00 Pan right", "0:04 Dives"]);
    assert!(SortItem::actions_from_json("not json").is_empty());
}

// ---------------------------------------------------------------------------------------------
// SortSchemaTest
// ---------------------------------------------------------------------------------------------

fn rule<'a>(grammar: &'a str, name: &str) -> &'a str {
    let line = grammar.lines().find(|l| l.starts_with(&format!("{name} "))).unwrap_or_else(|| panic!("rule {name} missing in\n{grammar}"));
    line.split_once("::= ").unwrap().1
}

#[test]
fn assign_schema_requires_all_fields() {
    let s = schema::assign_json_schema();
    assert_eq!(s["additionalProperties"], json!(false));
    assert_eq!(s["required"], json!(["new_categories", "assignments"]));
    let nc = &s["properties"]["new_categories"]["items"];
    assert_eq!((&nc["additionalProperties"], &nc["required"]), (&json!(false), &json!(["name", "description"])));
    let a = &s["properties"]["assignments"]["items"];
    assert_eq!((&a["additionalProperties"], &a["required"]), (&json!(false), &json!(["id", "category", "name"])));
    assert_eq!(a["properties"]["id"]["type"], json!("integer"));
}

#[test]
fn both_schemas_cap_new_categories_at_twelve_and_propose_has_only_them() {
    for s in [schema::assign_json_schema(), schema::propose_json_schema()] {
        assert_eq!(s["properties"]["new_categories"]["maxItems"], json!(12));
    }
    let s = schema::propose_json_schema();
    assert_eq!(s["required"], json!(["new_categories"]));
    assert_eq!(s["properties"].as_object().unwrap().keys().collect::<Vec<_>>(), vec!["new_categories"]);
    assert_eq!(s["additionalProperties"], json!(false));
}

#[test]
fn assign_grammar_rule_names_and_shape() {
    let g = schema::assign_grammar(&[41, 7, 1234], &["Food".to_string()]);
    for name in ["root", "newcats", "newcat", "assigns", "tail", "category", "catname", "fname", "desc", "ws"] {
        rule(&g, name);
    }
    let at: Vec<usize> = [41, 7, 1234].iter().map(|id| g.find(&format!(r#""\"id\":" ws "{id}" tail"#)).expect("every id is a literal")).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "ids in chunk order: {at:?}");
    assert!(!g.contains(r#""\"id\":" ws "8""#));
    assert_eq!(rule(&g, "category"), r#""\"Food\"" | catname"#);
}

#[test]
fn assign_grammar_bounds_everything() {
    let g = schema::assign_grammar(&[1], &[]);
    assert_eq!(rule(&g, "ws"), r"[ \t\n]{0,4}");
    assert!(rule(&g, "catname").contains("{1,40}"));
    assert!(rule(&g, "fname").contains("{1,60}"));
    assert!(rule(&g, "desc").contains("{1,160}"));
    assert!(rule(&g, "newcats").contains("{0,11}"));
    assert_eq!(rule(&g, "category"), "catname");
    assert!(!g.contains(")*") && !g.contains("]*") && !g.contains("]+"), "no unbounded repetition");
}

#[test]
fn assign_grammar_escapes_existing_names_and_an_empty_chunk_is_an_empty_array() {
    let g = schema::assign_grammar(&[1], &["Say \"hi\" \\ there".to_string()]);
    assert!(rule(&g, "category").contains(r#"Say \\\"hi\\\" \\\\ there"#), "{}", rule(&g, "category"));
    assert_eq!(rule(&schema::assign_grammar(&[], &[]), "assigns"), r#""[" ws "]""#);
}

#[test]
fn propose_grammar_has_no_assignments() {
    let g = schema::propose_grammar();
    assert!(!g.contains("assignments"));
    assert!(rule(&g, "root").contains("new_categories"));
    assert!(rule(&g, "newcats").contains("{0,11}"));
    assert_eq!(rule(&g, "ws"), r"[ \t\n]{0,4}");
}

/// The small model's grammar allows only one-word new names: no spaces and no joining punctuation.
#[test]
fn grammars_allow_only_one_word_new_category_names() {
    let existing = ["Sunset Over Sea".to_string()];
    for g in [schema::assign_grammar(&[1], &existing), schema::propose_grammar()] {
        let catname = rule(&g, "catname");
        // The rule's character class is also a valid regex class.
        let class = Regex::new(r"\[[^\]]*\]").unwrap().find(catname).unwrap().as_str();
        let chars = Regex::new(&format!("^{class}+$")).unwrap();
        for good in ["Animals", "Mama's", "Café"] {
            assert!(chars.is_match(good), "{good} in {catname}");
        }
        for bad in ["Sea Turtles", "Turtles&Seals", "Sea-Life", "Food/Drink", "Water_Sports", "Surf+Sail"] {
            assert!(!chars.is_match(bad), "{bad} in {catname}");
        }
    }
    // An existing multi-word category is still a literal the model may pick.
    assert!(rule(&schema::assign_grammar(&[1], &existing), "category").contains("Sunset Over Sea"));
}
