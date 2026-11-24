//! The mods' scripts in the running game: the lines printed when they start and for what they
//! report while running (decision 0014).
//!
//! The scripts themselves run in `vox-mods`; this module only turns their statuses and
//! messages into console lines, so the wording can be tested without a window.

use vox_mods::{Capability, DiscoveredMod, MessageKind, ModMessage, ModStatus};

/// The line for how starting one mod's script ended. `mods` are the mods it was started from,
/// to name the capabilities of a running one (the game prints them, decision 0014 c).
pub fn status_line(status: &ModStatus, mods: &[DiscoveredMod]) -> String {
    match status {
        ModStatus::Loaded { id } => {
            let capabilities: Vec<&str> = mods
                .iter()
                .find(|found| found.manifest().id() == id)
                .map(|found| {
                    found
                        .manifest()
                        .capabilities()
                        .iter()
                        .map(|capability| Capability::name(*capability))
                        .collect()
                })
                .unwrap_or_default();
            let listed = if capabilities.is_empty() {
                "none".to_owned()
            } else {
                capabilities.join(", ")
            };
            format!("Mod `{id}`: script running, capabilities: {listed}")
        }
        ModStatus::Disabled { id, failure } => {
            format!("Mod `{id}`: script disabled: {failure}")
        }
    }
}

/// The console line for one message of a running mod, tagged with the mod's id. The text is
/// already made printable by `vox-mods` (one line, no control characters).
pub fn message_line(message: &ModMessage) -> String {
    let id = &message.id;
    match &message.kind {
        MessageKind::Log(line) => format!("[{id}] {line}"),
        MessageKind::LinesDropped(count) => {
            format!("[{id}] ({count} more lines of one call were dropped)")
        }
        MessageKind::Disabled(failure) => {
            format!("warning: mod `{id}` disabled: {failure}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use vox_mods::{BlockEdit, BlockEvent, BlockEventKind, ModFailure, ModHost, ModId};
    use vox_world::{BlockId, Chunk, ChunkPos, World};

    use super::*;
    use crate::content::{Content, STONE_NAME};
    use crate::editing::{EditedChunks, StaleChunks, apply_mod_edits};
    use crate::packs;
    use crate::startup::assemble;
    use crate::streaming::{Column, ColumnSpans, Terrain};

    /// The example mods of the source tree.
    fn examples_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/mods")
    }

    fn id(text: &str) -> ModId {
        ModId::parse(text).unwrap()
    }

    #[test]
    fn messages_carry_the_mod_id() {
        let message = |kind| {
            message_line(&ModMessage {
                id: id("ruby"),
                kind,
            })
        };
        assert_eq!(
            message(MessageKind::Log("hello".to_owned())),
            "[ruby] hello"
        );
        assert_eq!(
            message(MessageKind::LinesDropped(3)),
            "[ruby] (3 more lines of one call were dropped)"
        );
        assert_eq!(
            message(MessageKind::Disabled(ModFailure::TooManyEdits {
                limit: 64
            })),
            "warning: mod `ruby` disabled: queued more than 64 block writes in one call"
        );
    }

    #[test]
    fn the_example_mod_runs_with_its_capabilities() {
        let dirs = [packs::built_in_mods_dir(), examples_dir()];
        let startup = assemble(&dirs, packs::built_in_pack(), &[]).unwrap();
        assert_eq!(startup.load_order_line(), "Mods: base, example_pillar");
        let content = Content::new(&startup.stack, &startup.blocks_files).unwrap();
        let (mut host, statuses) = ModHost::start(&startup.mods);
        // `base` has no script, so only the example is listed.
        let lines: Vec<String> = statuses
            .iter()
            .map(|status| status_line(status, &startup.mods))
            .collect();
        assert_eq!(
            lines,
            ["Mod `example_pillar`: script running, capabilities: world.read, world.write"]
        );

        // Placing stone grows a pillar of four above it; breaking it takes the pillar down.
        let registry = &content.registry;
        let stone = registry.id(STONE_NAME).unwrap();
        let mut world = World::new();
        world.insert(ChunkPos::ORIGIN, Chunk::new());
        world.set_block([4, 4, 4], stone);
        let placed = BlockEvent {
            kind: BlockEventKind::BlockPlaced,
            position: [4, 4, 4],
            block: stone,
        };
        let edits = host.block_event(&placed, &world, registry);
        let pillar: Vec<BlockEdit> = (5..=8)
            .map(|y| BlockEdit {
                position: [4, y, 4],
                block: stone,
            })
            .collect();
        assert_eq!(edits, pillar);
        let mut stale = StaleChunks::default();
        let terrain = Terrain::new(0, content.terrain, content.face_textures());
        let mut spans = ColumnSpans::default();
        spans.insert(Column::new(0, 0), 0..=0);
        let mut edited = EditedChunks::default();
        let created = apply_mod_edits(
            &mut world,
            &terrain,
            &spans,
            &mut edited,
            &edits,
            &mut stale,
        );
        assert_eq!(created, 0);
        assert!(edited.contains(ChunkPos::ORIGIN));
        assert_eq!(stale.as_slice(), [ChunkPos::ORIGIN]);

        world.set_block([4, 4, 4], BlockId::AIR);
        let broken = BlockEvent {
            kind: BlockEventKind::BlockBroken,
            ..placed
        };
        let edits = host.block_event(&broken, &world, registry);
        assert!(
            edits.iter().all(|edit| edit.block.is_air()) && edits.len() == pillar.len(),
            "{edits:?}"
        );
        let printed: Vec<String> = host.take_messages().iter().map(message_line).collect();
        assert_eq!(
            printed,
            [
                "[example_pillar] a pillar of 4 grew on the stone at 4 4 4",
                "[example_pillar] stone broken at 4 4 4, 4 stone above it fell",
            ]
        );
    }

    #[test]
    fn a_disabled_script_is_named_with_why() {
        let status = ModStatus::Disabled {
            id: id("ruby"),
            failure: ModFailure::Memory,
        };
        let line = status_line(&status, &[]);
        assert!(
            line.starts_with("Mod `ruby`: script disabled: used more than"),
            "{line}"
        );
    }
}
