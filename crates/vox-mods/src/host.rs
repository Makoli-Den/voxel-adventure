//! [`ModHost`]: every running mod script, each in its own [`Sandbox`], and what they reported.
//!
//! A failing script disables only its own mod: its Lua state is dropped and the failure is
//! reported as data, never as a panic or an early return, so the other mods and the game go on
//! (decision 0014, section b). Data-only mods (no `entry`) have no state and are not listed.
//!
//! The entry script is read from the mod's own folder, not through the pack stack: another
//! pack may override a mod's textures, but never its code.

use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::{Component, Path};

use vox_resources::{PATH_SEPARATOR, ResourceId};

use crate::api::BlockEventKind;
use crate::discover::{ASSETS_DIR, DiscoveredMod};
use crate::error::ModFailure;
use crate::manifest::ModId;
use crate::sandbox::{LogEntry, Sandbox};

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
                    host.mods.push(RunningMod { id, sandbox });
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

    /// Calls the handler of `kind` of every running mod, in load order, with no arguments.
    /// A stand-in for the real event dispatch of step S5, so the handler budget can be tested.
    #[cfg(test)]
    fn dispatch_test_event(&mut self, kind: BlockEventKind) {
        let mut index = 0;
        while index < self.mods.len() {
            let running = &self.mods[index];
            let handler = running.sandbox.handlers().borrow().get(kind);
            let result = match handler {
                Some(handler) => running
                    .sandbox
                    .run(crate::sandbox::CallLimits::HANDLER, |_| {
                        handler.call::<()>(())
                    }),
                None => Ok(()),
            };
            collect_log(&running.id, &running.sandbox, &mut self.messages);
            match result {
                Ok(()) => index += 1,
                Err(failure) => {
                    let removed = self.mods.remove(index);
                    self.messages.push(ModMessage {
                        id: removed.id,
                        kind: MessageKind::Disabled(failure),
                    });
                }
            }
        }
    }
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
    use crate::sandbox::MAX_LOG_LINES_PER_CALL;
    use std::path::PathBuf;

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
        host.dispatch_test_event(BlockEventKind::BlockBroken);
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
        host.dispatch_test_event(BlockEventKind::BlockBroken);
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
        host.dispatch_test_event(BlockEventKind::BlockBroken);
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
        host.dispatch_test_event(BlockEventKind::BlockPlaced);
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
        host.dispatch_test_event(BlockEventKind::BlockBroken);
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
}
