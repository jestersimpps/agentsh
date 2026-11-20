pub fn closest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let lower = word.to_lowercase();
    let max_edits = (word.chars().count() / 4).max(1);
    candidates
        .into_iter()
        .filter(|c| *c != word)
        .filter_map(|c| {
            let candidate = c.to_lowercase();
            let similarity = strsim::jaro_winkler(&lower, &candidate);
            let edits = strsim::damerau_levenshtein(&lower, &candidate);
            (similarity >= 0.84 || edits <= max_edits).then_some((c, edits, similarity))
        })
        .min_by(|a, b| a.1.cmp(&b.1).then(b.2.total_cmp(&a.2)))
        .map(|(c, _, _)| c)
}

const BASH_EQUIVALENTS: &[(&str, &str)] = &[
    ("cat", "read \"file\""),
    ("less", "read \"file\""),
    ("more", "read \"file\""),
    ("head", "read \"file\" to:20"),
    ("tail", "read \"file\" tail:20"),
    ("echo", "print \"text\""),
    ("printf", "print \"text\""),
    ("touch", "write \"file\" \"\""),
    ("sed", "edit \"file\" old:\"a\" new:\"b\"  (or replace \"a\" \"b\" in:\"src\")"),
    ("awk", "where / pick / sum on structured rows"),
    ("cut", "pick .field"),
    ("wc", "count"),
    ("rmdir", "rm \"dir\" recursive:true"),
    ("xargs", "for $item in ... { ... }"),
    ("test", "exists(\"path\")"),
    ("jq", "json, then where / pick"),
    ("rg", "grep \"pattern\" in:\"src\""),
    ("ripgrep", "grep \"pattern\" in:\"src\""),
    ("tree", "find \".\" depth:3"),
    ("du", "find \".\" | sum .size"),
    ("tee", "print (passes its input through)"),
    ("export", "let name = value"),
    ("set", "let name = value"),
    ("source", "let name = value"),
    ("sleep", "exec \"sleep\" \"1\""),
];

pub fn bash_equivalent(name: &str) -> Option<&'static str> {
    BASH_EQUIVALENTS.iter().find(|(bash, _)| *bash == name).map(|(_, ours)| *ours)
}
