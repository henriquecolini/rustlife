//! Parser for Valve's text KeyValues format.
//!
//! KeyValues is the nested `"key" "value"` / `"key" { ... }` syntax used by
//! `gameinfo.txt`, Steam's `.vdf`/`.acf` manifests, materials (`.vmt`),
//! soundscripts and many other configuration files. This module implements the
//! text dialect only (the binary variant is not used by any content rustlife
//! reads).
//!
//! # Dialect notes
//!
//! * Keys and values may be quoted or bare. Bare tokens end at whitespace,
//!   `"`, `{` or `}`.
//! * `//` starts a comment that runs to the end of the line.
//! * Duplicate keys are legal and preserved in order; lookups are
//!   ASCII-case-insensitive and return the first match, as in the engine.
//! * A key or value may be followed by a platform conditional such as
//!   `[$WIN32]` or `[!$X360]`. Pairs whose conditional evaluates to false are
//!   dropped; see [`Conditions`].
//! * Escape sequences (`\n`, `\t`, `\\`, `\"`) are **off by default**, because
//!   the engine also leaves them off for most files and content paths are
//!   frequently written with backslashes (`"materials\concrete\wall"`).
//! * `#base` and `#include` directives are recorded in
//!   [`Document::directives`] rather than resolved, because resolving them
//!   needs a filesystem; callers that care (such as resource files) can merge
//!   the referenced documents themselves.
//! * Like the engine, the parser is lenient about blocks left unclosed at end
//!   of file, which is a common defect in community content. Structurally
//!   meaningless input (a `}` with no open block, a `{` with no key, an
//!   unterminated quoted string) is reported as a [`KvError`].

use std::fmt;

/// A parsed KeyValues file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    /// The top-level pairs. Most files have exactly one root pair (for example
    /// `"GameInfo" { ... }` or `"LightmappedGeneric" { ... }`), but some, such
    /// as soundscripts, have many.
    pub root: Block,
    /// `#base` / `#include` directives in the order they appeared.
    pub directives: Vec<Directive>,
}

/// A file-inclusion directive found at the top level of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Directive {
    /// `#base "path"`: keys from the referenced file are merged in as defaults.
    Base(String),
    /// `#include "path"`: the referenced file's pairs are appended.
    Include(String),
}

/// An ordered list of key/value pairs, the body of a `{ ... }` block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Block {
    pairs: Vec<Pair>,
}

/// One `key value` or `key { ... }` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    /// The key exactly as written (case preserved).
    pub key: String,
    /// The associated string or nested block.
    pub value: Value,
}

/// The value half of a [`Pair`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A scalar string value.
    String(String),
    /// A nested block.
    Block(Block),
}

impl Value {
    /// Returns the string if this is a scalar value.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            Self::Block(_) => None,
        }
    }

    /// Returns the block if this is a nested block.
    #[must_use]
    pub fn as_block(&self) -> Option<&Block> {
        match self {
            Self::Block(b) => Some(b),
            Self::String(_) => None,
        }
    }
}

impl Block {
    /// Creates a block from pairs, preserving order.
    #[must_use]
    pub fn from_pairs(pairs: Vec<Pair>) -> Self {
        Self { pairs }
    }

    /// Iterates over every pair in order.
    pub fn iter(&self) -> impl Iterator<Item = &Pair> {
        self.pairs.iter()
    }

    /// Number of pairs in the block.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    /// Whether the block has no pairs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// Returns the value of the first pair whose key matches `key`
    /// (ASCII-case-insensitively).
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.matching(key).next().map(|pair| &pair.value)
    }

    /// Returns every value whose key matches `key`, in order.
    pub fn get_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
        self.pairs
            .iter()
            .filter(move |pair| pair.key.eq_ignore_ascii_case(key))
            .map(|pair| &pair.value)
    }

    fn matching<'s>(&'s self, key: &str) -> impl Iterator<Item = &'s Pair> {
        self.pairs
            .iter()
            .filter(move |pair| pair.key.eq_ignore_ascii_case(key))
    }

    /// Returns the first scalar value for `key`.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.matching(key).find_map(|pair| pair.value.as_str())
    }

    /// Returns the first nested block for `key`.
    #[must_use]
    pub fn get_block(&self, key: &str) -> Option<&Block> {
        self.matching(key).find_map(|pair| pair.value.as_block())
    }

    /// Follows a path of block keys, e.g. `["GameInfo", "FileSystem"]`.
    #[must_use]
    pub fn get_path(&self, path: &[&str]) -> Option<&Block> {
        path.iter()
            .try_fold(self, |block, key| block.get_block(key))
    }
}

