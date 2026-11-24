//! The mod system, first step: reading mod manifests, finding mods on disk and putting them in
//! load order (decision 0014).
//!
//! A mod is a directory `<id>/` with a `mod.json` manifest and an `assets/` pack. This crate
//! reads only the manifests: it runs no scripts and reads no assets. It knows nothing about
//! rendering. The game calls it at startup (`vox-game`, decision 0014 step S3).
//!
//! - [`manifest`]: [`ModManifest`], [`ModId`], [`ApiVersion`], [`Capability`] and their rules.
//! - [`discover()`]: finds the mods of a folder.
//! - [`resolve()`]: checks versions and dependencies and returns the load order (pure).
//! - [`error`]: the error types.

mod discover;
pub mod error;
pub mod manifest;
mod resolve;

pub use discover::{ASSETS_DIR, DiscoveredMod, MANIFEST_FILE, discover};
pub use error::{ManifestProblem, ModError, ResolveError};
pub use manifest::{
    ApiVersion, Capability, MANIFEST_FORMAT, MAX_MANIFEST_BYTES, MOD_API_VERSION, ModId,
    ModManifest, RESERVED_MOD_ID,
};
pub use resolve::resolve;
