//! The Lua sandbox: one Lua 5.4 state per mod with a cut-down standard library, a budget of
//! instructions and time, a memory cap and a bounded log (decision 0014, section b).
//!
//! What a script can reach is decided here and in [`library`](crate::library): the base
//! library minus the functions that load code, drive the collector or run unwrapped handlers;
//! a few `string` functions; `table`, `math` and `utf8`. There is no `io`, `os`, `package` (so
//! no `require`), `debug` or `coroutine`. Code is accepted as text only: precompiled bytecode
//! is the classic way out of a Lua sandbox.
//!
//! Every host call runs under a [`Budget`]: a count hook charges every
//! [`HOOK_INTERVAL`](crate::HOOK_INTERVAL) instructions and checks the clock, and library
//! functions charge for the work they do in C. A spent budget is sticky (see `budget`).
//! Outside a host call the budget is zero, and scripts cannot register finalizers, so no mod
//! code runs between calls or while a disabled mod's state is closed.
//!
//! **Known residual:** single VM instructions that are linear in a string's length (`..`,
//! `==` and `<` on two long strings; `#` is constant) are not charged by size. Their cost is
//! bounded by the memory cap, and the clock, checked every
//! [`HOOK_INTERVAL`](crate::HOOK_INTERVAL) instructions, stops a loop of them only at the
//! next check: the worst measured case, `<` on two 1.9 MB strings of `\0` bytes, stalled one
//! call for about 1.3 s in a release build before the mod was disabled
//! (`docs/benchmarks.md`).
//!
//! All limits are provisional and are fixed by measurement in step S7 on the low-end device.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use mlua::{ChunkMode, HookTriggers, Lua, LuaOptions, StdLib, VmState};

use crate::api::{self, Handlers};
use crate::budget::{Budget, HOOK_INTERVAL, MAX_INSTRUCTIONS_AT_LOAD, MAX_LOAD_WALL_TIME, Spent};
use crate::error::ModFailure;
use crate::library;
use crate::text::printable;

/// Memory cap of one mod's Lua state, 4 MiB. Enough for event handlers and their tables. It
/// is also what bounds single VM instructions on long strings (`..`, `<`, `==`), which the
/// instruction count does not weigh: building a string copies it, so the longest string a mod
/// can make is about half of this, and the cost of one such instruction grows with it
/// (measured in `docs/benchmarks.md`). Lowered from 16 MiB for that reason. Provisional (S7).
pub const MAX_MOD_MEMORY: usize = 4 * 1024 * 1024;

/// Lines a mod may `print` in one call; the rest are counted and dropped, so a chatty or
/// broken mod cannot flood the game's console. Provisional.
pub const MAX_LOG_LINES_PER_CALL: usize = 16;

/// Longest logged line in bytes; a longer line is cut at a character boundary. Without it,
/// [`MAX_LOG_LINES_PER_CALL`] lines of [`MAX_MOD_MEMORY`] each would pile up in the host
/// between two reads of the messages. Provisional.
pub const MAX_LOG_LINE_BYTES: usize = 1024;

/// The standard libraries opened besides `base`, which Lua always opens. A function because
/// `StdLib` has no `const` union.
fn libraries() -> StdLib {
    StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::UTF8
}

/// The limits of one kind of host call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CallLimits {
    /// Lua instructions, including the charges of library functions.
    pub(crate) instructions: u64,
    /// Wall-clock time.
    pub(crate) wall_time: Duration,
}

impl CallLimits {
    /// Running the entry script.
    pub(crate) const LOAD: Self = Self {
        instructions: MAX_INSTRUCTIONS_AT_LOAD,
        wall_time: MAX_LOAD_WALL_TIME,
    };

    /// One event handler call. Only the test dispatch calls handlers until step S5.
    #[cfg(test)]
    pub(crate) const HANDLER: Self = Self {
        instructions: crate::budget::MAX_INSTRUCTIONS_PER_CALL,
        wall_time: crate::budget::MAX_CALL_WALL_TIME,
    };

    /// The failure for a budget spent under these limits.
    fn failure(self, spent: Spent) -> ModFailure {
        match spent {
            Spent::Instructions => ModFailure::InstructionBudget {
                limit: self.instructions,
            },
            Spent::WallTime => ModFailure::TimeBudget {
                limit: self.wall_time,
            },
            Spent::Memory => ModFailure::Memory,
        }
    }
}

/// One entry of a mod's log, taken by the host and tagged with the mod's id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LogEntry {
    /// A line the mod printed, cut to [`MAX_LOG_LINE_BYTES`], without control characters.
    Line(String),
    /// How many lines of one call were dropped by [`MAX_LOG_LINES_PER_CALL`].
    Dropped(usize),
}

