//! The budget of one host call into a mod: Lua instructions, wall-clock time and the nesting
//! depth of host functions, plus the sticky record of a spent budget (decision 0014, b).
//!
//! **A spent budget is sticky.** Once a call ran out of instructions or time, or a memory
//! error was seen (in a host function, or caught by `pcall`), the state is marked spent for
//! good: every later check raises again, the wrapped `pcall` re-raises instead of returning
//! `false`, the hook switches to every instruction and call so nothing runs while the error
//! unwinds, and the host refuses every later call and disables the mod. A flag that only the
//! host reads is not enough: in `while true do pcall(endless) end` every check lands inside the
//! inner call and is caught there, so the loop would never end. A memory error raised by the
//! VM can be hidden from `pcall` (a `__close` method raising another error replaces it); then
//! the mod keeps running, still held at its memory cap.
//!
//! All limits are provisional and are fixed by measurement in step S7 on the low-end device.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{HookTriggers, Lua, VmState};

/// How many Lua instructions run between two checks of the budget. Lower is more precise but
/// slower: every check is a call from the interpreter into Rust. Provisional (S7 measures it).
pub const HOOK_INTERVAL: u32 = 1_000;

/// Instruction budget of one event handler call: about 4 ms at an assumed 50 million Lua
/// instructions per second on an old CPU, a quarter of a 60 Hz frame. Provisional (S7).
pub const MAX_INSTRUCTIONS_PER_CALL: u64 = 200_000;

/// Instruction budget of running the entry script once at startup, where a mod builds its
/// tables. Startup is not frame-bound, so it may take far longer than a handler. Provisional.
pub const MAX_INSTRUCTIONS_AT_LOAD: u64 = 10_000_000;

/// Wall-clock limit of one event handler call, checked with the instruction budget. It catches
/// work the instruction count misses: single instructions that copy or compare long strings.
/// Three 60 Hz frames, so far above what [`MAX_INSTRUCTIONS_PER_CALL`] takes even in a debug
/// build, and a hitch the player notices only once (the mod is disabled). Provisional (S7).
pub const MAX_CALL_WALL_TIME: Duration = Duration::from_millis(50);

/// Wall-clock limit of running the entry script. Startup may be slow, but a mod must not
/// hang it. Far above what [`MAX_INSTRUCTIONS_AT_LOAD`] takes in a debug build. Provisional.
pub const MAX_LOAD_WALL_TIME: Duration = Duration::from_secs(2);

/// How deeply host functions that call back into Lua (`pcall`, `print`, the charged library
/// functions) may nest, as in `local function f() return pcall(f) end`. Every level costs
/// native stack in Rust and C. Measured on a 1 MiB thread (2026-11-24): without a limit that
/// recursion overflowed the stack of a debug build, and at 32 levels the debug build of Lua
/// stopped on its own stack check (`lapi.c`) inside `mlua`; 16 passed in debug and release.
/// Real scripts nest a few levels. Provisional.
pub const MAX_HOST_CALL_DEPTH: u32 = 16;

/// Why a budget is spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spent {
    /// More instructions than the call may run.
    Instructions,
    /// Longer than the call may take.
    WallTime,
    /// An allocation hit the memory cap.
    Memory,
}

impl Spent {
    /// The error raised into the script.
    fn error(self) -> mlua::Error {
        match self {
            Self::Instructions => mlua::Error::runtime("instruction budget exhausted"),
            Self::WallTime => mlua::Error::runtime("time budget exhausted"),
            Self::Memory => mlua::Error::MemoryError("memory cap reached".to_owned()),
        }
    }
}

/// The budget of the current call, shared by the hook, the wrapped library functions and the
/// host.
#[derive(Debug, Default)]
pub(crate) struct Budget {
    /// Instructions the current call may still run. Zero outside a host call, so nothing can
    /// run between calls.
    remaining: Cell<u64>,
    /// When the current call must have ended; `None` outside a call.
    deadline: Cell<Option<Instant>>,
    /// Set once a budget is spent, never cleared: the state is dead.
    spent: Cell<Option<Spent>>,
    /// How many host functions that call back into Lua are running now, nested.
    depth: Cell<u32>,
    /// Whether the hook already checks every instruction and call (see [`Budget::fail`]).
    escalated: Cell<bool>,
}

impl Budget {
    /// Starts a host call with `instructions` and `wall_time`.
    pub(crate) fn open(&self, instructions: u64, wall_time: Duration) {
        self.remaining.set(instructions);
        self.deadline.set(Instant::now().checked_add(wall_time));
    }

    /// Ends the host call: nothing may run until the next one.
    pub(crate) fn close(&self) {
        self.remaining.set(0);
        self.deadline.set(None);
    }

    /// Why the budget is spent, if it is.
    pub(crate) fn spent(&self) -> Option<Spent> {
        self.spent.get()
    }

    /// Records that the budget is spent for `why` (the first reason stays) and returns the
    /// error to raise.
    pub(crate) fn spend(&self, why: Spent) -> mlua::Error {
        let first = self.spent.get().unwrap_or(why);
        self.spent.set(Some(first));
        first.error()
    }

    /// Fails if the budget is spent.
    pub(crate) fn check(&self) -> mlua::Result<()> {
        match self.spent.get() {
            Some(why) => Err(why.error()),
            None => Ok(()),
        }
    }

