//! The error type of the content loader, written by hand like the other crates' errors.

use std::error::Error;
use std::fmt;

use vox_resources::{ResourceError, ResourceId};
use vox_world::{Face, RegistryError};

use crate::textures::MAX_LAYERS;

/// Why content could not be loaded. Every variant names the file, and the block where one is
/// involved, so the message alone says what to fix.
#[derive(Debug)]
pub enum ContentError {
    /// The file could not be read (not found in any pack, or an I/O failure).
    Read(ResourceError),
    /// The file is not valid JSON of the expected shape; the source carries line and column.
    Parse {
        /// The blocks file.
        file: ResourceId,
        /// What serde reported, including unknown keys.
        source: serde_json::Error,
    },
    /// The `format` number is not one this game reads.
    UnsupportedFormat {
        /// The blocks file.
        file: ResourceId,
        /// The number the file declares.
        found: u32,
        /// The only number this game reads.
        supported: u32,
    },
    /// A texture id in a block is not a valid resource id.
    InvalidTexture {
        /// The blocks file.
        file: ResourceId,
        /// Name of the block, as written in the file.
        block: String,
        /// The texture key (`all`, `side`, `top` or `bottom`).
        key: &'static str,
        /// Why the id is invalid. Boxed to keep the error type small.
        source: Box<ResourceError>,
    },
    /// A block name is empty or uses characters a name must not have.
    InvalidName {
        /// The blocks file.
        file: ResourceId,
        /// The name as written in the file.
        block: String,
    },
    /// A face of a block ends up without a texture.
    MissingFace {
        /// The blocks file.
        file: ResourceId,
        /// Name of the block, as written in the file.
        block: String,
        /// The face that has no texture.
        face: Face,
    },
    /// The registry refused the block (an empty or duplicate name, or no id left).
    Register {
        /// The blocks file.
        file: ResourceId,
        /// Name of the block, as written in the file.
        block: String,
        /// The registry's reason.
        source: RegistryError,
    },
    /// More distinct textures than [`MAX_LAYERS`] (the array layers every device has).
    TooManyTextures {
        /// The blocks file.
        file: ResourceId,
    },
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(f, "{error}"),
            Self::Parse { file, source } => write!(f, "blocks file `{file}`: {source}"),
            Self::UnsupportedFormat {
                file,
                found,
                supported,
            } => write!(
                f,
                "blocks file `{file}`: format {found} is not supported, expected {supported}"
            ),
            Self::InvalidTexture {
                file,
                block,
                key,
                source,
            } => write!(
                f,
                "blocks file `{file}`, block `{block}`, texture `{key}`: {source}"
            ),
            Self::InvalidName { file, block } => write!(
                f,
                "blocks file `{file}`: the block name `{block}` is not valid                  (use lowercase letters, digits, `_`, `.` and `-`, no `:` or `/`)"
            ),
            Self::MissingFace { file, block, face } => write!(
                f,
                "blocks file `{file}`, block `{block}`: the {face:?} face has no texture \
                 (give `all`, or `side`, `top` and `bottom`)"
            ),
            Self::Register {
                file,
                block,
                source,
            } => write!(f, "blocks file `{file}`, block `{block}`: {source}"),
            Self::TooManyTextures { file } => {
                write!(
                    f,
                    "blocks file `{file}`: more than {MAX_LAYERS} different textures"
                )
            }
        }
    }
}

impl Error for ContentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read(source) => Some(source),
            Self::InvalidTexture { source, .. } => Some(source.as_ref()),
            Self::Parse { source, .. } => Some(source),
            Self::Register { source, .. } => Some(source),
            Self::UnsupportedFormat { .. }
            | Self::InvalidName { .. }
            | Self::MissingFace { .. }
            | Self::TooManyTextures { .. } => None,
        }
    }
}

impl From<ResourceError> for ContentError {
    fn from(error: ResourceError) -> Self {
        Self::Read(error)
    }
}
