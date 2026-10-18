//! Canonical virtual paths and glob matching.
//!
//! The Source engine treats content paths as case-insensitive and accepts both
//! `/` and `\` as separators. Every lookup in this crate goes through
//! [`normalize`] so that mounts only ever have to compare one canonical form:
//! lowercase ASCII, forward slashes, no leading `./` or `/`, and no repeated or
//! trailing separators.

/// Converts a content path to its canonical form.
///
/// ```
/// use source_fs::path::normalize;
///
/// assert_eq!(normalize(r"Materials\Concrete//Wall01.VTF"), "materials/concrete/wall01.vtf");
/// assert_eq!(normalize("./maps/dm_lockdown.bsp"), "maps/dm_lockdown.bsp");
/// assert_eq!(normalize("/sound/"), "sound");
/// ```
#[must_use]
pub fn normalize(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for segment in path.split(['/', '\\']) {
        // Drop empty segments (from repeated or leading/trailing separators)
        // and current-directory markers. `..` is kept verbatim: content never
        // legitimately uses it, and resolving it could escape a mount root.
        if segment.is_empty() || segment == "." {
            continue;
        }
        if !out.is_empty() {
            out.push('/');
        }
        out.extend(segment.chars().map(|c| c.to_ascii_lowercase()));
    }
    out
}

/// Joins a directory, a file stem and an extension the way content references
/// are usually assembled (for example `$cdmaterials` + texture name + `.vmt`).
///
/// The result is normalized. An empty `dir` yields a root-level path.
#[must_use]
pub fn join(dir: &str, file: &str) -> String {
    if dir.is_empty() {
        normalize(file)
    } else {
        normalize(&format!("{dir}/{file}"))
    }
}

/// Returns `path` with `extension` appended unless it already ends with it.
///
/// Many Source references omit the extension (`$basetexture "concrete/wall01"`)
/// while some include it; this accepts both. Comparison is case-insensitive.
#[must_use]
pub fn with_extension(path: &str, extension: &str) -> String {
    let normalized = normalize(path);
    let suffix = format!(
        ".{}",
        extension.trim_start_matches('.').to_ascii_lowercase()
    );
    if normalized.ends_with(&suffix) {
        normalized
    } else {
        normalized + &suffix
    }
}

/// A compiled glob pattern over canonical paths.
///
/// Supported syntax:
/// * `*` matches any run of characters within one path segment;
/// * `?` matches exactly one character other than `/`;
/// * `**` matches any run of characters including `/`.
///
/// Patterns are normalized like paths, so matching is case-insensitive.
///
/// ```
/// use source_fs::path::Glob;
///
/// let maps = Glob::new("maps/*.bsp");
/// assert!(maps.matches("maps/dm_lockdown.bsp"));
/// assert!(!maps.matches("maps/graphs/dm_lockdown.ain"));
///
/// let all_vmts = Glob::new("materials/**.vmt");
/// assert!(all_vmts.matches("materials/concrete/wall01.vmt"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    tokens: Vec<GlobToken>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GlobToken {
    Literal(char),
    AnyChar,
    AnyInSegment,
    AnyAcrossSegments,
}

impl Glob {
    /// Compiles `pattern`.
    #[must_use]
    pub fn new(pattern: &str) -> Self {
        let normalized = normalize(pattern);
        let mut tokens = Vec::with_capacity(normalized.len());
        let mut chars = normalized.chars().peekable();
        while let Some(c) = chars.next() {
            let token = match c {
                '*' if chars.peek() == Some(&'*') => {
                    chars.next();
                    GlobToken::AnyAcrossSegments
                }
                '*' => GlobToken::AnyInSegment,
                '?' => GlobToken::AnyChar,
                other => GlobToken::Literal(other),
            };
            tokens.push(token);
        }
        Self { tokens }
    }

    /// Returns the literal prefix before the first wildcard.
    ///
    /// Mounts that keep a sorted index can use it to skip unrelated entries.
    #[must_use]
    pub fn literal_prefix(&self) -> String {
        self.tokens
            .iter()
            .map_while(|token| match token {
                GlobToken::Literal(c) => Some(*c),
                _ => None,
            })
            .collect()
    }