    /// Charges `instructions` to the current call and checks the clock. Fails, and stays
    /// failed, once the call is over either limit.
    pub(crate) fn charge(&self, instructions: u64) -> mlua::Result<()> {
        self.check()?;
        match self.remaining.get().checked_sub(instructions) {
            Some(left) => self.remaining.set(left),
            None => {
                self.remaining.set(0);
                return Err(self.spend(Spent::Instructions));
            }
        }
        match self.deadline.get() {
            Some(deadline) if Instant::now() < deadline => Ok(()),
            // No deadline means no call is open: nothing may run.
            _ => Err(self.spend(Spent::WallTime)),
        }
    }

    /// Enters a host function that may call back into Lua; the guard leaves it on drop.
    /// Fails past [`MAX_HOST_CALL_DEPTH`]. This is an ordinary error, not a spent budget: the
    /// script may catch it, and unwinding frees the levels again.
    pub(crate) fn enter(&self) -> mlua::Result<DepthGuard<'_>> {
        let depth = self.depth.get();
        if depth >= MAX_HOST_CALL_DEPTH {
            return Err(mlua::Error::runtime(format!(
                "host functions nested more than {MAX_HOST_CALL_DEPTH} levels deep"
            )));
        }
        self.depth.set(depth + 1);
        Ok(DepthGuard { budget: self })
    }

    /// Handles an error leaving a host function or the hook: notes a memory error (see
    /// [`Budget::note`]) and, once the budget is spent, switches the hook of `lua` to fire on
    /// every instruction and every call, so whatever still runs while the error unwinds (a
    /// `__close` method, a C function it calls) fails at once instead of at the next of the
    /// [`HOOK_INTERVAL`] checks. Returns the error to raise.
    pub(crate) fn fail(self: &Rc<Self>, lua: &Lua, error: mlua::Error) -> mlua::Error {
        let error = self.note(error);
        if self.spent().is_some() && !self.escalated.replace(true) {
            let budget = Rc::clone(self);
            let every_step = HookTriggers::new().on_calls().every_nth_instruction(1);
            // If this fails, the old hook still stops the script at its next check.
            let _ = lua.set_global_hook(every_step, move |_, _| {
                budget.check().map(|()| VmState::Continue)
            });
        }
        error
    }

    /// Records a memory error that passed through a host function, so it stays fatal even if
    /// the script catches it. Returns the error unchanged.
    pub(crate) fn note(&self, error: mlua::Error) -> mlua::Error {
        if is_memory_error(&error) {
            self.spend(Spent::Memory)
        } else {
            error
        }
    }
}

/// One level of host function nesting, left on drop.
pub(crate) struct DepthGuard<'a> {
    budget: &'a Budget,
}

impl Drop for DepthGuard<'_> {
    fn drop(&mut self) {
        self.budget.depth.set(self.budget.depth.get() - 1);
    }
}

/// Whether `error`, or the error it wraps, is Lua running out of memory.
pub(crate) fn is_memory_error(error: &mlua::Error) -> bool {
    let mut cause = error;
    while let mlua::Error::CallbackError { cause: inner, .. } = cause {
        cause = inner;
    }
    matches!(cause, mlua::Error::MemoryError(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges_and_sticks() {
        let budget = Budget::default();
        budget.open(10, Duration::from_secs(60));
        assert!(budget.charge(4).is_ok());
        assert!(budget.charge(6).is_ok());
        assert_eq!(budget.spent(), None);
        assert!(budget.charge(1).is_err());
        assert_eq!(budget.spent(), Some(Spent::Instructions));
        budget.open(100, Duration::from_secs(60));
        assert!(budget.charge(1).is_err(), "a spent budget is never renewed");
    }

    #[test]
    fn the_clock_spends_the_budget() {
        let budget = Budget::default();
        budget.open(u64::MAX, Duration::ZERO);
        assert!(budget.charge(0).is_err());
        assert_eq!(budget.spent(), Some(Spent::WallTime));
    }

    #[test]
    fn nothing_runs_outside_a_call() {
        let budget = Budget::default();
        assert!(budget.charge(0).is_err());
        let budget = Budget::default();
        budget.open(100, Duration::from_secs(60));
        budget.close();
        assert!(budget.charge(0).is_err());
    }

    #[test]
    fn the_first_reason_stays() {
        let budget = Budget::default();
        budget.spend(Spent::Memory);
        budget.spend(Spent::Instructions);
        assert_eq!(budget.spent(), Some(Spent::Memory));
        assert!(is_memory_error(&budget.check().unwrap_err()));
    }

    #[test]
    fn depth_is_limited_and_released() {
        let budget = Budget::default();
        let mut guards = Vec::new();
        for _ in 0..MAX_HOST_CALL_DEPTH {
            guards.push(budget.enter().unwrap());
        }
        assert!(budget.enter().is_err());
        guards.pop();
        assert!(budget.enter().is_ok());
        drop(guards);
        assert_eq!(budget.depth.get(), 0);
    }

    #[test]
    fn memory_errors_are_noted_through_callbacks() {
        let budget = Budget::default();
        // `mlua` declares the field as an `Arc`; the error never crosses threads here.
        #[allow(clippy::arc_with_non_send_sync)]
        let wrapped = mlua::Error::CallbackError {
            traceback: String::new(),
            cause: std::sync::Arc::new(mlua::Error::MemoryError("x".to_owned())),
        };
        assert!(is_memory_error(&budget.note(wrapped)));
        assert_eq!(budget.spent(), Some(Spent::Memory));
        let other = Budget::default();
        other.note(mlua::Error::runtime("x"));
        assert_eq!(other.spent(), None);
    }
}
