//! [`ResourceId`]: a validated `namespace:path` name.

use std::fmt;
use std::str::FromStr;

use crate::error::{IdProblem, ResourceError};

/// Separates the namespace from the path in the text form of an id.
pub const NAMESPACE_SEPARATOR: char = ':';

/// Separates path segments, on every platform (a backslash is rejected).
pub const PATH_SEPARATOR: char = '/';

/// The Windows path separator. Rejected in ids so one id means one file everywhere.
const BACKSLASH: char = '\\';

/// Characters allowed in a namespace besides lowercase ASCII letters and digits.
const NAMESPACE_EXTRA_CHARS: &[char] = &['_'];

/// Characters allowed in a path segment besides lowercase ASCII letters and digits. The dot
/// is there for file extensions (`stone.png`).
const PATH_EXTRA_CHARS: &[char] = &['_', '.', '-'];

/// Separates a name from its extension. A segment may not end with it, and the part before the
/// first one (the stem) is checked against [`RESERVED_NAMES`].
const DOT: char = '.';

/// Windows device names. Windows opens `nul`, `nul.txt` and `nul.a.b` as the device, not as a
/// file, whatever the directory, so an id with such a stem (in a namespace or a segment) would
/// mean different things on different systems. Lowercase, because ids are lowercase.
const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A resource name `namespace:path`, validated once when it is parsed.
///
/// Namespace: `[a-z0-9_]+`. Path: segments of `[a-z0-9_.-]+` separated by `/`, with no empty
/// segment, no segment ending with a dot (which also rules out `.` and `..`), no Windows device
/// name (`nul`, `con`, `com1`, ...) as the part of a namespace or segment before its first dot,
/// no leading `/` and no backslash. A leading dot (`.hidden`) is allowed. Lowercase only, so a
/// pack behaves the same on case-insensitive and case-sensitive file systems. The extension is
/// part of the path; nothing is appended implicitly.
///
/// ```
/// use vox_resources::ResourceId;
///
/// let id = ResourceId::parse("base:textures/block/stone.png").unwrap();
/// assert_eq!(id.namespace(), "base");
/// assert_eq!(id.path(), "textures/block/stone.png");
/// assert_eq!(id.to_string(), "base:textures/block/stone.png");
/// assert!(ResourceId::parse("base:../secret").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceId {
    namespace: String,
    path: String,
}

impl ResourceId {
    /// Parses and validates `text`.
    ///
    /// # Errors
    /// [`ResourceError::InvalidId`] naming the first rule the text breaks.
    pub fn parse(text: &str) -> Result<Self, ResourceError> {
        match validate(text) {
            Ok((namespace, path)) => Ok(Self {
                namespace: namespace.to_owned(),
                path: path.to_owned(),
            }),
            Err(problem) => Err(ResourceError::InvalidId {
                text: text.to_owned(),
                problem,
            }),
        }
    }

    /// The part before the `:`.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The part after the `:`, with `/` between segments.
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{NAMESPACE_SEPARATOR}{}", self.namespace, self.path)
    }
}

impl FromStr for ResourceId {
    type Err = ResourceError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

fn is_namespace_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || NAMESPACE_EXTRA_CHARS.contains(&c)
}

fn is_path_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || PATH_EXTRA_CHARS.contains(&c)
}

/// Whether the part of `name` before its first dot is a Windows device name.
fn is_reserved(name: &str) -> bool {
    let stem = name.split(DOT).next().unwrap_or(name);
    RESERVED_NAMES.contains(&stem)
}

/// Checks `text` as a namespace alone, with the same rules as the namespace of a
/// [`ResourceId`]: `[a-z0-9_]+`, not a Windows device name. Mods use their id as a namespace,
/// so they share this rule instead of copying it.
///
/// ```
/// use vox_resources::{IdProblem, validate_namespace};
///
/// assert_eq!(validate_namespace("my_mod2"), Ok(()));
/// assert_eq!(validate_namespace("My"), Err(IdProblem::BadNamespaceChar('M')));
/// ```
///
/// # Errors
/// The first [`IdProblem`] the text breaks.
pub fn validate_namespace(text: &str) -> Result<(), IdProblem> {
    if text.is_empty() {
        return Err(IdProblem::EmptyNamespace);
    }
    if let Some(bad) = text.chars().find(|&c| !is_namespace_char(c)) {
        return Err(IdProblem::BadNamespaceChar(bad));
    }
    if is_reserved(text) {
        return Err(IdProblem::ReservedName);
    }
    Ok(())
}

