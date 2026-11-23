//! Error types of the resource file system, written by hand to keep the crate dependency-free.

use std::error::Error;
use std::fmt;
use std::io;

use crate::ResourceId;

/// Why a text is not a valid [`ResourceId`]. Carries the offending character where one exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdProblem {
    /// There is no `:` between namespace and path.
    MissingColon,
    /// The namespace is empty.
    EmptyNamespace,
    /// The namespace contains a character outside `[a-z0-9_]`.
    BadNamespaceChar(char),
    /// The path is empty.
    EmptyPath,
    /// The path starts with `/`.
    LeadingSlash,
    /// The path contains a backslash; only `/` separates segments, on every platform.
    Backslash,
    /// A path segment is empty (`a//b` or a trailing `/`).
    EmptySegment,
    /// A path segment ends with a dot. That rules out `.` and `..` (escaping the pack) and
    /// `a.`, which Windows opens as `a`, so two ids would name one file.
    TrailingDot,
    /// A namespace or path segment whose part before the first dot is a Windows device name
    /// (`nul`, `con`, `com1`, ...), which Windows opens as a device and not as a file.
    ReservedName,
    /// The path contains a character outside `[a-z0-9_.-]` (and `/`).
    BadPathChar(char),
}

impl fmt::Display for IdProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingColon => write!(f, "missing ':' between namespace and path"),
            Self::EmptyNamespace => write!(f, "empty namespace"),
            Self::BadNamespaceChar(c) => {
                write!(f, "character {c:?} is not allowed in a namespace")
            }
            Self::EmptyPath => write!(f, "empty path"),
            Self::LeadingSlash => write!(f, "path starts with '/'"),
            Self::Backslash => write!(f, "path contains a backslash, use '/'"),
            Self::EmptySegment => write!(f, "path has an empty segment"),
            Self::TrailingDot => write!(f, "path has a segment ending with a dot"),
            Self::ReservedName => write!(f, "name is a reserved Windows device name"),
            Self::BadPathChar(c) => write!(f, "character {c:?} is not allowed in a path"),
        }
    }
}

/// Everything that can go wrong when parsing an id or reading a resource.
#[derive(Debug)]
pub enum ResourceError {
    /// No pack in the stack has this resource.
    NotFound {
        /// The resource that was asked for.
        id: ResourceId,
    },
    /// A pack has the resource but reading it failed (permissions, a directory instead of a
    /// file, invalid UTF-8 for text, ...).
    Io {
        /// The resource that was asked for.
        id: ResourceId,
        /// The underlying failure.
        source: io::Error,
    },
    /// The text is not a valid resource id.
    InvalidId {
        /// The text as given.
        text: String,
        /// The first rule it breaks.
        problem: IdProblem,
    },
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { id } => write!(f, "resource `{id}` not found in any pack"),
            Self::Io { id, source } => write!(f, "cannot read resource `{id}`: {source}"),
            Self::InvalidId { text, problem } => {
                write!(f, "invalid resource id `{text}`: {problem}")
            }
        }
    }
}

impl Error for ResourceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::NotFound { .. } | Self::InvalidId { .. } => None,
        }
    }
}