/// The lines a mod printed, with the per-call cap.
#[derive(Debug, Default)]
pub(crate) struct Log {
    entries: Vec<LogEntry>,
    lines_this_call: usize,
    dropped_this_call: usize,
}

impl Log {
    /// Whether one more line fits into the current call. If not, the line is counted as
    /// dropped. Asked before the line is built, so a dropped line costs no formatting.
    pub(crate) fn admit_line(&mut self) -> bool {
        if self.lines_this_call < MAX_LOG_LINES_PER_CALL {
            self.lines_this_call += 1;
            true
        } else {
            self.dropped_this_call += 1;
            false
        }
    }

    /// Stores an admitted line, cut to [`MAX_LOG_LINE_BYTES`], with control characters
    /// replaced so the line cannot pose as another line or move the cursor.
    pub(crate) fn push_line(&mut self, line: &str) {
        self.entries
            .push(LogEntry::Line(printable(line, MAX_LOG_LINE_BYTES)));
    }

    fn start_call(&mut self) {
        self.lines_this_call = 0;
        self.dropped_this_call = 0;
    }

    fn finish_call(&mut self) {
        if self.dropped_this_call > 0 {
            self.entries.push(LogEntry::Dropped(self.dropped_this_call));
            self.dropped_this_call = 0;
        }
    }
}

/// One mod's Lua state with its limits. Not `Send`: it lives on the main thread.
pub(crate) struct Sandbox {
    /// Shared with `vox.on` inside the state, so the handlers may be freed while the state
    /// closes; `mlua` values hold only a weak reference to their state, which makes that safe.
    handlers: Rc<RefCell<Handlers>>,
    lua: Lua,
    budget: Rc<Budget>,
    log: Rc<RefCell<Log>>,
}

impl Sandbox {
    /// A fresh state with the restricted library, the limits and the `vox` API. No mod code
    /// has run yet.
    pub(crate) fn new() -> mlua::Result<Self> {
        let lua = Lua::new_with(libraries(), LuaOptions::default())?;
        lua.set_memory_limit(MAX_MOD_MEMORY)?;
        let budget = Rc::new(Budget::default());
        library::restrict(&lua, &budget)?;
        let log = Rc::new(RefCell::new(Log::default()));
        let handlers = Rc::new(RefCell::new(Handlers::default()));
        api::install(&lua, &handlers, &log, &budget)?;

        let hook_budget = Rc::clone(&budget);
        lua.set_global_hook(
            HookTriggers::new().every_nth_instruction(HOOK_INTERVAL),
            move |lua, _| {
                hook_budget
                    .charge(u64::from(HOOK_INTERVAL))
                    .map(|()| VmState::Continue)
                    .map_err(|error| hook_budget.fail(lua, error))
            },
        )?;
        Ok(Self {
            handlers,
            lua,
            budget,
            log,
        })
    }

    /// Runs the entry script: the text chunk `source` named `name`, under the load limits.
    /// `vox.on` works only during this call.
    ///
    /// # Errors
    /// The [`ModFailure`] that disables the mod.
    pub(crate) fn exec(&self, source: &[u8], name: &str) -> Result<(), ModFailure> {
        self.handlers.borrow_mut().set_open(true);
        let result = self.run(CallLimits::LOAD, |lua| {
            lua.load(source)
                .set_name(name)
                .set_mode(ChunkMode::Text)
                .exec()
        });
        self.handlers.borrow_mut().set_open(false);
        result
    }

    /// Runs `call` as one host call under `limits`: refuses to start if the budget is already
    /// spent, and treats the call as failed if it spent the budget, even if the script caught
    /// the error.
    ///
    /// # Errors
    /// The [`ModFailure`] that disables the mod.
    pub(crate) fn run<R>(
        &self,
        limits: CallLimits,
        call: impl FnOnce(&Lua) -> mlua::Result<R>,
    ) -> Result<R, ModFailure> {
        if let Some(spent) = self.budget.spent() {
            return Err(limits.failure(spent));
        }
        self.log.borrow_mut().start_call();
        self.budget.open(limits.instructions, limits.wall_time);
        let result = call(&self.lua);
        self.budget.close();
        self.log.borrow_mut().finish_call();
        if let Some(spent) = self.budget.spent() {
            return Err(limits.failure(spent));
        }
        result.map_err(|error| ModFailure::from_lua(&error))
    }

    /// The handlers the script registered.
    pub(crate) fn handlers(&self) -> &RefCell<Handlers> {
        &self.handlers
    }

    /// The log lines and drop counts since the last call of this function.
    pub(crate) fn take_log(&self) -> Vec<LogEntry> {
        std::mem::take(&mut self.log.borrow_mut().entries)
    }

