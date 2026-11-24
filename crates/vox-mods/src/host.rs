//! [`ModHost`]: every running mod script, each in its own [`Sandbox`], and what they reported.
//!
//! A failing script disables only its own mod: its Lua state is dropped and the failure is
//! reported as data, never as a panic or an early return, so the other mods and the game go on
//! (decision 0014, section b). Data-only mods (no `entry`) have no state and are not listed.
//!
//! The entry script is read from the mod's own folder, not through the pack stack: another
//! pack may override a mod's textures, but never its code.

use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::{Component, Path};

use mlua::Function;
use vox_resources::{PATH_SEPARATOR, ResourceId};
use vox_world::{BlockRegistry, World};

use crate::api::BlockEventKind;
use crate::discover::{ASSETS_DIR, DiscoveredMod};
use crate::error::ModFailure;
use crate::manifest::{Capability, ModId};
use crate::sandbox::{CallLimits, LogEntry, Sandbox};
use crate::world_handle::{
    BlockEdit, BlockEvent, CallRecord, MAX_EDITS_PER_EVENT, WorldAccess, event_table, world_handle,
};

/// Largest entry script that is read, 1 MiB. Real event scripts are a few KiB; the cap keeps a
/// broken or hostile file from being read into memory whole. Provisional.
pub const MAX_SCRIPT_BYTES: usize = 1024 * 1024;

/// Marks a Lua chunk name to be shown as written, so errors read `ruby:scripts/main.lua:3:`.
const CHUNK_NAME_AS_IS: char = '=';

/// How starting one mod's script ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModStatus {
    /// The entry script ran to the end; the mod is running.
    Loaded {
        /// The mod.
        id: ModId,
    },
    /// The script could not be read or failed; the mod's code is off for this session.
    Disabled {
        /// The mod.
        id: ModId,
        /// Why.
        failure: ModFailure,
    },
}

/// Something a mod reported while running, for the game to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModMessage {
    /// The mod it comes from.
    pub id: ModId,
    /// What happened.
    pub kind: MessageKind,
}

/// The content of a [`ModMessage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageKind {
    /// A line the mod printed, cut to [`MAX_LOG_LINE_BYTES`](crate::MAX_LOG_LINE_BYTES).
    Log(String),
    /// How many lines of one call were dropped by
    /// [`MAX_LOG_LINES_PER_CALL`](crate::MAX_LOG_LINES_PER_CALL).
    LinesDropped(usize),
    /// The mod failed while running and was disabled.
    Disabled(ModFailure),
}

/// A mod whose script is running.
struct RunningMod {
    id: ModId,
    /// What its manifest declares; the world handle refuses the rest.
    capabilities: Vec<Capability>,
    sandbox: Sandbox,
}

/// All running mod scripts, in load order. Not `Send`: it lives on the main thread.
pub struct ModHost {
    mods: Vec<RunningMod>,
    messages: Vec<ModMessage>,
}

impl ModHost {
    /// Starts the scripts of `mods`, given in load order: each entry script runs once in a
    /// fresh sandbox. A mod whose script cannot be read or fails is disabled; the others still
    /// start. Returns one [`ModStatus`] per mod with an entry script, in the same order. Lines
    /// the scripts printed wait in [`ModHost::take_messages`].
    pub fn start(mods: &[DiscoveredMod]) -> (Self, Vec<ModStatus>) {
        let mut host = Self {
            mods: Vec::new(),
            messages: Vec::new(),
        };
        let mut statuses = Vec::new();
        for found in mods {
            let Some(entry) = found.manifest().entry() else {
                continue;
            };
            let id = found.manifest().id().clone();
            match host.load(found, entry) {
                Ok(sandbox) => {
                    statuses.push(ModStatus::Loaded { id: id.clone() });
                    host.mods.push(RunningMod {
                        id,
                        capabilities: found.manifest().capabilities().to_vec(),
                        sandbox,
                    });
                }
                Err(failure) => statuses.push(ModStatus::Disabled { id, failure }),
            }
        }
        (host, statuses)
    }

    /// Reads and runs one entry script; its log goes to the messages even if it failed.
    fn load(&mut self, found: &DiscoveredMod, entry: &ResourceId) -> Result<Sandbox, ModFailure> {
        let source = read_entry(found.dir(), found.manifest().id(), entry)?;
        let sandbox = Sandbox::new().map_err(|error| ModFailure::Setup(error.to_string()))?;
        let result = sandbox.exec(&source, &format!("{CHUNK_NAME_AS_IS}{entry}"));
        collect_log(found.manifest().id(), &sandbox, &mut self.messages);
        result.map(|()| sandbox)
    }

    /// The messages since the last call: printed lines and mods disabled while running.
    pub fn take_messages(&mut self) -> Vec<ModMessage> {
        std::mem::take(&mut self.messages)
    }

    /// Whether the script of mod `id` is running: it has one, it started, and it has not
    /// failed since.
    pub fn is_enabled(&self, id: &ModId) -> bool {
        self.mods.iter().any(|running| &running.id == id)
    }

    /// Whether any running mod registered a handler for `kind`. When none did, the game need
    /// not build the event at all, so an unhandled event costs nothing (decision 0014, d).
    pub fn has_handler(&self, kind: BlockEventKind) -> bool {
        self.mods
            .iter()
            .any(|running| running.sandbox.handlers().borrow().contains(kind))
    }

    /// Bytes the Lua state of mod `id` uses now, or `None` if its script is not running.
    /// Every state is capped at [`MAX_MOD_MEMORY`](crate::MAX_MOD_MEMORY).
    pub fn used_memory(&self, id: &ModId) -> Option<usize> {
        self.mods
            .iter()
            .find(|running| &running.id == id)
            .map(|running| running.sandbox.used_memory())
    }