/// Splits `text` into namespace and path, or names the first broken rule.
fn validate(text: &str) -> Result<(&str, &str), IdProblem> {
    let (namespace, path) = text
        .split_once(NAMESPACE_SEPARATOR)
        .ok_or(IdProblem::MissingColon)?;
    validate_namespace(namespace)?;
    if path.is_empty() {
        return Err(IdProblem::EmptyPath);
    }
    if path.contains(BACKSLASH) {
        return Err(IdProblem::Backslash);
    }
    if path.starts_with(PATH_SEPARATOR) {
        return Err(IdProblem::LeadingSlash);
    }
    for segment in path.split(PATH_SEPARATOR) {
        if segment.is_empty() {
            return Err(IdProblem::EmptySegment);
        }
        if segment.ends_with(DOT) {
            return Err(IdProblem::TrailingDot);
        }
        if let Some(bad) = segment.chars().find(|&c| !is_path_char(c)) {
            return Err(IdProblem::BadPathChar(bad));
        }
        if is_reserved(segment) {
            return Err(IdProblem::ReservedName);
        }
    }
    Ok((namespace, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(text: &str) -> IdProblem {
        match ResourceId::parse(text) {
            Err(ResourceError::InvalidId { problem, text: t }) => {
                assert_eq!(t, text);
                problem
            }
            other => panic!("`{text}` should be invalid, got {other:?}"),
        }
    }

    #[test]
    fn valid_ids_parse_and_round_trip() {
        for text in [
            "base:blocks.json",
            "core:shaders/chunk.vert",
            "my_mod2:a/b-c/d_e.f.png",
            "x:0",
            "x:a.b",
            "x:...a", // a leading dot is allowed
        ] {
            let id = ResourceId::parse(text).unwrap();
            assert_eq!(id.to_string(), text);
            assert_eq!(text.parse::<ResourceId>().unwrap(), id);
        }
    }

    #[test]
    fn accessors_split_at_the_first_colon_only() {
        let id = ResourceId::parse("base:textures/block/stone.png").unwrap();
        assert_eq!(id.namespace(), "base");
        assert_eq!(id.path(), "textures/block/stone.png");
        // A second colon is a path character error, not part of the namespace.
        assert_eq!(problem("a:b:c"), IdProblem::BadPathChar(':'));
    }

    #[test]
    fn uppercase_is_rejected_in_both_parts() {
        assert_eq!(problem("Base:a.png"), IdProblem::BadNamespaceChar('B'));
        assert_eq!(problem("base:Stone.png"), IdProblem::BadPathChar('S'));
    }

    #[test]
    fn segments_ending_with_a_dot_are_rejected() {
        assert_eq!(problem("base:../secret"), IdProblem::TrailingDot);
        assert_eq!(problem("base:a/../b"), IdProblem::TrailingDot);
        assert_eq!(problem("base:./a"), IdProblem::TrailingDot);
        assert_eq!(problem("base:a/..."), IdProblem::TrailingDot);
        assert_eq!(problem("base:.."), IdProblem::TrailingDot);
        assert_eq!(problem("base:a."), IdProblem::TrailingDot);
        assert_eq!(problem("base:a./b"), IdProblem::TrailingDot);
    }

    #[test]
    fn dots_inside_and_at_the_start_of_a_name_are_allowed() {
        for text in ["base:.hidden", "base:a..b", "base:a..b/c", "base:.a/.b"] {
            assert!(ResourceId::parse(text).is_ok(), "`{text}` should be valid");
        }
        // The trailing slash, not the double dot, is the problem here.
        assert_eq!(problem("base:a..b/"), IdProblem::EmptySegment);
    }

    #[test]
    fn windows_device_names_are_rejected_by_stem() {
        assert_eq!(problem("base:nul"), IdProblem::ReservedName);
        assert_eq!(problem("base:nul.txt"), IdProblem::ReservedName);
        assert_eq!(problem("base:dir/con.a.b"), IdProblem::ReservedName);
        assert_eq!(problem("base:com1"), IdProblem::ReservedName);
        assert_eq!(problem("base:lpt9.png"), IdProblem::ReservedName);
        assert_eq!(problem("nul:a"), IdProblem::ReservedName);
        assert_eq!(problem("con:a.txt"), IdProblem::ReservedName);
        assert_eq!(problem("aux:x"), IdProblem::ReservedName);
        // Only the stem counts: a device name after the first dot, or as part of a longer name,
        // is an ordinary file.
        for text in [
            "base:a.nul.txt",
            "base:nulx",
            "base:com0",
            "base:com10",
            "base:console",
        ] {
            assert!(ResourceId::parse(text).is_ok(), "`{text}` should be valid");
        }
    }

    #[test]
    fn empty_parts_are_rejected() {
        assert_eq!(problem("base:a//b"), IdProblem::EmptySegment);
        assert_eq!(problem("base:a/"), IdProblem::EmptySegment);
        assert_eq!(problem(":a"), IdProblem::EmptyNamespace);
        assert_eq!(problem("base:"), IdProblem::EmptyPath);
        assert_eq!(problem(""), IdProblem::MissingColon);
    }

    #[test]
    fn backslash_is_rejected() {
        assert_eq!(problem("base:a\\b"), IdProblem::Backslash);
        assert_eq!(problem("base:..\\secret"), IdProblem::Backslash);
    }

    #[test]
    fn namespace_alone_follows_the_namespace_rules() {
        assert_eq!(validate_namespace("my_mod2"), Ok(()));
        assert_eq!(validate_namespace(""), Err(IdProblem::EmptyNamespace));
        assert_eq!(
            validate_namespace("a-b"),
            Err(IdProblem::BadNamespaceChar('-'))
        );
        assert_eq!(validate_namespace("nul"), Err(IdProblem::ReservedName));
        // A colon is not a namespace character: this is a namespace, not an id.
        assert_eq!(
            validate_namespace("a:b"),
            Err(IdProblem::BadNamespaceChar(':'))
        );
    }

    #[test]
    fn missing_colon_is_rejected() {
        assert_eq!(problem("stone.png"), IdProblem::MissingColon);
    }

    #[test]
    fn leading_slash_is_rejected() {
        assert_eq!(problem("base:/etc/passwd"), IdProblem::LeadingSlash);
    }

    #[test]
    fn other_characters_are_rejected() {
        assert_eq!(problem("base:a b"), IdProblem::BadPathChar(' '));
        assert_eq!(problem("ba-se:a"), IdProblem::BadNamespaceChar('-'));
        assert_eq!(problem("base:a\0b"), IdProblem::BadPathChar('\0'));
        assert_eq!(problem("base:\u{e9}"), IdProblem::BadPathChar('\u{e9}'));
    }

    #[test]
    fn error_message_names_the_text_and_the_rule() {
        let message = ResourceId::parse("Base:a").unwrap_err().to_string();
        assert!(message.contains("Base:a"));
        assert!(message.contains("'B'"));
    }
}