    /// Bytes the Lua state uses now, as counted by the allocator.
    pub(crate) fn used_memory(&self) -> usize {
        self.lua.used_memory()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    use super::*;
    use crate::budget::{MAX_CALL_WALL_TIME, MAX_HOST_CALL_DEPTH, MAX_INSTRUCTIONS_PER_CALL};

    fn sandbox() -> Sandbox {
        Sandbox::new().unwrap()
    }

    /// Runs `source` as a handler call and returns what it evaluates to.
    fn eval<R: mlua::FromLuaMulti>(sandbox: &Sandbox, source: &str) -> Result<R, ModFailure> {
        eval_with(sandbox, CallLimits::HANDLER, source)
    }

    /// Runs `source` under `limits` and returns what it evaluates to.
    fn eval_with<R: mlua::FromLuaMulti>(
        sandbox: &Sandbox,
        limits: CallLimits,
        source: &str,
    ) -> Result<R, ModFailure> {
        sandbox.run(limits, |lua| {
            lua.load(source).set_mode(ChunkMode::Text).eval()
        })
    }

    /// Runs `body` on a new thread with a stack of `stack` bytes and waits for it at most
    /// `timeout`. `None` if it did not finish in time.
    fn on_thread<T: Send + 'static>(
        stack: usize,
        timeout: Duration,
        body: impl FnOnce() -> T + Send + 'static,
    ) -> Option<T> {
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .stack_size(stack)
            .spawn(move || {
                let _ = sender.send(body());
            })
            .unwrap();
        receiver.recv_timeout(timeout).ok()
    }

    /// The main thread stack of a Windows program, the smallest the game runs on.
    const MAIN_THREAD_STACK: usize = 1024 * 1024;

    #[test]
    fn unsafe_libraries_and_functions_are_absent() {
        let sandbox = sandbox();
        for name in [
            "io",
            "os",
            "debug",
            "package",
            "coroutine",
            "require",
            "load",
            "loadfile",
            "dofile",
            "collectgarbage",
            "xpcall",
            "warn",
            "loadstring",
            "module",
            "string.dump",
            "string.match",
            "string.gmatch",
            "string.gsub",
            "string.pack",
            "string.unpack",
            "string.packsize",
            // Through the metatable of a string value, which indexes the same table.
            "('x').dump",
            "('x').gsub",
        ] {
            let kind: String = eval(&sandbox, &format!("return type({name})")).unwrap();
            assert_eq!(kind, "nil", "{name}");
        }
    }

    #[test]
    fn safe_libraries_are_present() {
        let sandbox = sandbox();
        let result: String = eval(
            &sandbox,
            "return string.upper('a') .. table.concat({1, 2}) .. math.floor(3.5) .. utf8.char(66) \
             .. tostring(pcall(error)) .. type(setmetatable) .. ('ab'):rep(2, '-') \
             .. ('abc'):sub(2):reverse() .. string.format('%d', 7) .. string.byte('A')",
        )
        .unwrap();
        assert_eq!(result, "A123Bfalsefunctionab-abcb765");
    }

    #[test]
    fn a_precompiled_chunk_is_refused() {
        // Bytecode made by an unrestricted state, which still has `string.dump`.
        let bytecode = Lua::new()
            .load("return 42")
            .into_function()
            .unwrap()
            .dump(false);
        let sandbox = sandbox();
        let result = sandbox.exec(&bytecode, "=bytecode");
        assert!(
            matches!(&result, Err(ModFailure::Syntax(message)) if message.contains("binary")),
            "{result:?}"
        );
    }

    #[test]
    fn an_endless_loop_is_stopped() {
        let sandbox = sandbox();
        let result = eval::<()>(&sandbox, "while true do end");
        assert_eq!(
            result,
            Err(ModFailure::InstructionBudget {
                limit: MAX_INSTRUCTIONS_PER_CALL
            })
        );
    }