    /// Sends `event` to the handler of every running mod that has one, in load order, and
    /// returns the block writes they queued, in load order and in each mod's own order, so
    /// applying them one after the other lets a later mod win on the same cell. At most
    /// [`MAX_EDITS_PER_EVENT`] are returned; the rest are dropped without disabling their mod,
    /// and a log line of that mod says how many.
    ///
    /// Every handler reads the same `world`, the state right after the player's edit: writes
    /// are only queued, never applied here. A mod that fails (a Lua error, a spent budget, a
    /// missing capability, an unknown block name, too many writes) is disabled; its writes of
    /// this call are dropped and a [`MessageKind::Disabled`] waits in
    /// [`ModHost::take_messages`]. The game applies the result without sending events for it,
    /// so mods never call each other through writes.
    ///
    /// Costs nothing and allocates nothing when no running mod handles `event.kind`. An event
    /// whose block the registry does not know (the game never makes one) is not sent.
    pub fn block_event(
        &mut self,
        event: &BlockEvent,
        world: &World,
        registry: &BlockRegistry,
    ) -> Vec<BlockEdit> {
        let mut edits = Vec::new();
        if !self.has_handler(event.kind) {
            return edits;
        }
        let Some(block_name) = registry.name(event.block) else {
            return edits;
        };
        let mut index = 0;
        while index < self.mods.len() {
            let running = &self.mods[index];
            let handler = running.sandbox.handlers().borrow().get(event.kind);
            let result = match handler {
                Some(handler) => {
                    call_handler(running, &handler, event, block_name, world, registry)
                }
                None => Ok(Vec::new()),
            };
            collect_log(&running.id, &running.sandbox, &mut self.messages);
            match result {
                Ok(mut queued) => {
                    let room = MAX_EDITS_PER_EVENT - edits.len();
                    if queued.len() > room {
                        let dropped = queued.len() - room;
                        queued.truncate(room);
                        self.messages.push(ModMessage {
                            id: self.mods[index].id.clone(),
                            kind: MessageKind::Log(format!(
                                "{dropped} block writes dropped: an event applies at most \
                                 {MAX_EDITS_PER_EVENT} writes of all mods together"
                            )),
                        });
                    }
                    edits.extend(queued);
                    index += 1;
                }
                Err(failure) => {
                    let removed = self.mods.remove(index);
                    self.messages.push(ModMessage {
                        id: removed.id,
                        kind: MessageKind::Disabled(failure),
                    });
                }
            }
        }
        edits
    }
}

/// Calls one mod's `handler` with the event table and a world handle that lives as long as
/// the call. Returns the writes it queued, or why the mod must be disabled.
fn call_handler(
    running: &RunningMod,
    handler: &Function,
    event: &BlockEvent,
    block_name: &str,
    world: &World,
    registry: &BlockRegistry,
) -> Result<Vec<BlockEdit>, ModFailure> {
    let record = RefCell::new(CallRecord::default());
    let access = WorldAccess {
        world,
        registry,
        capabilities: &running.capabilities,
    };
    let budget = running.sandbox.budget();
    let result = running.sandbox.run(CallLimits::HANDLER, |lua| {
        lua.scope(|scope| {
            let event = event_table(lua, event, block_name)?;
            let handle = world_handle(scope, lua, budget, access, &record)?;
            handler.call::<()>((event, handle))
        })
    });
    let CallRecord { edits, violation } = record.into_inner();
    match (result, violation) {
        // A spent budget is reported as such: it is what stopped the script.
        (Err(failure), _) if is_limit(&failure) => Err(failure),
        // A mistake disables the mod even if the script caught its error.
        (_, Some(violation)) => Err(violation),
        (Err(failure), None) => Err(failure),
        (Ok(()), None) => Ok(edits),
    }
}

/// Whether `failure` is a spent instruction, time or memory budget.
fn is_limit(failure: &ModFailure) -> bool {
    matches!(
        failure,
        ModFailure::InstructionBudget { .. } | ModFailure::TimeBudget { .. } | ModFailure::Memory
    )
}

/// Moves the log of `sandbox` into `messages`, tagged with `id`.
fn collect_log(id: &ModId, sandbox: &Sandbox, messages: &mut Vec<ModMessage>) {
    messages.extend(sandbox.take_log().into_iter().map(|entry| ModMessage {
        id: id.clone(),
        kind: match entry {
            LogEntry::Line(line) => MessageKind::Log(line),
            LogEntry::Dropped(count) => MessageKind::LinesDropped(count),
        },
    }));
}

/// Reads the entry script `entry` of the mod `id` in `mod_dir`: the file
/// `<mod_dir>/assets/<id>/<path>`.
///
/// The manifest already guarantees the entry is in the mod's namespace and made of plain
/// names; this is checked again because it guards code loading. No folder on the way and not
/// the file itself may be a link, so the script is inside the mod folder for real, not only
/// by name. (A link swapped in between the check and the read is not caught; mod folders are
/// local, and the check is about mistakes and downloaded archives, not a racing attacker.)
fn read_entry(mod_dir: &Path, id: &ModId, entry: &ResourceId) -> Result<Vec<u8>, ModFailure> {
    let outside = || ModFailure::EntryOutsideMod {
        entry: entry.to_string(),
    };
    if entry.namespace() != id.as_str() {
        return Err(outside());
    }
    let names = [ASSETS_DIR, entry.namespace()]
        .into_iter()
        .chain(entry.path().split(PATH_SEPARATOR));
    let mut path = mod_dir.to_owned();
    for name in names {
        if !is_plain_name(name) {
            return Err(outside());
        }
        path.push(name);
        check_not_link(&path)?;
    }
    let io_error = |path: &Path, error: std::io::Error| ModFailure::EntryIo {
        path: path.to_owned(),
        message: error.to_string(),
    };
    if !fs::symlink_metadata(&path)
        .map_err(|error| io_error(&path, error))?
        .is_file()
    {
        return Err(ModFailure::EntryNotAFile { path });
    }
    let limit = u64::try_from(MAX_SCRIPT_BYTES).unwrap_or(u64::MAX);
    let mut source = Vec::new();
    File::open(&path)
        .and_then(|file| file.take(limit.saturating_add(1)).read_to_end(&mut source))
        .map_err(|error| io_error(&path, error))?;
    if source.len() > MAX_SCRIPT_BYTES {
        return Err(ModFailure::EntryTooLarge {
            path,
            limit: MAX_SCRIPT_BYTES,
        });
    }
    Ok(source)
}