/// Error produced when a document cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("keyvalues syntax error at line {line}, column {column}: {kind}")]
pub struct KvError {
    /// 1-based line of the offending token.
    pub line: usize,
    /// 1-based column (in characters) of the offending token.
    pub column: usize,
    /// What went wrong.
    pub kind: KvErrorKind,
}

/// The category of a [`KvError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KvErrorKind {
    /// A `}` appeared with no open block.
    UnexpectedClose,
    /// A `{` appeared where a key was expected.
    UnexpectedOpen,
    /// A quoted string was never closed.
    UnterminatedString,
    /// A `#base`/`#include` directive was not followed by a path.
    MissingDirectivePath,
}

impl fmt::Display for KvErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnexpectedClose => "unexpected '}' with no open block",
            Self::UnexpectedOpen => "unexpected '{' where a key was expected",
            Self::UnterminatedString => "unterminated quoted string",
            Self::MissingDirectivePath => "directive is missing its path",
        })
    }
}

/// The set of platform symbols considered defined when evaluating
/// conditionals such as `[$WIN32]`.
///
/// The default emulates the reference PC build of the engine (`$WIN32`,
/// `$WINDOWS`, `$PC`) regardless of the host OS, so that content resolves
/// identically on every platform rustlife runs on. In the engine `$WIN32`
/// means "desktop PC", not literally Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conditions {
    defined: Vec<String>,
}

impl Default for Conditions {
    fn default() -> Self {
        Self::new(["WIN32", "WINDOWS", "PC"])
    }
}

impl Conditions {
    /// Creates a condition set with the given symbols defined (without `$`).
    pub fn new<I, S>(symbols: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            defined: symbols.into_iter().map(Into::into).collect(),
        }
    }

    /// Evaluates a conditional expression (the text between `[` and `]`).
    ///
    /// Grammar: `expr := and ('||' and)*`, `and := term ('&&' term)*`,
    /// `term := '!'? '$' IDENT`. An empty expression is true. Terms that do not
    /// follow the grammar evaluate to false, so malformed conditionals drop
    /// their pair rather than silently enabling it.
    #[must_use]
    pub fn evaluate(&self, expression: &str) -> bool {
        let expression = expression.trim();
        if expression.is_empty() {
            return true;
        }
        expression.split("||").any(|conjunction| {
            conjunction
                .split("&&")
                .all(|term| self.evaluate_term(term.trim()))
        })
    }

    fn evaluate_term(&self, term: &str) -> bool {
        let (negated, rest) = match term.strip_prefix('!') {
            Some(rest) => (true, rest.trim_start()),
            None => (false, term),
        };
        let Some(symbol) = rest.strip_prefix('$') else {
            return false;
        };
        let defined = self.defined.iter().any(|d| d.eq_ignore_ascii_case(symbol));
        defined != negated
    }
}

/// Options controlling how a document is parsed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseOptions {
    /// Interpret `\n`, `\t`, `\\` and `\"` inside quoted strings.
    pub escapes: bool,
    /// Symbols used to evaluate `[$...]` conditionals.
    pub conditions: Conditions,
}

