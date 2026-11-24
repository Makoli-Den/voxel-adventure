//! The mod API v1 as scripts see it: the `vox` table and `print` (decision 0014, section c).
//!
//! `vox.on(event, handler)` registers a handler, `vox.api_version` is the API version as text,
//! and `print` queues a line into the mod's bounded log. Handlers get the event table and the
//! world handle of `world_handle`. Every change here or there is an API change and updates
//! `docs/mod-api.md` and [`MOD_API_VERSION`].

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use mlua::{Function, Lua, Value, Variadic};

use crate::budget::Budget;
use crate::manifest::MOD_API_VERSION;
use crate::sandbox::{Log, MAX_LOG_LINE_BYTES};
use crate::text::MAX_UTF8_CHAR_BYTES;

/// The global table that holds the API.
const API_TABLE: &str = "vox";

/// The field of [`API_TABLE`] with the API version, `MAJOR.MINOR`.
const API_VERSION_FIELD: &str = "api_version";

/// The function of [`API_TABLE`] that registers a handler.
const ON_FUNCTION: &str = "on";

/// The global replaced by the host's logging `print`.
const PRINT_FUNCTION: &str = "print";

/// The global `print` uses to turn a value into text, as Lua's own `print` does.
const TOSTRING_FUNCTION: &str = "tostring";

/// Separates the values of one `print` call, as Lua's own `print` does.
const PRINT_SEPARATOR: char = '\t';

/// An event a mod can handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockEventKind {
    /// The player broke a block.
    BlockBroken,
    /// The player placed a block.
    BlockPlaced,
}

impl BlockEventKind {
    /// Every event, in a fixed order.
    pub const ALL: [Self; 2] = [Self::BlockBroken, Self::BlockPlaced];

    /// The name used in `vox.on`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::BlockBroken => "block_broken",
            Self::BlockPlaced => "block_placed",
        }
    }

    /// The event called `name`, if there is one.
    pub fn from_name(name: &[u8]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.name().as_bytes() == name)
    }

    /// The position in [`Self::ALL`].
    const fn index(self) -> usize {
        match self {
            Self::BlockBroken => 0,
            Self::BlockPlaced => 1,
        }
    }
}

impl fmt::Display for BlockEventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The handlers one mod registered: at most one per event, and only while the entry script
/// runs, so the set of handled events is fixed after startup.
#[derive(Debug, Default)]
pub(crate) struct Handlers {
    by_kind: [Option<Function>; BlockEventKind::ALL.len()],
    /// Whether `vox.on` is allowed now: only while the entry script runs.
    open: bool,
}

impl Handlers {
    /// Allows or forbids `vox.on`.
    pub(crate) fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// Registers `handler` for `kind`. An error after the entry script, or for a second
    /// handler of the same event.
    fn register(&mut self, kind: BlockEventKind, handler: Function) -> mlua::Result<()> {
        if !self.open {
            return Err(mlua::Error::runtime(
                "`vox.on` may only be called while the entry script runs",
            ));
        }
        let slot = &mut self.by_kind[kind.index()];
        if slot.is_some() {
            return Err(mlua::Error::runtime(format!(
                "a handler for `{kind}` is already registered"
            )));
        }
        *slot = Some(handler);
        Ok(())
    }

    /// Whether a handler for `kind` is registered.
    pub(crate) fn contains(&self, kind: BlockEventKind) -> bool {
        self.by_kind[kind.index()].is_some()
    }

    /// The handler for `kind`, if one is registered.
    pub(crate) fn get(&self, kind: BlockEventKind) -> Option<Function> {
        self.by_kind[kind.index()].clone()
    }
}

/// Puts the `vox` table and the logging `print` into the globals of `lua`. Call after the
/// library is restricted: `print` keeps the `tostring` it finds now, so a script that
/// replaces `tostring` does not change what `print` does.
pub(crate) fn install(
    lua: &Lua,
    handlers: &Rc<RefCell<Handlers>>,
    log: &Rc<RefCell<Log>>,
    budget: &Rc<Budget>,
) -> mlua::Result<()> {
    let vox = lua.create_table()?;
    vox.raw_set(API_VERSION_FIELD, MOD_API_VERSION.to_string())?;
    vox.raw_set(ON_FUNCTION, on_function(lua, handlers)?)?;
    let globals = lua.globals();
    globals.raw_set(API_TABLE, vox)?;
    let tostring: Function = globals.raw_get(TOSTRING_FUNCTION)?;
    globals.raw_set(PRINT_FUNCTION, print_function(lua, log, budget, tostring)?)?;
    Ok(())
}

/// `vox.on(event, handler)`.
fn on_function(lua: &Lua, handlers: &Rc<RefCell<Handlers>>) -> mlua::Result<Function> {
    let handlers = Rc::clone(handlers);
    lua.create_function(move |_, (name, handler): (mlua::String, Function)| {
        let name = name.as_bytes();
        let kind = BlockEventKind::from_name(&name).ok_or_else(|| {
            mlua::Error::runtime(format!(
                "unknown event `{}`; known events: {}",
                String::from_utf8_lossy(&name),
                known_events()
            ))
        })?;
        handlers.borrow_mut().register(kind, handler)
    })
}

/// The event names, comma-separated, for error messages.
fn known_events() -> String {
    BlockEventKind::ALL.map(BlockEventKind::name).join(", ")
}