    #[test]
    fn the_budget_is_sticky_across_pcall() {
        let sandbox = sandbox();
        // Every check lands inside the inner call; only the wrapped pcall stops the outer loop.
        let result = eval::<()>(
            &sandbox,
            "while true do pcall(function() while true do end end) end",
        );
        assert!(
            matches!(result, Err(ModFailure::InstructionBudget { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn a_spent_state_refuses_every_later_call() {
        let sandbox = sandbox();
        // The script catches the budget error once and returns normally.
        let caught = eval::<bool>(&sandbox, "return pcall(function() while true do end end)");
        assert!(
            matches!(caught, Err(ModFailure::InstructionBudget { .. })),
            "{caught:?}"
        );
        let later = eval::<i64>(&sandbox, "return 1");
        assert!(
            matches!(later, Err(ModFailure::InstructionBudget { .. })),
            "{later:?}"
        );
    }

    #[test]
    fn a_short_call_stays_within_budget_and_the_budget_renews() {
        let sandbox = sandbox();
        // About two instructions per iteration (`ADD`, `FORLOOP`), so each call uses about a
        // quarter of its budget and the sixteen calls together about four budgets.
        let loop_size = MAX_INSTRUCTIONS_PER_CALL / 8;
        for _ in 0..16 {
            let sum: i64 = eval(
                &sandbox,
                &format!("local s = 0 for i = 1, {loop_size} do s = s + 1 end return s"),
            )
            .unwrap();
            assert_eq!(sum, i64::try_from(loop_size).unwrap());
        }
    }

    #[test]
    fn a_memory_bomb_hits_the_cap() {
        let sandbox = sandbox();
        let result = eval_with::<()>(
            &sandbox,
            CallLimits::LOAD,
            "local t = {} for i = 1, 1e9 do t[i] = string.rep('x', 65536) .. i end",
        );
        assert_eq!(result, Err(ModFailure::Memory));
        assert!(sandbox.used_memory() <= MAX_MOD_MEMORY);
    }

    /// Slots of 16 bytes that fill `share` of the memory cap.
    fn slots(share: f64) -> usize {
        (MAX_MOD_MEMORY as f64 * share) as usize / 16
    }

    /// Bytes that fill `share` of the memory cap.
    fn bytes(share: f64) -> usize {
        (MAX_MOD_MEMORY as f64 * share) as usize
    }

    #[test]
    fn long_strings_and_tables_made_in_c_count_toward_memory() {
        // Under the load budget, so memory, not the instruction charge, is what stops them.
        let cases = [
            format!("return #string.rep('x', {})", 2 * MAX_MOD_MEMORY),
            format!(
                "return #table.concat({{string.rep('y', {half}), string.rep('z', {half})}})",
                half = MAX_MOD_MEMORY / 2
            ),
            // An array of numbers filling 3/4 of the cap, built by Lua, then copied by
            // `table.move` into a table that grows in C past the cap.
            format!(
                "local a = {{}} for i = 1, {n} do a[i] = i end \
                 return #table.move(a, 1, #a, 1, {{}})",
                n = slots(0.75)
            ),
        ];
        for source in cases {
            let sandbox = sandbox();
            let result = eval_with::<i64>(&sandbox, CallLimits::LOAD, &source);
            assert_eq!(result, Err(ModFailure::Memory), "{source}");
        }
        // The same path at a size below the cap works, so the failure above is the cap.
        let sandbox = sandbox();
        let n = slots(0.125);
        let moved: i64 = eval_with(
            &sandbox,
            CallLimits::LOAD,
            &format!(
                "local a = {{}} for i = 1, {n} do a[i] = i end return #table.move(a, 1, #a, 1, {{}})"
            ),
        )
        .unwrap();
        assert_eq!(moved, i64::try_from(n).unwrap());
    }

    #[test]
    fn a_caught_memory_error_still_disables_the_mod() {
        let piece = bytes(0.4);
        let cases = [
            // In a library function: the charged wrapper notes it.
            format!(
                "local hold = string.rep('x', {piece}) \
                 while true do pcall(string.rep, 'x', {piece}) end"
            ),
            // In plain Lua: pcall sees Lua's memory error message.
            "while true do pcall(function() local t = {} for i = 1, 1e9 do t[i] = i end end) end"
                .to_owned(),
        ];
        for source in cases {
            let sandbox = sandbox();
            let result = eval_with::<()>(&sandbox, CallLimits::LOAD, &source);
            assert_eq!(result, Err(ModFailure::Memory), "{source}");
            assert_eq!(eval::<i64>(&sandbox, "return 1"), Err(ModFailure::Memory));
        }
    }

    /// The integer global `name` of a state whose budget is spent. Every Lua call fails in
    /// such a state, so the hook is removed first; tests only.
    fn global_after_failure(sandbox: &Sandbox, name: &str) -> i64 {
        sandbox.lua.remove_global_hook();
        sandbox.lua.globals().raw_get(name).unwrap()
    }

    /// Runs `call` in a loop as one handler call after `setup` ran at load, and returns how
    /// many times it completed (`n`) and how long the call took. The loop must end on a spent
    /// budget.
    fn count_calls(setup: &str, call: &str) -> (i64, Duration) {
        let sandbox = sandbox();
        eval_with::<()>(&sandbox, CallLimits::LOAD, setup).unwrap();
        let source = format!("n = 0 for i = 1, 1e9 do {call} n = n + 1 end");
        let start = Instant::now();
        let result = eval::<()>(&sandbox, &source);
        let elapsed = start.elapsed();
        assert!(
            matches!(
                result,
                Err(ModFailure::InstructionBudget { .. } | ModFailure::TimeBudget { .. })
            ),
            "{call}: {result:?}"
        );
        (global_after_failure(&sandbox, "n"), elapsed)
    }

    #[test]
    fn library_work_in_c_is_charged() {
        // Loops of calls that each do C work: the loop stops after about budget / charge
        // calls (`n` counts them). Without a charge it would run thousands of calls on the few
        // instructions of the loop itself, until the clock stopped it. Each case is sized so
        // one call is charged a few thousand instructions; removing its charge fails here.
        let text = bytes(0.4);
        let codes = "c = {} for i = 1, 2000 do c[i] = 65 end";
        let long = "s = string.rep('7', 65536)";
        for (setup, call, max_calls) in [
            (
                format!("big = string.rep('x', {text})"),
                "string.find(big, 'y')",
                4,
            ),
            (long.to_owned(), "s:upper()", 50),
            (long.to_owned(), "s:lower()", 50),
            (long.to_owned(), "s:reverse()", 50),
            (long.to_owned(), "s:sub(1, -1)", 50),
            (long.to_owned(), "s:len()", 100_000),
            (long.to_owned(), "string.format('%s', s)", 50),
            (long.to_owned(), "s:byte(1, 2000)", 50),
            (long.to_owned(), "tonumber(s)", 100),
            (long.to_owned(), "utf8.len(s)", 100),
            (long.to_owned(), "utf8.codepoint(s)", 100),
            (long.to_owned(), "utf8.offset(s, 1)", 100),
            (codes.to_owned(), "string.char(table.unpack(c))", 50),
            (codes.to_owned(), "utf8.char(table.unpack(c))", 50),
            (codes.to_owned(), "table.unpack(c)", 50),
            (codes.to_owned(), "table.pack(table.unpack(c))", 50),
            (
                "t = {} for i = 1, 1000 do t[i] = 'x' end".to_owned(),
                "table.concat(t)",
                200,
            ),
            (
                "t = {} for i = 1, 1000 do t[i] = i end".to_owned(),
                "table.sort(t)",
                20,
            ),
            (String::new(), "table.move({}, 1, 1e6, 1)", 0),
            (
                "t = {} for i = 1, 1000 do t[i] = i end".to_owned(),
                "table.insert(t, 1, 0) table.remove(t, 1)",
                100,
            ),
            // One step of the `codes` iterator skips a run of continuation bytes in C.
            (
                "s = 'a' .. string.rep('\\x80', 65536) step = utf8.codes(s)".to_owned(),
                "step(s, 1)",
                100,
            ),
        ] {
            let (calls, _) = count_calls(&setup, call);
            assert!(calls <= max_calls, "{call}: {calls} calls");
        }
    }

    #[test]
    fn huge_counts_are_refused_before_the_work() {
        for source in [
            "string.rep('x', 1e15)",
            "string.rep('', math.maxinteger)",
            "('').rep('', 1e15, '')",
            // Numbers in strings are converted by the C functions, and by the charge too.
            "string.rep('x', '7000000')",
            "table.move({}, '1', '1e15', 1)",
        ] {
            let sandbox = sandbox();
            let start = Instant::now();
            let result = eval::<()>(&sandbox, source);
            assert!(
                matches!(result, Err(ModFailure::InstructionBudget { .. })),
                "{source}: {result:?}"
            );
            assert!(
                start.elapsed() < MAX_CALL_WALL_TIME,
                "{source}: {:?}",
                start.elapsed()
            );
        }
    }

    #[test]
    fn table_functions_refuse_metamethods() {
        for source in [
            "table.insert(setmetatable({}, { __len = function() return 1 << 62 end }), 1, 0)",
            "table.unpack(setmetatable({}, { __len = function() return 1 << 62 end }))",
            "table.concat(setmetatable({}, { __index = function() return 'x' end }), '', 1, 1e8)",
            "table.sort(setmetatable({}, { __len = function() return 1e8 end, \
                                          __index = function(t, i) return -i end }))",
            "table.move({1, 2}, 1, 2, 1, setmetatable({}, { __newindex = function() end }))",
            // A string with a `__len` added to its metatable is no way around it.
            "getmetatable('').__len = function() return 1 << 62 end table.insert('x', 1, 0)",
        ] {
            let sandbox = sandbox();
            let start = Instant::now();
            let result = eval::<()>(&sandbox, source);
            assert!(
                matches!(&result, Err(ModFailure::Runtime(message)) if message.contains("table")),
                "{source}: {result:?}"
            );
            assert!(
                start.elapsed() < MAX_CALL_WALL_TIME,
                "{source}: {:?}",
                start.elapsed()
            );
        }
    }

    #[test]
    fn sorting_long_strings_is_charged_by_their_bytes() {
        // Strings full of `\0` bytes compare piece by piece with `strcoll`, which is slow.
        let sandbox = sandbox();
        let start = Instant::now();
        let result = eval::<()>(
            &sandbox,
            &format!(
                "local z = string.rep('a\\0', {}) local t = {{}} \
                 for i = 1, 64 do t[i] = z end table.sort(t)",
                bytes(0.0625)
            ),
        );
        assert!(
            matches!(result, Err(ModFailure::InstructionBudget { .. })),
            "{result:?}"
        );
        assert!(
            start.elapsed() < MAX_CALL_WALL_TIME,
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn many_values_are_charged_per_value() {
        // `print` with many values: after the line cap it stores nothing, but every call still
        // copies all values. About budget / values calls complete.
        let values: u64 = 16_384;
        let sandbox = sandbox();
        let start = Instant::now();
        let result = eval::<()>(
            &sandbox,
            &format!(
                "local t = {{}} for i = 1, {values} do t[i] = i end n = 0                  local function p(...) for i = 1, 1e9 do print(...) n = n + 1 end end                  p(table.unpack(t))"
            ),
        );
        assert!(
            matches!(
                result,
                Err(ModFailure::InstructionBudget { .. } | ModFailure::TimeBudget { .. })
            ),
            "{result:?}"
        );
        assert!(
            start.elapsed() < MAX_LOAD_WALL_TIME,
            "{:?}",
            start.elapsed()
        );
        let calls = global_after_failure(&sandbox, "n");
        let most = MAX_INSTRUCTIONS_PER_CALL / values;
        assert!(calls <= i64::try_from(most).unwrap(), "{calls} calls");
    }

    #[test]
    fn the_clock_stops_what_the_count_misses() {
        // `..` is one instruction however long its operands; the clock, checked with the
        // count, stops the loop.
        let sandbox = sandbox();
        eval_with::<()>(
            &sandbox,
            CallLimits::LOAD,
            &format!("s = string.rep('x', {})", bytes(0.125)),
        )
        .unwrap();
        let start = Instant::now();
        let result = eval::<()>(&sandbox, "while true do local t = s .. s end");
        assert_eq!(
            result,
            Err(ModFailure::TimeBudget {
                limit: MAX_CALL_WALL_TIME
            })
        );
        assert!(
            start.elapsed() < MAX_LOAD_WALL_TIME,
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn nothing_runs_after_the_budget_is_spent() {
        // When the endless loop is stopped, the error unwinds through a hundred `__close`
        // methods, each doing one slow comparison. Once the budget is spent the hook fires on
        // every instruction and call, so each method fails at once.
        let sandbox = sandbox();
        eval_with::<()>(
            &sandbox,
            CallLimits::LOAD,
            &format!("Z = string.rep('a\\0', {})", bytes(0.0625)),
        )
        .unwrap();
        let start = Instant::now();
        let result = eval::<()>(
            &sandbox,
            "local function f(n) \
               local guard <close> = setmetatable({}, { __close = function() local _ = Z < Z end }) \
               if n > 0 then f(n - 1) else while true do end end \
             end \
             f(100)",
        );
        assert!(
            matches!(result, Err(ModFailure::InstructionBudget { .. })),
            "{result:?}"
        );
        assert!(
            start.elapsed() < MAX_CALL_WALL_TIME,
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn find_searches_plain_text_only() {
        let sandbox = sandbox();
        let results: String = eval(
            &sandbox,
            "local function show(...) local a, b = ... \
                 return tostring(a) .. ',' .. tostring(b) .. '#' .. select('#', ...) end \
             return table.concat({ \
               show(string.find('a.b', '.')), show(('x%d'):find('%d')), \
               show(string.find('abcabc', 'c', -2)), show(string.find('abc', '', 10)), \
               show(string.find('abc', '', 4)), show(string.find('abc', 'z')), \
               show(string.find('abc', 'b', 1, false)) }, ' ')",
        )
        .unwrap();
        assert_eq!(results, "2,2#2 2,3#2 6,6#2 nil,nil#1 4,3#2 nil,nil#1 2,2#2");
    }

    #[test]
    fn format_widths_are_bounded_by_lua() {
        let sandbox = sandbox();
        let wide: i64 = eval(&sandbox, "return #string.format('%99d', 1)").unwrap();
        assert_eq!(wide, 99);
        for format in ["%99999d", "%100d", "%.100f", "%999s"] {
            let result = eval::<String>(&sandbox, &format!("return string.format('{format}', 1)"));
            assert!(
                matches!(&result, Err(ModFailure::Runtime(message)) if message.contains("invalid conversion")),
                "{format}: {result:?}"
            );
        }
    }

    #[test]
    fn finalizers_are_refused() {
        for meta in ["{ __gc = function() end }", "{ __gc = false }"] {
            let sandbox = sandbox();
            let result = eval::<()>(&sandbox, &format!("setmetatable({{}}, {meta})"));
            assert!(
                matches!(&result, Err(ModFailure::Runtime(message)) if message.contains("__gc")),
                "{meta}: {result:?}"
            );
        }
        // An ordinary metatable still works, and an error object of the host has a
        // protected metatable a script cannot edit.
        let sandbox = sandbox();
        let kind: String = eval(
            &sandbox,
            "local t = setmetatable({}, { __index = function() return 1 end }) \
             local ok, e = pcall(vox.on, 'nope', print) \
             return t.x .. type(e) .. type(getmetatable(e))",
        )
        .unwrap();
        assert_eq!(kind, "1userdataboolean");
    }

    #[test]
    fn closing_a_state_runs_no_mod_code() {
        // `__gc` added after `setmetatable` does not mark the object for finalization, so the
        // endless finalizers never run, not even when the state is closed.
        let finished = on_thread(MAIN_THREAD_STACK, Duration::from_secs(10), || {
            let sandbox = Sandbox::new().unwrap();
            eval_with::<()>(
                &sandbox,
                CallLimits::LOAD,
                "keep = {} for i = 1, 10000 do local mt = {} keep[i] = setmetatable({}, mt) \
                 mt.__gc = function() while true do end end end",
            )
            .unwrap();
            drop(sandbox);
        });
        assert!(finished.is_some(), "dropping the state hung");
    }

    #[test]
    fn deep_pcall_recursion_is_stopped_on_a_small_stack() {
        let result = on_thread(MAIN_THREAD_STACK, Duration::from_secs(10), || {
            let sandbox = Sandbox::new().unwrap();
            let outcome = eval::<mlua::MultiValue>(
                &sandbox,
                "local function f() return pcall(f) end return f()",
            )
            .map(|values| values.len());
            // Also through `print` and `__tostring`, and through a charged library function.
            let through_print = eval::<()>(
                &sandbox,
                "local t t = setmetatable({}, { __tostring = function() print(t) return '' end }) \
                 print(t)",
            );
            let through_sort = eval::<()>(
                &sandbox,
                "local function f(a, b) table.sort({2, 1}, f) return a < b end \
                 table.sort({2, 1}, f)",
            );
            // Lua's own limit of nested C calls, reached inside nested host functions.
            let index_loop =
                "local t = setmetatable({}, { __index = function(t, k) return t[k] end })";
            let under_pcalls = eval::<mlua::MultiValue>(
                &sandbox,
                &format!(
                    "{index_loop} local function f(n) if n == 0 then return t.x end                      return pcall(f, n - 1) end return f({})",
                    MAX_HOST_CALL_DEPTH - 1
                ),
            );
            let under_sort = eval::<()>(
                &sandbox,
                &format!("{index_loop} table.sort({{2, 1}}, function() return t.x end)"),
            );
            (
                outcome.is_ok(),
                through_print.is_err(),
                through_sort.is_err(),
                under_pcalls.is_ok(),
                under_sort.is_err(),
            )
        });
        assert_eq!(
            result,
            Some((true, true, true, true, true)),
            "stack overflow or hang"
        );
    }

    #[test]
    fn string_metatable_edits_stay_inside_the_state() {
        let first = sandbox();
        eval::<()>(
            &first,
            "getmetatable('').__index.upper = function() return 'hacked' end \
             getmetatable('').__index = { len = function() return -1 end }",
        )
        .unwrap();
        let hacked: i64 = eval(&first, "return ('abc'):len()").unwrap();
        assert_eq!(hacked, -1);
        let second = sandbox();
        let clean: String = eval(&second, "return ('abc'):upper() .. #'abc'").unwrap();
        assert_eq!(clean, "ABC3");
    }

    #[test]
    fn a_metatable_on_globals_is_harmless() {
        let sandbox = sandbox();
        eval::<()>(
            &sandbox,
            "setmetatable(_G, { __index = function() while true do end end, \
                               __newindex = function() error('no') end })",
        )
        .unwrap();
        // The host sets nothing through `_G` after setup, and `print` and `vox` are plain
        // fields, so they still work.
        eval::<()>(&sandbox, "print('still here', vox.api_version)").unwrap();
        let log = sandbox.take_log();
        assert_eq!(log, vec![LogEntry::Line("still here\t1.0".to_owned())]);
        // Reading a missing global now loops, which the budget stops as usual.
        let result = eval::<()>(&sandbox, "return missing");
        assert!(
            matches!(result, Err(ModFailure::InstructionBudget { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn a_runtime_error_carries_the_message() {
        let sandbox = sandbox();
        let result = sandbox.exec(b"error('broken on purpose')", "=demo:scripts/main.lua");
        assert!(
            matches!(&result, Err(ModFailure::Runtime(message))
                if message.contains("demo:scripts/main.lua:1: broken on purpose")),
            "{result:?}"
        );
    }

    #[test]
    fn failure_messages_are_one_bounded_line() {
        let sandbox = sandbox();
        let result = sandbox.exec(
            b"error('\\n[base] fake line\\27[31m' .. string.rep('x', 1e5))",
            "=demo",
        );
        let Err(ModFailure::Runtime(message)) = result else {
            panic!("{result:?}");
        };
        assert!(message.len() <= crate::error::MAX_FAILURE_MESSAGE_BYTES);
        assert!(
            !message.chars().any(|c| c.is_control() && c != '\t'),
            "{message:?}"
        );
        assert!(message.contains(" [base] fake line [31m"), "{message:?}");
    }

    #[test]
    fn the_log_cap_drops_and_counts_the_rest() {
        let sandbox = sandbox();
        let extra = 5;
        let total = MAX_LOG_LINES_PER_CALL + extra;
        eval::<()>(&sandbox, &format!("for i = 1, {total} do print(i) end")).unwrap();
        let log = sandbox.take_log();
        assert_eq!(log.len(), MAX_LOG_LINES_PER_CALL + 1);
        assert_eq!(log[0], LogEntry::Line("1".to_owned()));
        assert_eq!(log[MAX_LOG_LINES_PER_CALL], LogEntry::Dropped(extra));
        // The cap is per call: the next call may print again.
        eval::<()>(&sandbox, "print('again')").unwrap();
        assert_eq!(sandbox.take_log(), vec![LogEntry::Line("again".to_owned())]);
    }

    #[test]
    fn print_joins_with_tabs_and_cuts_long_lines() {
        let sandbox = sandbox();
        eval::<()>(
            &sandbox,
            "print('a', 1, true, nil, 2.5, setmetatable({}, { __tostring = function() return 'T' end }))",
        )
        .unwrap();
        assert_eq!(
            sandbox.take_log(),
            vec![LogEntry::Line("a\t1\ttrue\tnil\t2.5\tT".to_owned())]
        );
        // Three-byte characters, so the cut falls inside one and must back off.
        eval::<()>(
            &sandbox,
            &format!("print(string.rep('\u{20ac}', {MAX_LOG_LINE_BYTES}))"),
        )
        .unwrap();
        let log = sandbox.take_log();
        let [LogEntry::Line(line)] = log.as_slice() else {
            panic!("{log:?}");
        };
        assert!(line.len() <= MAX_LOG_LINE_BYTES);
        assert!(line.len() > MAX_LOG_LINE_BYTES - '\u{20ac}'.len_utf8());
        assert!(line.chars().all(|c| c == '\u{20ac}'));
    }

    #[test]
    fn print_stops_building_a_full_line() {
        let sandbox = sandbox();
        // A long string first: the value after it is never converted.
        eval::<()>(
            &sandbox,
            &format!(
                "print(string.rep('x', {}), setmetatable({{}}, \
                   {{ __tostring = function() converted = true return 'y' end }}))",
                4 * MAX_LOG_LINE_BYTES
            ),
        )
        .unwrap();
        let converted: bool = eval(&sandbox, "return converted == true").unwrap();
        assert!(!converted);
        let log = sandbox.take_log();
        assert_eq!(log, vec![LogEntry::Line("x".repeat(MAX_LOG_LINE_BYTES))]);
    }

    #[test]
    fn printed_control_characters_cannot_fake_lines() {
        let sandbox = sandbox();
        eval::<()>(&sandbox, "print('ok\\n[base] fake\\27[2J', 'tab\\tkept')").unwrap();
        assert_eq!(
            sandbox.take_log(),
            vec![LogEntry::Line("ok [base] fake [2J\ttab\tkept".to_owned())]
        );
    }

    #[test]
    fn a_print_inside_tostring_does_not_panic() {
        let sandbox = sandbox();
        eval::<()>(
            &sandbox,
            "print(setmetatable({}, { __tostring = function() print('inner') return 'outer' end }))",
        )
        .unwrap();
        assert_eq!(
            sandbox.take_log(),
            vec![
                LogEntry::Line("inner".to_owned()),
                LogEntry::Line("outer".to_owned())
            ]
        );
    }

    #[test]
    fn an_empty_state_is_small() {
        let sandbox = sandbox();
        let bytes = sandbox.used_memory();
        // Decision 0014 counts on tens of KB per state; this catches a big regression.
        assert!(bytes < 64 * 1024, "{bytes}");
    }
}