/// Parses `text` with default [`ParseOptions`].
///
/// ```
/// use source_fs::keyvalues;
///
/// let doc = keyvalues::parse(r#"
///     "LightmappedGeneric"
///     {
///         "$basetexture" "concrete/wall01"   // diffuse
///         $surfaceprop concrete
///         "$envmap" "env_cubemap" [$X360]
///     }
/// "#).unwrap();
///
/// let material = doc.root.get_block("lightmappedgeneric").unwrap();
/// assert_eq!(material.get_str("$basetexture"), Some("concrete/wall01"));
/// assert_eq!(material.get_str("$SURFACEPROP"), Some("concrete"));
/// assert_eq!(material.get_str("$envmap"), None); // console-only pair dropped
/// ```
///
/// # Errors
///
/// Returns a [`KvError`] for structurally invalid input; see the module docs.
pub fn parse(text: &str) -> Result<Document, KvError> {
    parse_with(text, &ParseOptions::default())
}

/// Parses `text` with explicit options.
///
/// # Errors
///
/// Returns a [`KvError`] for structurally invalid input; see the module docs.
pub fn parse_with(text: &str, options: &ParseOptions) -> Result<Document, KvError> {
    Parser {
        lexer: Lexer::new(text, options.escapes),
        options,
    }
    .parse_document()
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    /// A quoted or bare string.
    String {
        text: String,
        quoted: bool,
    },
    /// `[...]` platform conditional (brackets stripped).
    Conditional(String),
    Open,
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    kind: TokenKind,
    line: usize,
    column: usize,
    /// Whether a newline separated this token from the previous one. Needed
    /// because a conditional only applies to the pair on its own line.
    after_newline: bool,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    escapes: bool,
    line: usize,
    column: usize,
    peeked: Option<Result<Option<Token>, KvError>>,
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str, escapes: bool) -> Self {
        // Skip a UTF-8 byte-order mark, which some editors write.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        Self {
            chars: text.chars().peekable(),
            escapes,
            line: 1,
            column: 1,
            peeked: None,
        }
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        if c == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(c)
    }

    fn peek(&mut self) -> Result<Option<&Token>, KvError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lex());
        }
        match self.peeked.as_ref() {
            Some(Ok(token)) => Ok(token.as_ref()),
            Some(Err(error)) => Err(error.clone()),
            None => Ok(None),
        }
    }

    fn next_token(&mut self) -> Result<Option<Token>, KvError> {
        match self.peeked.take() {
            Some(result) => result,
            None => self.lex(),
        }
    }

    /// Skips whitespace and comments; returns whether a newline was crossed.
    fn skip_trivia(&mut self) -> bool {
        let mut crossed_newline = false;
        loop {
            match self.chars.peek() {
                Some('\n') => {
                    crossed_newline = true;
                    self.bump();
                }
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') => {
                    let mut lookahead = self.chars.clone();
                    lookahead.next();
                    if lookahead.peek() != Some(&'/') {
                        return crossed_newline;
                    }
                    while let Some(c) = self.chars.peek() {
                        if *c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => return crossed_newline,
            }
        }
    }

    fn lex(&mut self) -> Result<Option<Token>, KvError> {
        let after_newline = self.skip_trivia();
        let (line, column) = (self.line, self.column);
        let Some(&c) = self.chars.peek() else {
            return Ok(None);
        };
        let kind = match c {
            '{' => {
                self.bump();
                TokenKind::Open
            }
            '}' => {
                self.bump();
                TokenKind::Close
            }
            '"' => {
                self.bump();
                TokenKind::String {
                    text: self.lex_quoted(line, column)?,
                    quoted: true,
                }
            }
            '[' => {
                self.bump();
                let mut text = String::new();
                while let Some(c) = self.bump() {
                    if c == ']' {
                        break;
                    }
                    text.push(c);
                }
                TokenKind::Conditional(text)
            }
            _ => {
                let mut text = String::new();
                while let Some(&c) = self.chars.peek() {
                    if c.is_whitespace() || matches!(c, '"' | '{' | '}') {
                        break;
                    }
                    text.push(c);
                    self.bump();
                }
                TokenKind::String {
                    text,
                    quoted: false,
                }
            }
        };
        Ok(Some(Token {
            kind,
            line,
            column,
            after_newline,
        }))
    }

    fn lex_quoted(&mut self, line: usize, column: usize) -> Result<String, KvError> {
        let mut text = String::new();
        loop {
            match self.bump() {
                None => {
                    return Err(KvError {
                        line,
                        column,
                        kind: KvErrorKind::UnterminatedString,
                    });
                }
                Some('"') => return Ok(text),
                Some('\\') if self.escapes => match self.bump() {
                    Some('n') => text.push('\n'),
                    Some('t') => text.push('\t'),
                    Some('\\') => text.push('\\'),
                    Some('"') => text.push('"'),
                    // Unknown escapes are kept verbatim, matching the engine.
                    Some(other) => {
                        text.push('\\');
                        text.push(other);
                    }
                    None => {
                        return Err(KvError {
                            line,
                            column,
                            kind: KvErrorKind::UnterminatedString,
                        });
                    }
                },
                Some(c) => text.push(c),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser<'a, 'o> {
    lexer: Lexer<'a>,
    options: &'o ParseOptions,
}

impl Parser<'_, '_> {
    fn parse_document(mut self) -> Result<Document, KvError> {
        let mut document = Document::default();
        let mut pairs = Vec::new();
        while let Some(token) = self.lexer.next_token()? {
            match token.kind {
                TokenKind::String {
                    ref text,
                    quoted: false,
                } if text.eq_ignore_ascii_case("#base")
                    || text.eq_ignore_ascii_case("#include") =>
                {
                    let path = self.expect_directive_path(&token)?;
                    document
                        .directives
                        .push(if text.eq_ignore_ascii_case("#base") {
                            Directive::Base(path)
                        } else {
                            Directive::Include(path)
                        });
                }
                _ => {
                    if let Some(pair) = self.parse_pair(token)? {
                        pairs.push(pair);
                    }
                }
            }
        }
        document.root = Block::from_pairs(pairs);
        Ok(document)
    }

    fn expect_directive_path(&mut self, directive: &Token) -> Result<String, KvError> {
        match self.lexer.next_token()? {
            Some(Token {
                kind: TokenKind::String { text, .. },
                ..
            }) => Ok(text),
            _ => Err(KvError {
                line: directive.line,
                column: directive.column,
                kind: KvErrorKind::MissingDirectivePath,
            }),
        }
    }

    /// Parses one pair whose key token has already been consumed. Returns
    /// `Ok(None)` when the pair is dropped by a conditional or truncated by EOF.
    fn parse_pair(&mut self, key_token: Token) -> Result<Option<Pair>, KvError> {
        let key = match key_token.kind {
            TokenKind::String { text, .. } => text,
            TokenKind::Close => {
                return Err(KvError {
                    line: key_token.line,
                    column: key_token.column,
                    kind: KvErrorKind::UnexpectedClose,
                });
            }
            TokenKind::Open => {
                return Err(KvError {
                    line: key_token.line,
                    column: key_token.column,
                    kind: KvErrorKind::UnexpectedOpen,
                });
            }
            // A stray conditional with nothing to attach to is ignored.
            TokenKind::Conditional(_) => return Ok(None),
        };

        let mut enabled = true;
        let mut value_token = self.lexer.next_token()?;
        // `"key" [$COND] { ... }`: conditional between key and block.
        if let Some(Token {
            kind: TokenKind::Conditional(ref expression),
            ..
        }) = value_token
        {
            enabled &= self.options.conditions.evaluate(expression);
            value_token = self.lexer.next_token()?;
        }

        let value = match value_token {
            // Truncated file: a dangling key at EOF is dropped.
            None => return Ok(None),
            Some(Token {
                kind: TokenKind::String { text, .. },
                ..
            }) => Value::String(text),
            Some(Token {
                kind: TokenKind::Open,
                ..
            }) => Value::Block(self.parse_block()?),
            Some(Token {
                kind: TokenKind::Close,
                line,
                column,
                ..
            }) => {
                return Err(KvError {
                    line,
                    column,
                    kind: KvErrorKind::UnexpectedClose,
                });
            }
            Some(Token {
                kind: TokenKind::Conditional(_),
                line,
                column,
                ..
            }) => {
                return Err(KvError {
                    line,
                    column,
                    kind: KvErrorKind::UnexpectedOpen,
                });
            }
        };

        // `"key" "value" [$COND]`: trailing conditional on the same line.
        if let Some(Token {
            kind: TokenKind::Conditional(_),
            after_newline: false,
            ..
        }) = self.lexer.peek()?
            && let Some(Token {
                kind: TokenKind::Conditional(expression),
                ..
            }) = self.lexer.next_token()?
        {
            enabled &= self.options.conditions.evaluate(&expression);
        }

        Ok(enabled.then_some(Pair { key, value }))
    }

    /// Parses pairs until the matching `}` (already past the `{`). A missing
    /// `}` at end of file closes the block implicitly.
    fn parse_block(&mut self) -> Result<Block, KvError> {
        let mut pairs = Vec::new();
        loop {
            match self.lexer.next_token()? {
                None
                | Some(Token {
                    kind: TokenKind::Close,
                    ..
                }) => break,
                Some(token) => {
                    if let Some(pair) = self.parse_pair(token)? {
                        pairs.push(pair);
                    }
                }
            }
        }
        Ok(Block::from_pairs(pairs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::fmt::Write as _;

    fn root(text: &str) -> Block {
        parse(text).unwrap().root
    }

    #[test]
    fn parses_nested_blocks_and_mixed_quoting() {
        let block = root(
            r#"
            "GameInfo"
            {
                game "Half-Life 2 DM"
                FileSystem
                {
                    SteamAppId 320
                    SearchPaths { game+mod "hl2mp/custom/*" }
                }
            }"#,
        );
        let info = block.get_block("GameInfo").unwrap();
        assert_eq!(info.get_str("game"), Some("Half-Life 2 DM"));
        let fs = block.get_path(&["gameinfo", "filesystem"]).unwrap();
        assert_eq!(fs.get_str("steamappid"), Some("320"));
        assert_eq!(
            fs.get_block("SearchPaths").unwrap().get_str("game+mod"),
            Some("hl2mp/custom/*")
        );
    }

    #[test]
    fn preserves_duplicate_keys_in_order() {
        let block = root(r#""game" "a" "game" "b" "other" "c" "GAME" "d""#);
        let values: Vec<_> = block.get_all("game").filter_map(Value::as_str).collect();
        assert_eq!(values, ["a", "b", "d"]);
        assert_eq!(block.len(), 4);
    }

    #[test]
    fn skips_comments_including_trailing_and_full_line() {
        let block = root("// header\n\"a\" \"1\" // trailing\n// \"b\" \"2\"\n\"c\" \"3\"");
        assert_eq!(block.get_str("a"), Some("1"));
        assert_eq!(block.get_str("b"), None);
        assert_eq!(block.get_str("c"), Some("3"));
    }

    #[test]
    fn single_slash_is_part_of_bare_token() {
        assert_eq!(root("path a/b").get_str("path"), Some("a/b"));
    }

    #[test]
    fn backslashes_are_literal_without_escapes() {
        let block = root(r#""$basetexture" "concrete\wall\n01""#);
        assert_eq!(block.get_str("$basetexture"), Some(r"concrete\wall\n01"));
    }

    #[test]
    fn escapes_are_interpreted_when_enabled() {
        let options = ParseOptions {
            escapes: true,
            ..ParseOptions::default()
        };
        let doc = parse_with(r#""k" "a\"b\n\\c\q""#, &options).unwrap();
        assert_eq!(doc.root.get_str("k"), Some("a\"b\n\\c\\q"));
    }

    #[test]
    fn conditionals_filter_pairs() {
        let block = root(
            "\"pc\" \"1\" [$WIN32]\n\
             \"console\" \"1\" [$X360]\n\
             \"not_console\" \"1\" [!$X360]\n\
             \"either\" \"1\" [$X360||$PC]\n\
             \"both\" \"1\" [$WIN32&&$X360]\n\
             \"blk\" [$X360] { \"x\" \"y\" }\n",
        );
        assert!(block.get("pc").is_some());
        assert!(block.get("console").is_none());
        assert!(block.get("not_console").is_some());
        assert!(block.get("either").is_some());
        assert!(block.get("both").is_none());
        assert!(block.get("blk").is_none());
    }

    #[test]
    fn conditional_on_next_line_does_not_attach_to_previous_pair() {
        // A conditional on its own line has no pair to qualify and is ignored,
        // rather than retroactively removing the pair above it.
        let block = root("\"a\" \"1\"\n[$X360]\n\"b\" \"2\"");
        assert_eq!(block.get_str("a"), Some("1"));
        assert_eq!(block.get_str("b"), Some("2"));
    }

    #[test]
    fn custom_conditions() {
        let conditions = Conditions::new(["LINUX"]);
        assert!(conditions.evaluate("$linux"));
        assert!(!conditions.evaluate("$WIN32"));
        assert!(conditions.evaluate(""));
        assert!(!conditions.evaluate("garbage"));
    }

    #[test]
    fn records_directives() {
        let doc = parse("#base \"base.res\"\n#include inc.res\n\"k\" \"v\"").unwrap();
        assert_eq!(
            doc.directives,
            [
                Directive::Base("base.res".into()),
                Directive::Include("inc.res".into())
            ]
        );
        assert_eq!(doc.root.get_str("k"), Some("v"));
        assert!(parse("#base").is_err());
    }

    #[test]
    fn tolerates_unclosed_blocks_and_dangling_keys() {
        let block = root("\"a\" { \"b\" { \"c\" \"d\"");
        assert_eq!(block.get_path(&["a", "b"]).unwrap().get_str("c"), Some("d"));
        assert!(root("\"a\" \"1\" \"dangling\"").get("dangling").is_none());
    }

    #[test]
    fn reports_structural_errors_with_positions() {
        let error = parse("\"a\" \"1\"\n}").unwrap_err();
        assert_eq!(
            (error.line, error.column, error.kind),
            (2, 1, KvErrorKind::UnexpectedClose)
        );
        assert_eq!(parse("{ }").unwrap_err().kind, KvErrorKind::UnexpectedOpen);
        assert_eq!(
            parse("\"a\" \"open").unwrap_err().kind,
            KvErrorKind::UnterminatedString
        );
    }

    #[test]
    fn strips_byte_order_mark_and_handles_empty_input() {
        assert_eq!(root("\u{feff}\"k\" \"v\"").get_str("k"), Some("v"));
        assert!(root("").is_empty());
        assert!(root("   // only a comment").is_empty());
    }

    #[test]
    fn empty_and_brace_adjacent_tokens() {
        let block = root(r#""k" "" "b"{"x""y"}"#);
        assert_eq!(block.get_str("k"), Some(""));
        assert_eq!(block.get_block("b").unwrap().get_str("x"), Some("y"));
    }

    proptest! {
        #[test]
        fn never_panics_on_arbitrary_input(text in "\\PC{0,256}") {
            let _ = parse(&text);
            let _ = parse_with(&text, &ParseOptions { escapes: true, ..ParseOptions::default() });
        }

        #[test]
        fn never_panics_on_keyvalues_like_input(text in r#"[ "{}\[\]$!/a-z0-9\n]{0,128}"#) {
            let _ = parse(&text);
        }

        #[test]
        fn round_trips_flat_quoted_pairs(
            pairs in proptest::collection::vec(("[a-z0-9_]{1,8}", "[a-z0-9 _./]{0,12}"), 0..16)
        ) {
            let text = pairs.iter().fold(String::new(), |mut text, (k, v)| {
                let _ = writeln!(text, "\"{k}\" \"{v}\"");
                text
            });
            let block = parse(&text).unwrap().root;
            prop_assert_eq!(block.len(), pairs.len());
            for (pair, (k, v)) in block.iter().zip(&pairs) {
                prop_assert_eq!(&pair.key, k);
                prop_assert_eq!(pair.value.as_str(), Some(v.as_str()));
            }
        }
    }
}
