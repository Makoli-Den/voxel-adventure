//! The virtual resource file system: `namespace:path` ids and layered packs.
//!
//! Every file the game loads (shaders, block definitions, textures, later sounds and
//! translations) is addressed by a [`ResourceId`] and served by a [`PackStack`], where a
//! higher pack overrides a lower one file by file (see `CLAUDE.md`, architecture rule 4).
//! This crate knows nothing about what the bytes mean and has no dependencies.

mod error;
mod id;
mod pack;

pub use error::{IdProblem, ResourceError};
pub use id::{NAMESPACE_SEPARATOR, PATH_SEPARATOR, ResourceId, validate_namespace};
pub use pack::{DirPack, PackStack};