/// `print(...)`: like Lua's, but the line goes to the mod's log instead of the console.
///
/// The line stops growing at [`MAX_LOG_LINE_BYTES`]: later values are not even converted
/// (no `__tostring` runs for them), and of each string only the prefix that can still fit is
/// copied, so printing a long string costs the line, not the string.
fn print_function(
    lua: &Lua,
    log: &Rc<RefCell<Log>>,
    budget: &Rc<Budget>,
    tostring: Function,
) -> mlua::Result<Function> {
    let log = Rc::clone(log);
    let budget = Rc::clone(budget);
    lua.create_function(move |lua, values: Variadic<Value>| {
        print_line(&log, &budget, &tostring, &values).map_err(|error| budget.fail(lua, error))
    })
}

/// The body of `print`: charges one per value (`mlua` has already copied them; that copy is
/// charged here, after the fact), then builds and stores the line.
fn print_line(
    log: &RefCell<Log>,
    budget: &Budget,
    tostring: &Function,
    values: &[Value],
) -> mlua::Result<()> {
    // `__tostring` may print again; the depth limit bounds that recursion.
    let _level = budget.enter()?;
    budget.charge(u64::try_from(values.len()).unwrap_or(u64::MAX))?;
    if !log.borrow_mut().admit_line() {
        return Ok(());
    }
    // No borrow is held while formatting: `__tostring` runs Lua code, which may print.
    let mut line = String::new();
    for (index, value) in values.iter().enumerate() {
        if line.len() >= MAX_LOG_LINE_BYTES {
            break;
        }
        if index > 0 {
            line.push(PRINT_SEPARATOR);
        }
        let text: mlua::String = match value {
            Value::String(text) => text.clone(),
            other => tostring.call(other.clone())?,
        };
        let bytes = text.as_bytes();
        // A character cut at the end of the copy turns into a replacement character, which
        // lies past the room and is cut away again by `push_line`.
        let room = MAX_LOG_LINE_BYTES.saturating_sub(line.len()) + MAX_UTF8_CHAR_BYTES - 1;
        line.push_str(&String::from_utf8_lossy(&bytes[..bytes.len().min(room)]));
    }
    log.borrow_mut().push_line(&line);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ModFailure;
    use crate::sandbox::{CallLimits, Sandbox};

    /// Runs `source` as an entry script, where `vox.on` is allowed.
    fn load(sandbox: &Sandbox, source: &str) -> Result<(), ModFailure> {
        sandbox.exec(source.as_bytes(), "=test")
    }

    #[test]
    fn event_names_round_trip() {
        for kind in BlockEventKind::ALL {
            assert_eq!(
                BlockEventKind::from_name(kind.name().as_bytes()),
                Some(kind)
            );
            assert_eq!(BlockEventKind::ALL[kind.index()], kind);
        }
        assert_eq!(BlockEventKind::from_name(b"block_eaten"), None);
    }

    #[test]
    fn api_version_is_the_game_s() {
        let sandbox = Sandbox::new().unwrap();
        let version: String = sandbox
            .run(CallLimits::HANDLER, |lua| {
                lua.load("return vox.api_version").eval()
            })
            .unwrap();
        assert_eq!(version, MOD_API_VERSION.to_string());
    }

    #[test]
    fn on_registers_one_handler_per_event() {
        let sandbox = Sandbox::new().unwrap();
        load(
            &sandbox,
            "vox.on('block_broken', function() end) vox.on('block_placed', function() end)",
        )
        .unwrap();
        let handlers = sandbox.handlers().borrow();
        assert!(handlers.get(BlockEventKind::BlockBroken).is_some());
        assert!(handlers.get(BlockEventKind::BlockPlaced).is_some());
    }

    #[test]
    fn on_works_only_while_the_entry_script_runs() {
        let sandbox = Sandbox::new().unwrap();
        load(
            &sandbox,
            "late = function() vox.on('block_placed', print) end",
        )
        .unwrap();
        let result = sandbox.run(CallLimits::HANDLER, |lua| {
            lua.globals().get::<Function>("late")?.call::<()>(())
        });
        assert!(
            matches!(&result, Err(ModFailure::Runtime(message))
                if message.contains("only be called while the entry script runs")),
            "{result:?}"
        );
        assert!(
            !sandbox
                .handlers()
                .borrow()
                .contains(BlockEventKind::BlockPlaced)
        );
    }

    #[test]
    fn an_unknown_event_is_an_error() {
        let sandbox = Sandbox::new().unwrap();
        let result = load(&sandbox, "vox.on('block_eaten', function() end)");
        assert!(
            matches!(&result, Err(ModFailure::Runtime(message))
                if message.contains("unknown event `block_eaten`")
                    && message.contains("block_broken, block_placed")),
            "{result:?}"
        );
    }

    #[test]
    fn a_second_handler_for_one_event_is_an_error() {
        let sandbox = Sandbox::new().unwrap();
        let result = load(
            &sandbox,
            "vox.on('block_placed', print) vox.on('block_placed', print)",
        );
        assert!(
            matches!(&result, Err(ModFailure::Runtime(message))
                if message.contains("`block_placed` is already registered")),
            "{result:?}"
        );
    }

    #[test]
    fn a_handler_must_be_a_function() {
        let sandbox = Sandbox::new().unwrap();
        assert!(matches!(
            load(&sandbox, "vox.on('block_broken', 42)"),
            Err(ModFailure::Runtime(_))
        ));
    }
}