/// Whether `name` is exactly one ordinary path component: not empty, no separator, no `.`,
/// `..`, root or drive prefix.
fn is_plain_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(part)), None) if part == name
    )
}

/// Fails if `path` is a link (symbolic link or junction). A missing path is left to the read,
/// which reports it with its path.
fn check_not_link(path: &Path) -> Result<(), ModFailure> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(ModFailure::EntryLink {
            path: path.to_owned(),
        }),
        Err(error) if error.kind() != ErrorKind::NotFound => Err(ModFailure::EntryIo {
            path: path.to_owned(),
            message: error.to_string(),
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::budget::{MAX_INSTRUCTIONS_AT_LOAD, MAX_INSTRUCTIONS_PER_CALL};
    use crate::discover::discover;
    use crate::error::MAX_FAILURE_MESSAGE_BYTES;
    use crate::sandbox::MAX_LOG_LINES_PER_CALL;
    use crate::world_handle::MAX_EDITS_PER_CALL;
    use std::path::PathBuf;
    use vox_world::{BlockId, Chunk, ChunkPos, Face, TextureLayer};

    /// Sends an event of `kind` about air at the origin of an empty world, for tests that only
    /// need the handlers to run.
    fn fire(host: &mut ModHost, kind: BlockEventKind) -> Vec<BlockEdit> {
        let event = BlockEvent {
            kind,
            position: [0, 0, 0],
            block: BlockId::AIR,
        };
        host.block_event(&event, &World::new(), &BlockRegistry::new())
    }

    /// The fixture mods with scripts, by folder name.
    fn fixture_mods() -> Vec<DiscoveredMod> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scripts");
        discover(&dir).unwrap()
    }

    /// The fixture mods called `names`, in that order (the load order of the test).
    fn mods(names: &[&str]) -> Vec<DiscoveredMod> {
        let all = fixture_mods();
        names
            .iter()
            .map(|name| {
                all.iter()
                    .find(|found| found.manifest().id().as_str() == *name)
                    .unwrap_or_else(|| panic!("no fixture mod `{name}`"))
                    .clone()
            })
            .collect()
    }

    fn id(text: &str) -> ModId {
        ModId::parse(text).unwrap()
    }

    fn log(id_text: &str, line: &str) -> ModMessage {
        ModMessage {
            id: id(id_text),
            kind: MessageKind::Log(line.to_owned()),
        }
    }

    #[test]
    fn hello_world_prints_with_its_id() {
        let (mut host, statuses) = ModHost::start(&mods(&["hello"]));
        assert_eq!(statuses, vec![ModStatus::Loaded { id: id("hello") }]);
        assert!(host.is_enabled(&id("hello")));
        assert_eq!(
            host.take_messages(),
            vec![log("hello", "hello from hello, api 1.0")]
        );
        assert!(host.take_messages().is_empty(), "messages are taken once");
    }

    #[test]
    fn has_handler_follows_the_running_mods() {
        let (mut host, _) = ModHost::start(&mods(&["endless_handler"]));
        assert!(host.has_handler(BlockEventKind::BlockBroken));
        assert!(!host.has_handler(BlockEventKind::BlockPlaced));
        fire(&mut host, BlockEventKind::BlockBroken);
        assert!(
            !host.has_handler(BlockEventKind::BlockBroken),
            "a disabled mod handles nothing"
        );
    }

    #[test]
    fn used_memory_is_reported_for_running_mods_only() {
        let (host, _) = ModHost::start(&mods(&["hello", "syntax"]));
        let used = host.used_memory(&id("hello")).unwrap();
        assert!(used > 0 && used <= crate::MAX_MOD_MEMORY, "{used}");
        assert_eq!(host.used_memory(&id("syntax")), None);
    }

    #[test]
    fn a_syntax_error_disables_only_that_mod() {
        let (mut host, statuses) = ModHost::start(&mods(&["syntax", "hello"]));
        assert!(
            matches!(&statuses[0], ModStatus::Disabled { id: failed, failure: ModFailure::Syntax(message) }
                if *failed == id("syntax") && message.contains("syntax:scripts/main.lua:")),
            "{statuses:?}"
        );
        assert_eq!(statuses[1], ModStatus::Loaded { id: id("hello") });
        assert!(!host.is_enabled(&id("syntax")));
        assert!(host.is_enabled(&id("hello")));
        assert_eq!(
            host.take_messages(),
            vec![log("hello", "hello from hello, api 1.0")]
        );
    }

    #[test]
    fn an_endless_loop_at_load_disables_the_mod() {
        let (mut host, statuses) = ModHost::start(&mods(&["endless_load", "hello"]));
        assert_eq!(
            statuses,
            vec![
                ModStatus::Disabled {
                    id: id("endless_load"),
                    failure: ModFailure::InstructionBudget {
                        limit: MAX_INSTRUCTIONS_AT_LOAD
                    }
                },
                ModStatus::Loaded { id: id("hello") },
            ]
        );
        // What it printed before the loop still arrives.
        assert_eq!(
            host.take_messages(),
            vec![
                log("endless_load", "about to loop"),
                log("hello", "hello from hello, api 1.0")
            ]
        );
    }

    #[test]
    fn an_endless_loop_in_a_handler_disables_the_mod() {
        let (mut host, statuses) = ModHost::start(&mods(&["endless_handler", "hello"]));
        assert!(
            statuses
                .iter()
                .all(|s| matches!(s, ModStatus::Loaded { .. }))
        );
        host.take_messages();
        fire(&mut host, BlockEventKind::BlockBroken);
        assert!(!host.is_enabled(&id("endless_handler")));
        assert!(host.is_enabled(&id("hello")));
        assert_eq!(
            host.take_messages(),
            vec![
                ModMessage {
                    id: id("endless_handler"),
                    kind: MessageKind::Disabled(ModFailure::InstructionBudget {
                        limit: MAX_INSTRUCTIONS_PER_CALL
                    })
                },
                log("hello", "hello saw a block break"),
            ]
        );
        // A disabled mod is not called again.
        fire(&mut host, BlockEventKind::BlockBroken);
        assert_eq!(
            host.take_messages(),
            vec![log("hello", "hello saw a block break")]
        );
    }

    #[test]
    fn an_endless_loop_inside_pcall_still_disables_the_mod() {
        let (mut host, statuses) = ModHost::start(&mods(&["pcall_loop"]));
        assert_eq!(
            statuses,
            vec![ModStatus::Loaded {
                id: id("pcall_loop")
            }]
        );
        fire(&mut host, BlockEventKind::BlockPlaced);
        assert!(!host.is_enabled(&id("pcall_loop")));
        let messages = host.take_messages();
        assert_eq!(
            messages.last().map(|message| &message.kind),
            Some(&MessageKind::Disabled(ModFailure::InstructionBudget {
                limit: MAX_INSTRUCTIONS_PER_CALL
            })),
            "{messages:?}"
        );
        // The script's own line after the caught error never ran.
        assert!(!messages.contains(&log("pcall_loop", "survived")));
    }

    #[test]
    fn a_memory_bomb_disables_the_mod() {
        let (host, statuses) = ModHost::start(&mods(&["memory_bomb", "hello"]));
        assert_eq!(
            statuses,
            vec![
                ModStatus::Disabled {
                    id: id("memory_bomb"),
                    failure: ModFailure::Memory
                },
                ModStatus::Loaded { id: id("hello") },
            ]
        );
        assert!(!host.is_enabled(&id("memory_bomb")));
    }

    #[test]
    fn the_log_cap_holds_per_call() {
        let (mut host, _) = ModHost::start(&mods(&["chatty"]));
        let messages = host.take_messages();
        assert_eq!(messages.len(), MAX_LOG_LINES_PER_CALL + 1);
        assert_eq!(messages[0], log("chatty", "line 1"));
        assert_eq!(
            messages[MAX_LOG_LINES_PER_CALL].kind,
            MessageKind::LinesDropped(1000 - MAX_LOG_LINES_PER_CALL)
        );
        // A handler call has its own allowance.
        fire(&mut host, BlockEventKind::BlockBroken);
        assert_eq!(
            host.take_messages().len(),
            MAX_LOG_LINES_PER_CALL + 1,
            "the handler prints 1000 lines too"
        );
    }

    #[test]
    fn a_mod_without_a_script_is_not_listed() {
        let (host, statuses) = ModHost::start(&mods(&["data_only"]));
        assert!(statuses.is_empty());
        assert!(!host.is_enabled(&id("data_only")));
    }

    #[test]
    fn a_missing_script_disables_the_mod_with_its_path() {
        let (_, statuses) = ModHost::start(&mods(&["no_script"]));
        assert!(
            matches!(&statuses[..], [ModStatus::Disabled { failure: ModFailure::EntryIo { path, .. }, .. }]
                if path.ends_with("assets/no_script/scripts/main.lua")),
            "{statuses:?}"
        );
    }

    #[test]
    fn mods_do_not_share_globals() {
        let (mut host, statuses) = ModHost::start(&mods(&["tamper", "hello"]));
        assert!(
            statuses
                .iter()
                .all(|s| matches!(s, ModStatus::Loaded { .. }))
        );
        // `tamper` replaced `string.format`, `print` and the string metatable in its own
        // state; `hello` formats its line with `string.format` and prints normally.
        assert_eq!(
            host.take_messages(),
            vec![
                log("tamper", "tampered"),
                log("hello", "hello from hello, api 1.0")
            ]
        );
    }

    #[test]
    fn plain_names_only() {
        for good in ["scripts", "main.lua", "a_b"] {
            assert!(is_plain_name(good), "{good}");
        }
        for bad in ["", ".", "..", "a/b", "/", "a/"] {
            assert!(!is_plain_name(bad), "{bad}");
        }
        // Separators and prefixes that only Windows has; elsewhere they are ordinary
        // characters (which resource ids do not allow anyway).
        #[cfg(windows)]
        for bad in ["a\\b", "C:", "C:\\x", "\\\\server\\share"] {
            assert!(!is_plain_name(bad), "{bad}");
        }
    }

    #[test]
    fn an_entry_of_another_namespace_is_refused() {
        let found = &mods(&["hello"])[0];
        let foreign = ResourceId::parse("other:scripts/main.lua").unwrap();
        assert_eq!(
            read_entry(found.dir(), &id("hello"), &foreign),
            Err(ModFailure::EntryOutsideMod {
                entry: "other:scripts/main.lua".to_owned()
            })
        );
    }

    #[test]
    fn a_folder_where_the_script_should_be_is_not_a_file() {
        let found = &mods(&["hello"])[0];
        let folder = ResourceId::parse("hello:scripts").unwrap();
        assert!(matches!(
            read_entry(found.dir(), &id("hello"), &folder),
            Err(ModFailure::EntryNotAFile { .. })
        ));
    }

    /// A scratch mod folder under the system temp folder, removed on drop.
    struct TempMod {
        root: PathBuf,
    }

    impl TempMod {
        fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("vox-mods-host-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }
    }

    impl Drop for TempMod {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// Makes `link` a directory link to `target`: a junction on Windows (needs no privilege),
    /// a symbolic link elsewhere.
    fn link_dir(target: &Path, link: &Path) {
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[test]
    fn a_linked_folder_on_the_way_to_the_script_is_refused() {
        let temp = TempMod::new("link");
        let outside = temp.root.join("outside");
        fs::create_dir_all(outside.join("scripts")).unwrap();
        fs::write(outside.join("scripts/main.lua"), "print('escaped')").unwrap();
        let mod_dir = temp.root.join("linked");
        fs::create_dir_all(mod_dir.join("assets")).unwrap();
        // Built with `join` per name: `mklink` reads a `/` inside a path as a switch.
        let link = mod_dir.join("assets").join("linked");
        link_dir(&outside, &link);
        // The script is reachable through the link, so only the check stops it.
        assert!(link.join("scripts").join("main.lua").is_file());
        let entry = ResourceId::parse("linked:scripts/main.lua").unwrap();
        assert_eq!(
            read_entry(&mod_dir, &id("linked"), &entry),
            Err(ModFailure::EntryLink { path: link })
        );
    }

    #[test]
    fn an_oversized_script_is_refused() {
        let temp = TempMod::new("large");
        let scripts = temp.root.join("assets/large/scripts");
        fs::create_dir_all(&scripts).unwrap();
        fs::write(scripts.join("main.lua"), vec![b' '; MAX_SCRIPT_BYTES + 1]).unwrap();
        let entry = ResourceId::parse("large:scripts/main.lua").unwrap();
        assert!(matches!(
            read_entry(&temp.root, &id("large"), &entry),
            Err(ModFailure::EntryTooLarge {
                limit: MAX_SCRIPT_BYTES,
                ..
            })
        ));
        // At the limit it is read whole.
        fs::write(scripts.join("main.lua"), vec![b' '; MAX_SCRIPT_BYTES]).unwrap();
        assert_eq!(
            read_entry(&temp.root, &id("large"), &entry).map(|source| source.len()),
            Ok(MAX_SCRIPT_BYTES)
        );
    }

    // --- Block events and the world handle (decision 0014, step S5) ---

    /// Both world capabilities.
    const READ_WRITE: &[&str] = &["world.read", "world.write"];

    /// Writes a scripted mod `id` with `capabilities` and the entry script `source` into
    /// `temp`.
    fn scripted(temp: &TempMod, id: &str, capabilities: &[&str], source: &str) {
        let dir = temp.root.join(id);
        let scripts = dir.join("assets").join(id).join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        let listed: Vec<String> = capabilities.iter().map(|c| format!("\"{c}\"")).collect();
        fs::write(
            dir.join("mod.json"),
            format!(
                r#"{{ "format": 1, "id": "{id}", "version": "0.1.0", "api": "1.0",
                    "entry": "{id}:scripts/main.lua", "capabilities": [{}] }}"#,
                listed.join(", ")
            ),
        )
        .unwrap();
        fs::write(scripts.join("main.lua"), source).unwrap();
    }

    /// Starts the mods `ids` of `temp` in that order; all must load.
    fn start(temp: &TempMod, ids: &[&str]) -> ModHost {
        let all = discover(&temp.root).unwrap();
        let ordered: Vec<DiscoveredMod> = ids
            .iter()
            .map(|wanted| {
                all.iter()
                    .find(|found| found.manifest().id().as_str() == *wanted)
                    .unwrap()
                    .clone()
            })
            .collect();
        let (mut host, statuses) = ModHost::start(&ordered);
        assert!(
            statuses
                .iter()
                .all(|s| matches!(s, ModStatus::Loaded { .. })),
            "{statuses:?}"
        );
        assert!(host.take_messages().is_empty());
        host
    }

    /// Air plus the blocks the tests name.
    fn registry() -> BlockRegistry {
        let mut registry = BlockRegistry::new();
        for name in ["test:stone", "test:gold", "test:sand"] {
            registry
                .register(name, [TextureLayer::MISSING; Face::COUNT])
                .unwrap();
        }
        registry
    }

    fn block(registry: &BlockRegistry, name: &str) -> BlockId {
        registry.id(name).unwrap()
    }

    /// A world of two empty chunks: the one at the origin and the one at (-1, -1, -1), so
    /// negative coordinates are covered too.
    fn world() -> World {
        let mut world = World::new();
        world.insert(ChunkPos::ORIGIN, Chunk::new());
        world.insert(ChunkPos::new(-1, -1, -1), Chunk::new());
        world
    }

    fn event(kind: BlockEventKind, position: [i32; 3], block: BlockId) -> BlockEvent {
        BlockEvent {
            kind,
            position,
            block,
        }
    }

    fn edit(position: [i32; 3], block: BlockId) -> BlockEdit {
        BlockEdit { position, block }
    }

    fn disabled(id_text: &str, failure: ModFailure) -> ModMessage {
        ModMessage {
            id: id(id_text),
            kind: MessageKind::Disabled(failure),
        }
    }

    #[test]
    fn reads_see_the_world_after_the_players_edit() {
        let temp = TempMod::new("reads");
        scripted(
            &temp,
            "reader",
            &["world.read"],
            r#"vox.on("block_broken", function(event, world)
                 print(event.x, event.y, event.z, event.block)
                 print(world:get_block(event.x, event.y, event.z), world:get_block(2, 0, 0),
                       world:get_block(-5, -7, -9))
               end)"#,
        );
        let mut host = start(&temp, &["reader"]);
        let registry = registry();
        let stone = block(&registry, "test:stone");
        let mut world = world();
        world.set_block([2, 0, 0], block(&registry, "test:gold"));
        world.set_block([-5, -7, -9], block(&registry, "test:sand"));
        world.set_block([1, 2, 3], stone);
        // The player breaks the stone: the game writes first, then sends the event.
        world.set_block([1, 2, 3], BlockId::AIR);
        let edits = host.block_event(
            &event(BlockEventKind::BlockBroken, [1, 2, 3], stone),
            &world,
            &registry,
        );
        assert!(edits.is_empty());
        assert_eq!(
            host.take_messages(),
            vec![
                log("reader", "1\t2\t3\ttest:stone"),
                log("reader", "core:air\ttest:gold\ttest:sand"),
            ]
        );
    }

    #[test]
    fn writes_come_back_in_load_order_and_a_later_mod_wins() {
        let temp = TempMod::new("order");
        scripted(
            &temp,
            "first",
            READ_WRITE,
            r#"vox.on("block_placed", function(event, world)
                 world:set_block(0, 0, 0, "test:gold")
                 world:set_block(-1, -1, -1, "test:gold")
                 event.block = "changed by first"
               end)"#,
        );
        scripted(
            &temp,
            "second",
            READ_WRITE,
            r#"vox.on("block_placed", function(event, world)
                 -- The writes of `first` are queued, not applied: still air here.
                 print(event.block, world:get_block(0, 0, 0))
                 world:set_block(0, 0, 0, "test:sand")
               end)"#,
        );
        let registry = registry();
        let stone = block(&registry, "test:stone");
        let gold = block(&registry, "test:gold");
        let sand = block(&registry, "test:sand");
        let placed = event(BlockEventKind::BlockPlaced, [5, 5, 5], stone);

        let mut host = start(&temp, &["first", "second"]);
        let mut world = world();
        let edits = host.block_event(&placed, &world, &registry);
        assert_eq!(
            edits,
            vec![
                edit([0, 0, 0], gold),
                edit([-1, -1, -1], gold),
                edit([0, 0, 0], sand)
            ]
        );
        assert_eq!(
            host.take_messages(),
            vec![log("second", "test:stone\tcore:air")]
        );
        for edit in &edits {
            world.set_block(edit.position, edit.block);
        }
        assert_eq!(world.block_at([0, 0, 0]), Some(sand), "the later mod wins");
        assert_eq!(world.block_at([-1, -1, -1]), Some(gold));

        // The other way round, `first` comes last and wins.
        let mut host = start(&temp, &["second", "first"]);
        let edits = host.block_event(&placed, &World::new(), &registry);
        assert!(edits.is_empty(), "no chunk is loaded: {edits:?}");
        let edits = host.block_event(&placed, &self::world(), &registry);
        assert_eq!(edits.last(), Some(&edit([-1, -1, -1], gold)));
        assert_eq!(edits.first(), Some(&edit([0, 0, 0], sand)));
    }

    #[test]
    fn an_unloaded_chunk_reads_nil_and_writes_false() {
        let temp = TempMod::new("unloaded");
        scripted(
            &temp,
            "edge",
            READ_WRITE,
            r#"vox.on("block_broken", function(event, world)
                 print(world:get_block(1000, 0, 0) == nil, world:get_block(0, -1000, 0) == nil,
                       world:set_block(0, 0, -1000, "test:gold"), world:set_block(0, 0, 0, "test:gold"))
               end)"#,
        );
        let mut host = start(&temp, &["edge"]);
        let registry = registry();
        let edits = fire_at_origin(&mut host, &registry);
        assert_eq!(edits, vec![edit([0, 0, 0], block(&registry, "test:gold"))]);
        assert_eq!(
            host.take_messages(),
            vec![log("edge", "true\ttrue\tfalse\ttrue")]
        );
    }

    /// Sends `block_broken` of stone at the origin of [`world`].
    fn fire_at_origin(host: &mut ModHost, registry: &BlockRegistry) -> Vec<BlockEdit> {
        let stone = block(registry, "test:stone");
        host.block_event(
            &event(BlockEventKind::BlockBroken, [0, 0, 0], stone),
            &world(),
            registry,
        )
    }

    #[test]
    fn an_unknown_block_name_disables_the_mod_even_if_caught() {
        let temp = TempMod::new("unknown");
        scripted(
            &temp,
            "careless",
            READ_WRITE,
            r#"vox.on("block_broken", function(event, world)
                 world:set_block(1, 0, 0, "test:gold")
                 print(pcall(world.set_block, world, 1, 0, 0, "test:nope"))
                 -- After the mistake the world refuses everything.
                 print(pcall(world.get_block, world, 1, 0, 0))
               end)"#,
        );
        scripted(
            &temp,
            "fine",
            READ_WRITE,
            r#"vox.on("block_broken", function(event, world)
                 world:set_block(2, 0, 0, "test:sand")
               end)"#,
        );
        let mut host = start(&temp, &["careless", "fine"]);
        let registry = registry();
        let edits = fire_at_origin(&mut host, &registry);
        // The careless mod's write is dropped; the next mod still runs.
        assert_eq!(edits, vec![edit([2, 0, 0], block(&registry, "test:sand"))]);
        let messages = host.take_messages();
        let failure = ModFailure::UnknownBlock {
            name: "test:nope".to_owned(),
        };
        assert!(
            messages.contains(&disabled("careless", failure.clone())),
            "{messages:?}"
        );
        let refusals = messages
            .iter()
            .filter(|message| {
                matches!(&message.kind, MessageKind::Log(line)
                    if line.starts_with("false\t") && line.contains("unknown block `test:nope`"))
            })
            .count();
        assert_eq!(refusals, 2, "{messages:?}");
        assert!(!host.is_enabled(&id("careless")));
        assert!(host.is_enabled(&id("fine")));
    }

    #[test]
    fn too_many_writes_disable_the_mod() {
        let temp = TempMod::new("flood");
        let writes = |count: usize| {
            format!(
                r#"vox.on("block_broken", function(event, world)
                     -- Writes to unloaded chunks queue nothing and do not count.
                     for i = 1, 10 do world:set_block(0, 1000, 0, "test:gold") end
                     for i = 1, {count} do world:set_block(i % 16, 0, 0, "test:gold") end
                   end)"#
            )
        };
        scripted(&temp, "exact", READ_WRITE, &writes(MAX_EDITS_PER_CALL));
        scripted(&temp, "flood", READ_WRITE, &writes(MAX_EDITS_PER_CALL + 1));
        let mut host = start(&temp, &["exact", "flood"]);
        let registry = registry();
        let edits = fire_at_origin(&mut host, &registry);
        assert_eq!(edits.len(), MAX_EDITS_PER_CALL);
        assert_eq!(
            host.take_messages(),
            vec![disabled(
                "flood",
                ModFailure::TooManyEdits {
                    limit: MAX_EDITS_PER_CALL
                }
            )]
        );
    }

    #[test]
    fn a_handle_kept_past_its_call_is_unusable() {
        let temp = TempMod::new("kept");
        scripted(
            &temp,
            "keeper",
            READ_WRITE,
            r#"local kept
               vox.on("block_broken", function(event, world)
                 if kept == nil then kept = world return end
                 print(pcall(kept.get_block, kept, 0, 0, 0))
                 print(world:get_block(0, 0, 0))
                 kept:set_block(0, 0, 0, "test:gold")
               end)"#,
        );
        let mut host = start(&temp, &["keeper"]);
        let registry = registry();
        assert!(fire_at_origin(&mut host, &registry).is_empty());
        assert!(fire_at_origin(&mut host, &registry).is_empty());
        let messages = host.take_messages();
        let [first, second, third] = messages.as_slice() else {
            panic!("{messages:?}");
        };
        assert!(
            matches!(&first.kind, MessageKind::Log(line)
                if line.starts_with("false\t") && line.contains("destructed")),
            "{first:?}"
        );
        assert_eq!(second, &log("keeper", "core:air"), "the new handle works");
        assert!(
            matches!(&third.kind, MessageKind::Disabled(ModFailure::Runtime(message))
                if message.contains("destructed")),
            "{third:?}"
        );
    }

    #[test]
    fn a_world_function_needs_its_declared_capability() {
        let temp = TempMod::new("capabilities");
        let call = |what: &str| {
            format!(
                r#"vox.on("block_broken", function(event, world)
                     print(pcall(function() return {what} end))
                   end)"#
            )
        };
        scripted(
            &temp,
            "blind",
            &["world.write"],
            &call("world:get_block(0, 0, 0)"),
        );
        scripted(
            &temp,
            "frozen",
            &["world.read"],
            &call(r#"world:set_block(0, 0, 0, "test:gold")"#),
        );
        scripted(
            &temp,
            "looker",
            &["world.read"],
            &call("world:get_block(0, 0, 0)"),
        );
        let mut host = start(&temp, &["blind", "frozen", "looker"]);
        let registry = registry();
        assert!(fire_at_origin(&mut host, &registry).is_empty());
        let messages = host.take_messages();
        let read = ModFailure::MissingCapability {
            function: "get_block",
            capability: Capability::WorldRead,
        };
        let write = ModFailure::MissingCapability {
            function: "set_block",
            capability: Capability::WorldWrite,
        };
        assert!(read.to_string().contains("`world.read`"), "{read}");
        assert!(write.to_string().contains("`world.write`"), "{write}");
        assert!(messages.contains(&disabled("blind", read)), "{messages:?}");
        assert!(
            messages.contains(&disabled("frozen", write)),
            "{messages:?}"
        );
        assert!(
            messages.contains(&log("looker", "true\tcore:air")),
            "{messages:?}"
        );
        assert!(host.is_enabled(&id("looker")));
    }

    #[test]
    fn bad_arguments_are_ordinary_errors_in_script_terms() {
        let temp = TempMod::new("arguments");
        scripted(
            &temp,
            "sloppy",
            READ_WRITE,
            r#"vox.on("block_broken", function(event, world)
                 for _, call in ipairs({
                   function() return world:get_block(0.5, 0, 0) end,
                   function() return world:get_block(2^31, 0, 0) end,
                   function() return world:get_block(0, "1", 0) end,
                   function() return world:get_block(0, 0) end,
                   function() return world.get_block(0, 0, 0) end,
                   function() return world:set_block(0, 0, 0, 7) end,
                 }) do
                   print(pcall(call))
                 end
                 print(world:get_block(-2^31, 0, 0) == nil, world:get_block(2.0, 0, 0))
               end)"#,
        );
        let mut host = start(&temp, &["sloppy"]);
        let registry = registry();
        assert!(fire_at_origin(&mut host, &registry).is_empty());
        let messages = host.take_messages();
        let lines: Vec<&str> = messages
            .iter()
            .map(|message| match &message.kind {
                MessageKind::Log(line) => line.as_str(),
                other => panic!("{other:?}"),
            })
            .collect();
        let expected = [
            "x must be an integer from -2147483648 to 2147483647",
            "x must be an integer",
            "y must be an integer",
            "z must be an integer",
            "with a colon",
            "the block name must be a string",
        ];
        assert_eq!(lines.len(), expected.len() + 1, "{lines:?}");
        for (line, wanted) in lines.iter().zip(expected) {
            assert!(
                line.starts_with("false\t") && line.contains(wanted),
                "{line} lacks {wanted}"
            );
            for rust in ["i32", "i64", "f64", "Value", "mlua", "BlockId"] {
                assert!(!line.contains(rust), "{line} names {rust}");
            }
        }
        assert_eq!(lines[expected.len()], "true\tcore:air");
        assert!(
            host.is_enabled(&id("sloppy")),
            "argument errors can be caught"
        );
    }

    #[test]
    fn world_functions_are_charged() {
        // A loop iteration is about six or seven Lua instructions (`SELF`, the loads, `CALL`,
        // `FORLOOP`), so these loops alone fit the budget (at most 7/13 of it); with the charge
        // of each call (one per argument, the lookup, one per result: 9) they do not. Without
        // the charge, or without `WORLD_LOOKUP_INSTRUCTIONS` (4), they fit again.
        const CALLS: u64 = MAX_INSTRUCTIONS_PER_CALL / 13;
        let temp = TempMod::new("charged");
        let looping = |call: &str| {
            format!(
                r#"vox.on("block_broken", function(event, world)
                     for i = 1, {CALLS} do {call} end
                   end)"#
            )
        };
        scripted(
            &temp,
            "reads",
            &["world.read"],
            &looping("world:get_block(0, 0, 0)"),
        );
        scripted(
            &temp,
            "writes",
            &["world.write"],
            &looping(r#"world:set_block(0, 1000, 0, "test:gold")"#),
        );
        for id in ["reads", "writes"] {
            let mut host = start(&temp, &[id]);
            let edits = fire_at_origin(&mut host, &registry());
            assert!(edits.is_empty());
            assert_eq!(
                host.take_messages(),
                vec![disabled(
                    id,
                    ModFailure::InstructionBudget {
                        limit: MAX_INSTRUCTIONS_PER_CALL
                    }
                )],
                "{id}"
            );
        }
    }

    #[test]
    fn a_spent_budget_wins_over_a_recorded_mistake() {
        let temp = TempMod::new("spent");
        scripted(
            &temp,
            "both",
            READ_WRITE,
            r#"vox.on("block_broken", function(event, world)
                 pcall(world.set_block, world, 0, 0, 0, "x:nope")
                 while true do end
               end)"#,
        );
        let mut host = start(&temp, &["both"]);
        fire_at_origin(&mut host, &registry());
        assert_eq!(
            host.take_messages(),
            vec![disabled(
                "both",
                ModFailure::InstructionBudget {
                    limit: MAX_INSTRUCTIONS_PER_CALL
                }
            )]
        );
    }

    #[test]
    fn an_unknown_block_name_is_clean_and_cut() {
        let temp = TempMod::new("names");
        let names = [
            ("zero_byte", r#""x:\0y""#),
            ("escape_code", r#""x:\27[31mred""#),
            ("huge", r#"string.rep(string.rep("a", 1000), 1000)"#),
        ];
        for (id, name) in names {
            scripted(
                &temp,
                id,
                READ_WRITE,
                &format!(
                    r#"vox.on("block_broken", function(event, world)
                         world:set_block(0, 0, 0, {name})
                       end)"#
                ),
            );
        }
        for (id, _) in names {
            let mut host = start(&temp, &[id]);
            fire_at_origin(&mut host, &registry());
            let messages = host.take_messages();
            let [
                ModMessage {
                    kind: MessageKind::Disabled(failure @ ModFailure::UnknownBlock { name }),
                    ..
                },
            ] = messages.as_slice()
            else {
                panic!("{id}: {messages:?}");
            };
            assert!(name.len() <= MAX_FAILURE_MESSAGE_BYTES, "{id}");
            for text in [name.as_str(), &failure.to_string()] {
                assert!(!text.chars().any(char::is_control), "{id}: {text:?}");
            }
            if id == "huge" {
                assert_eq!(name.len(), MAX_FAILURE_MESSAGE_BYTES);
            }
        }
    }

    #[test]
    fn writes_past_the_event_total_are_dropped_not_fatal() {
        let temp = TempMod::new("total");
        let writer = |count: usize| {
            format!(
                r#"vox.on("block_broken", function(event, world)
                     for i = 1, {count} do world:set_block(i % 16, 0, 0, "test:gold") end
                   end)"#
            )
        };
        // Four mods fill the total except for four writes, the fifth wants ten.
        let counts = [
            MAX_EDITS_PER_CALL,
            MAX_EDITS_PER_CALL,
            MAX_EDITS_PER_CALL,
            MAX_EDITS_PER_CALL - 4,
            10,
        ];
        assert_eq!(counts.iter().sum::<usize>() - 10 + 4, MAX_EDITS_PER_EVENT);
        let ids = ["w1", "w2", "w3", "w4", "w5"];
        for (id, count) in ids.iter().zip(counts) {
            scripted(&temp, id, READ_WRITE, &writer(count));
        }
        let mut host = start(&temp, &ids);
        let edits = fire_at_origin(&mut host, &registry());
        assert_eq!(edits.len(), MAX_EDITS_PER_EVENT);
        let messages = host.take_messages();
        let [dropped] = messages.as_slice() else {
            panic!("{messages:?}");
        };
        assert_eq!(dropped.id, id("w5"));
        assert!(
            matches!(&dropped.kind, MessageKind::Log(line) if line.starts_with("6 block writes dropped")),
            "{dropped:?}"
        );
        assert!(host.is_enabled(&id("w5")), "dropping disables nothing");
    }

    #[test]
    fn an_event_nobody_handles_calls_nobody() {
        let (mut host, _) = ModHost::start(&mods(&["hello"]));
        host.take_messages();
        assert!(fire(&mut host, BlockEventKind::BlockPlaced).is_empty());
        assert!(host.take_messages().is_empty());
        // An event of a block the registry does not know is not sent either.
        let unknown = event(BlockEventKind::BlockBroken, [0, 0, 0], BlockId(999));
        assert!(
            host.block_event(&unknown, &World::new(), &BlockRegistry::new())
                .is_empty()
        );
        assert!(host.take_messages().is_empty());
    }
}
