//! The mod system: reading mod manifests, finding mods on disk, putting them in load order and
//! running their scripts in a Lua sandbox (decision 0014).
//!
//! A mod is a directory `<id>/` with a `mod.json` manifest and an `assets/` pack, whose entry
//! script runs in a Lua 5.4 state of its own. This crate knows nothing about rendering; of the
//! world it knows only `vox-world`'s `World` and `BlockRegistry`, which handlers read. The game
//! resolves the mods at startup, starts their scripts and sends them the player's block edits
//! (`vox-game`, decision 0014 steps S3 and S5).
//!
//! - [`manifest`]: [`ModManifest`], [`ModId`], [`ApiVersion`], [`Capability`] and their rules.
//! - [`discover()`]: finds the mods of a folder.
//! - [`resolve()`]: checks versions and dependencies and returns the load order (pure).
//! - [`ModHost`]: runs the mods' scripts, each in its own sandbox with instruction, memory and
//!   log limits, sends them [`BlockEvent`]s, collects the [`BlockEdit`]s they queue, and
//!   disables a mod whose script fails.
//! - [`error`]: the error types.

mod api;
mod budget;
mod discover;
pub mod error;
mod host;
mod library;
pub mod manifest;
mod resolve;
mod sandbox;
mod text;
mod world_handle;

pub use api::BlockEventKind;
pub use budget::{
    HOOK_INTERVAL, MAX_CALL_WALL_TIME, MAX_HOST_CALL_DEPTH, MAX_INSTRUCTIONS_AT_LOAD,
    MAX_INSTRUCTIONS_PER_CALL, MAX_LOAD_WALL_TIME,
};
pub use discover::{ASSETS_DIR, DiscoveredMod, MANIFEST_FILE, discover};
pub use error::{MAX_FAILURE_MESSAGE_BYTES, ManifestProblem, ModError, ModFailure, ResolveError};
pub use host::{MAX_SCRIPT_BYTES, MessageKind, ModHost, ModMessage, ModStatus};
pub use library::BYTES_PER_INSTRUCTION;
pub use manifest::{
    ApiVersion, Capability, MANIFEST_FORMAT, MAX_MANIFEST_BYTES, MOD_API_VERSION, ModId,
    ModManifest, RESERVED_MOD_ID,
};
pub use resolve::resolve;
pub use sandbox::{MAX_LOG_LINE_BYTES, MAX_LOG_LINES_PER_CALL, MAX_MOD_MEMORY};
pub use world_handle::{BlockEdit, BlockEvent, MAX_EDITS_PER_CALL, MAX_EDITS_PER_EVENT};