    /// Tests a canonical path (as produced by [`normalize`]) against the pattern.
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        let text: Vec<char> = path.chars().collect();
        Self::match_from(&self.tokens, &text)
    }

    /// Dynamic-programming matcher over (token, text position) pairs. Runs in
    /// `O(pattern * text)` time with no backtracking blow-up, which is plenty
    /// for content paths (well under 260 characters).
    fn match_from(tokens: &[GlobToken], text: &[char]) -> bool {
        // dp[j] == true  <=>  tokens[..i] matches text[..j]
        let mut dp = vec![false; text.len() + 1];
        dp[0] = true;
        for token in tokens {
            let mut next = vec![false; text.len() + 1];
            match token {
                GlobToken::Literal(c) => {
                    for j in 0..text.len() {
                        next[j + 1] = dp[j] && text[j] == *c;
                    }
                }
                GlobToken::AnyChar => {
                    for j in 0..text.len() {
                        next[j + 1] = dp[j] && text[j] != '/';
                    }
                }
                GlobToken::AnyInSegment => {
                    next[0] = dp[0];
                    for j in 0..text.len() {
                        next[j + 1] = dp[j + 1] || (next[j] && text[j] != '/');
                    }
                }
                GlobToken::AnyAcrossSegments => {
                    next[0] = dp[0];
                    for j in 0..text.len() {
                        next[j + 1] = dp[j + 1] || next[j];
                    }
                }
            }
            dp = next;
        }
        dp[text.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn normalize_handles_separators_case_and_dots() {
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("."), "");
        assert_eq!(normalize("A\\B/./C"), "a/b/c");
        assert_eq!(normalize("//x//y//"), "x/y");
        assert_eq!(normalize("../escape"), "../escape");
    }

    #[test]
    fn join_and_extension_helpers() {
        assert_eq!(
            join("Models/Props", "Barrel.MDL"),
            "models/props/barrel.mdl"
        );
        assert_eq!(join("", "x.vmt"), "x.vmt");
        assert_eq!(with_extension("Concrete/Wall", "vmt"), "concrete/wall.vmt");
        assert_eq!(
            with_extension("concrete/wall.VMT", ".vmt"),
            "concrete/wall.vmt"
        );
    }

    #[test]
    fn glob_segment_wildcards_do_not_cross_separators() {
        let glob = Glob::new("maps/*.bsp");
        assert!(glob.matches("maps/a.bsp"));
        assert!(glob.matches("maps/.bsp"));
        assert!(!glob.matches("maps/sub/a.bsp"));
        assert!(!glob.matches("maps/a.bsp.bak"));
    }

    #[test]
    fn glob_double_star_crosses_separators() {
        let glob = Glob::new("**/*.vtf");
        assert!(glob.matches("materials/a/b.vtf"));
        assert!(
            !glob.matches("b.vtf"),
            "the literal `/` after ** must still match"
        );
        assert!(Glob::new("**").matches("anything/at/all"));
    }

    #[test]
    fn glob_question_mark_and_case() {
        let glob = Glob::new("Sound/?.WAV");
        assert!(glob.matches("sound/a.wav"));
        assert!(!glob.matches("sound/ab.wav"));
        assert_eq!(Glob::new("maps/dm_*").literal_prefix(), "maps/dm_");
    }

    proptest! {
        #[test]
        fn normalize_is_idempotent(path in ".{0,64}") {
            let once = normalize(&path);
            prop_assert_eq!(normalize(&once), once.clone());
        }

        #[test]
        fn literal_globs_match_only_themselves(a in "[a-z/]{0,16}", b in "[a-z/]{0,16}") {
            let na = normalize(&a);
            let nb = normalize(&b);
            prop_assert_eq!(Glob::new(&na).matches(&nb), na == nb);
        }

        #[test]
        fn double_star_matches_everything(path in "[a-z0-9_/.]{0,32}") {
            prop_assert!(Glob::new("**").matches(&normalize(&path)));
        }
    }
}
