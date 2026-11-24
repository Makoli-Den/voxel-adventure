//! Error types of the mod system, written by hand like those of `vox-resources`.
//!
//! Three levels: [`ManifestProblem`] (one manifest is wrong), [`ModError`] (finding and reading
//! mods failed, with the path) and [`ResolveError`] (the set of mods does not fit together).

use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

use vox_resources::IdProblem;

use crate::manifest::{ApiVersion, ModId};

/// Why one manifest is not valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestProblem {
    /// The file is larger than the limit; it was not parsed.
    TooLarge {
        /// The limit in bytes.
        limit: usize,
    },
    /// Not JSON, not UTF-8, a missing or unknown key, or a value of the wrong type (including an
    /// unknown capability name). Holds the message of the JSON parser, which has the position.
    Json(String),
    /// The `format` number is not the one this game reads.
    UnsupportedFormat {
        /// The number in the file.
        found: u64,
    },
    /// The `id` breaks the namespace rules.
    BadId {
        /// The text as written.
        text: String,
        /// The first rule it breaks.
        problem: IdProblem,
    },
    /// The `id` is the name reserved for the engine.
    ReservedId {
        /// The reserved name.
        text: String,
    },
    /// The `id` differs from the name of the folder the manifest was read from.
    IdDiffersFromFolder {
        /// The id in the manifest.
        id: ModId,
        /// The folder name.
        folder: String,
    },
    /// The `version` is not a semver version.
    BadVersion {
        /// The text as written.
        text: String,
        /// What the parser says.
        message: String,
    },
    /// The `api` is not `MAJOR.MINOR`.
    BadApi {
        /// The text as written.
        text: String,
    },
    /// A key of `dependencies` is not a valid mod id.
    BadDependencyId {
        /// The key as written.
        text: String,
        /// What is wrong with it.
        problem: Box<ManifestProblem>,
    },
    /// The same mod id appears twice in `dependencies`. JSON allows it, and the parser would
    /// keep only the last, silently dropping a requirement.
    DuplicateDependency {
        /// The repeated key.
        text: String,
    },
    /// A value of `dependencies` is not a version requirement.
    BadRequirement {
        /// The dependency it belongs to.
        dependency: String,
        /// The text as written.
        text: String,
        /// What the parser says.
        message: String,
    },
    /// The same capability is listed twice.
    DuplicateCapability {
        /// The capability's name in the manifest.
        name: &'static str,
    },
    /// The `entry` is not a valid resource id.
    BadEntry {
        /// The text as written.
        text: String,
        /// What is wrong with it.
        message: String,
    },
    /// The `entry` is in the namespace of another mod; a mod runs only its own script.
    EntryOutsideMod {
        /// The entry as written.
        text: String,
        /// The mod's own id, the only namespace allowed.
        id: ModId,
    },
}

impl fmt::Display for ManifestProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { limit } => write!(f, "larger than the limit of {limit} bytes"),
            Self::Json(message) => write!(f, "{message}"),
            Self::UnsupportedFormat { found } => write!(
                f,
                "format {found} is not supported (this game reads format {})",
                crate::manifest::MANIFEST_FORMAT
            ),
            Self::BadId { text, problem } => write!(f, "invalid mod id `{text}`: {problem}"),
            Self::ReservedId { text } => {
                write!(f, "mod id `{text}` is reserved for the engine")
            }
            Self::IdDiffersFromFolder { id, folder } => {
                write!(f, "mod id `{id}` differs from its folder name `{folder}`")
            }
            Self::BadVersion { text, message } => {
                write!(f, "invalid version `{text}`: {message}")
            }
            Self::BadApi { text } => write!(
                f,
                "invalid api `{text}`: expected MAJOR.MINOR without leading zeros, like `{}`",
                crate::manifest::MOD_API_VERSION
            ),
            Self::DuplicateDependency { text } => {
                write!(f, "dependency `{text}` is listed more than once")
            }
            Self::DuplicateCapability { name } => {
                write!(f, "capability `{name}` is listed more than once")
            }
            Self::BadDependencyId { text, problem } => {
                write!(f, "invalid dependency name `{text}`: {problem}")
            }
            Self::BadRequirement {
                dependency,
                text,
                message,
            } => write!(
                f,
                "invalid version requirement `{text}` for dependency `{dependency}`: {message}"
            ),
            Self::BadEntry { text, message } => {
                write!(f, "invalid entry `{text}`: {message}")
            }
            Self::EntryOutsideMod { text, id } => write!(
                f,
                "entry `{text}` must be in the mod's own namespace `{id}`"
            ),
        }
    }
}

impl Error for ManifestProblem {}

/// Everything that can go wrong when finding and reading mods.
#[derive(Debug)]
pub enum ModError {
    /// The mods folder cannot be listed.
    ReadDir {
        /// The folder.
        dir: PathBuf,
        /// The underlying failure.
        source: io::Error,
    },
    /// A manifest exists but reading it failed.
    Io {
        /// The manifest file.
        path: PathBuf,
        /// The underlying failure.
        source: io::Error,
    },
    /// A mod folder or its manifest is a symbolic link or junction. Links are never followed,
    /// so a mods folder cannot make the game read outside it (decision 0014).
    Symlink {
        /// The link.
        path: PathBuf,
    },
    /// `mod.json` exists but is not a regular file (a folder, for example).
    NotAFile {
        /// The manifest path.
        path: PathBuf,
    },
    /// A manifest was read but is not valid.
    Manifest {
        /// The manifest file.
        path: PathBuf,
        /// What is wrong with it.
        problem: ManifestProblem,
    },
}

impl fmt::Display for ModError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadDir { dir, source } => {
                write!(f, "cannot list mods folder {}: {source}", dir.display())
            }
            Self::Io { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            Self::Symlink { path } => write!(
                f,
                "{} is a symbolic link or junction; links are not followed, copy the mod instead",
                path.display()
            ),
            Self::NotAFile { path } => {
                write!(f, "{} is not a regular file", path.display())
            }
            Self::Manifest { path, problem } => {
                write!(f, "{}: {problem}", path.display())
            }
        }
    }
}

/// `Display` already includes the underlying cause, so `source()` stays `None`: a caller that
/// prints the whole chain would otherwise show the cause twice.
impl Error for ModError {}

/// A reason the set of mods cannot be loaded. `resolve` returns all of them at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// Two mods have the same id (in different mods folders).
    DuplicateId {
        /// The shared id.
        id: ModId,
        /// How many mods carry it.
        count: usize,
    },
    /// A mod was written for an API this game does not provide: another major version, or a
    /// newer minor version than the game has.
    UnsupportedApi {
        /// The mod.
        id: ModId,
        /// The API it was written for.
        mod_api: ApiVersion,
        /// The API of the game.
        game_api: ApiVersion,
    },
    /// A mod needs a mod that is not there.
    MissingDependency {
        /// The mod that needs it.
        id: ModId,
        /// The missing mod.
        dependency: ModId,
        /// The requirement in normalized form (as `VersionReq` prints it, so `1.2` shows as `^1.2`).
        requirement: String,
    },
    /// A dependency is there, but its version is outside the requirement.
    VersionMismatch {
        /// The mod that needs it.
        id: ModId,
        /// The dependency.
        dependency: ModId,
        /// The requirement in normalized form (as `VersionReq` prints it, so `1.2` shows as `^1.2`).
        requirement: String,
        /// The version that is present.
        found: String,
    },
    /// Mods depend on each other in a circle.
    Cycle {
        /// One shortest circle, starting at its smallest id: each mod depends on the next, and
        /// the last on the first. A mod that depends on itself is a circle of one.
        path: Vec<ModId>,
    },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateId { id, count } => {
                write!(f, "{count} mods have the id `{id}`")
            }
            Self::UnsupportedApi {
                id,
                mod_api,
                game_api,
            } => write!(
                f,
                "mod `{id}` was written for API {mod_api}, but this game provides API {game_api} \
                 (the major version must match and the minor version must not be newer)"
            ),
            Self::MissingDependency {
                id,
                dependency,
                requirement,
            } => write!(
                f,
                "mod `{id}` needs mod `{dependency}` {requirement}, which is not installed"
            ),
            Self::VersionMismatch {
                id,
                dependency,
                requirement,
                found,
            } => write!(
                f,
                "mod `{id}` needs mod `{dependency}` {requirement}, but version {found} is installed"
            ),
            Self::Cycle { path } => {
                write!(f, "mods depend on each other in a circle: ")?;
                for id in path {
                    write!(f, "{id} -> ")?;
                }
                match path.first() {
                    Some(first) => write!(f, "{first}"),
                    None => Ok(()),
                }
            }
        }
    }
}

impl Error for ResolveError {}
