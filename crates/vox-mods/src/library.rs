//! The standard library a mod script sees: Lua's own, cut down and wrapped so that every part
//! of it is limited (decision 0014, section b).
//!
//! - **Removed:** the base functions that load code (`dofile`, `loadfile`, `load`), drive the
//!   collector (`collectgarbage`), run a message handler the host cannot wrap (`xpcall`) or
//!   write to the console outside the log (`warn`); every `string` function except a short
//!   allowlist, so `dump` (bytecode) and the pattern functions (`match`, `gmatch`, `gsub`,
//!   whose backtracking runs in C where the count hook cannot see it) are gone. Patterns
//!   return later through a Rust matcher that charges the budget, as a minor API version.
//! - **`string.find` searches plain text only**, in Rust in linear time; the `plain` argument
//!   is accepted and ignored.
//! - **Charged:** every remaining `string`, `table` and `utf8` function, the iterator
//!   `utf8.codes` returns, and `tonumber` charge the budget for the work they do in C: one
//!   instruction per argument and per result, one per [`BYTES_PER_INSTRUCTION`] bytes read or
//!   written, and the element counts of the table functions, so a loop over a long string or
//!   table cannot run for seconds within an instruction budget.
//! - **`table` functions take plain tables only:** a table with `__index`, `__newindex` or
//!   `__len` would make the C code call scripts or loop over a length the charge cannot know.
//! - **`pcall` re-raises** a spent budget instead of returning `false` (see `budget`).
//! - **`setmetatable` refuses `__gc`:** a finalizer runs whenever the collector decides,
//!   between host calls or while a disabled mod's state is closed, where no budget applies.

use std::rc::Rc;

use memchr::memmem;
use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::budget::{Budget, MAX_INSTRUCTIONS_AT_LOAD, Spent};

/// Bytes a library function may read or write for the cost of one Lua instruction. A memory
/// copy or scan moves tens of bytes in the time the interpreter runs one instruction, so this
/// under-charges plain copies a little and is a safe bound for the rest. Provisional (S7).
pub const BYTES_PER_INSTRUCTION: u64 = 32;

/// Base functions removed after opening the libraries (see the module comment).
const REMOVED_GLOBALS: [&str; 6] = [
    "dofile",
    "loadfile",
    "load",
    "collectgarbage",
    "xpcall",
    "warn",
];

/// The `string` library table. It is also the `__index` of every string value's metatable, so
/// editing the table covers `("x"):rep(2)` too.
const STRING_LIBRARY: &str = "string";

/// The `string` functions kept, all charged. Every other one is removed.
const KEPT_STRING_FUNCTIONS: [&str; 9] = [
    "byte", "char", "format", "len", "lower", "rep", "reverse", "sub", "upper",
];

/// `string.find`, replaced by a plain search.
const FIND: &str = "find";

/// The `table` library: every function charged, and table arguments must be plain.
const TABLE_LIBRARY: &str = "table";

/// The `utf8` library: every function charged.
const UTF8_LIBRARY: &str = "utf8";

/// `utf8.codes`, whose returned iterator is charged as well.
const UTF8_CODES: &str = "codes";

/// Base functions that are charged: they read a whole string argument.
const CHARGED_GLOBALS: [&str; 1] = ["tonumber"];

/// The protected call, wrapped to re-raise a spent budget.
const PCALL: &str = "pcall";

/// `setmetatable`, wrapped to refuse finalizers.
const SETMETATABLE: &str = "setmetatable";

/// The metatable field of a finalizer.
const GC_FIELD: &str = "__gc";

/// Metatable fields that make the C table functions call scripts or trust a script's length.
const TABLE_METAMETHODS: [&str; 3] = ["__index", "__newindex", "__len"];

/// The error value Lua gives when an allocation fails. A script can raise the same string
/// itself; then it only disables itself, which is harmless.
const LUA_MEMORY_ERROR_MESSAGE: &[u8] = b"not enough memory";

/// What a library function is charged before it runs, from its arguments; an error refuses
/// the call.
type Cost = fn(&Lua, &MultiValue) -> mlua::Result<u64>;

/// Cuts down and wraps the standard library of `lua` as described in the module comment.
pub(crate) fn restrict(lua: &Lua, budget: &Rc<Budget>) -> mlua::Result<()> {
    let globals = lua.globals();
    for name in REMOVED_GLOBALS {
        globals.raw_set(name, Value::Nil)?;
    }

    let string: Table = globals.raw_get(STRING_LIBRARY)?;
    for (name, value) in functions(&string)? {
        if KEPT_STRING_FUNCTIONS.contains(&name.as_str()) {
            string.raw_set(name.as_str(), charged(lua, budget, value, cost_of(&name))?)?;
        } else {
            string.raw_set(name, Value::Nil)?;
        }
    }
    string.raw_set(FIND, plain_find(lua, budget)?)?;

    let table: Table = globals.raw_get(TABLE_LIBRARY)?;
    for (name, value) in functions(&table)? {
        let cost = table_cost_of(&name);
        table.raw_set(name, charged(lua, budget, value, cost)?)?;
    }

    let utf8: Table = globals.raw_get(UTF8_LIBRARY)?;
    for (name, value) in functions(&utf8)? {
        let wrapped = if name == UTF8_CODES {
            charged_codes(lua, budget, value)?
        } else {
            charged(lua, budget, value, input_bytes)?
        };
        utf8.raw_set(name, wrapped)?;
    }

    for name in CHARGED_GLOBALS {
        let original: Function = globals.raw_get(name)?;
        globals.raw_set(name, charged(lua, budget, original, input_bytes)?)?;
    }

    let pcall: Function = globals.raw_get(PCALL)?;
    globals.raw_set(PCALL, guarded_pcall(lua, budget, pcall)?)?;
    let setmetatable: Function = globals.raw_get(SETMETATABLE)?;
    globals.raw_set(SETMETATABLE, no_finalizers(lua, setmetatable)?)?;
    Ok(())
}

/// The functions stored in `table`, by name, collected first so the table can be edited.
fn functions(table: &Table) -> mlua::Result<Vec<(String, Function)>> {
    let mut found = Vec::new();
    for pair in table.pairs::<String, Value>() {
        if let (name, Value::Function(function)) = pair? {
            found.push((name, function));
        }
    }
    Ok(found)
}

/// The charge before calling the `string` function `name`.
fn cost_of(name: &str) -> Cost {
    match name {
        "rep" => rep_cost,
        _ => input_bytes,
    }
}

/// The charge before calling the `table` function `name`; all but `pack` take a table first.
fn table_cost_of(name: &str) -> Cost {
    match name {
        "sort" => sort_cost,
        // With a position, both shift the elements after it; without, they work at the end.
        "insert" => |_, args| {
            let len = raw_len(plain_table(args.front())?);
            Ok(if args.len() > 2 { len } else { 0 })
        },
        "remove" => |_, args| {
            let len = raw_len(plain_table(args.front())?);
            Ok(if args.len() > 1 { len } else { 0 })
        },
        "move" => move_cost,
        "unpack" => |lua, args| element_count(lua, args, 1),
        "concat" => |lua, args| element_count(lua, args, 2),
        _ => input_bytes,
    }
}

/// Runs `body` for a host function and passes its error through [`Budget::fail`], so a
/// memory error stays fatal and a spent budget stops all further code at once.
fn guarded<R>(
    lua: &Lua,
    budget: &Rc<Budget>,
    body: impl FnOnce() -> mlua::Result<R>,
) -> mlua::Result<R> {
    body().map_err(|error| budget.fail(lua, error))
}

/// `f` wrapped so that it charges one per argument and `cost` of its arguments before it runs,
/// and the size of its results after.
fn charged(lua: &Lua, budget: &Rc<Budget>, f: Function, cost: Cost) -> mlua::Result<Function> {
    let budget = Rc::clone(budget);
    lua.create_function(move |lua, args: MultiValue| {
        guarded(lua, &budget, || {
            let _level = budget.enter()?;
            budget.charge(count(args.len()).saturating_add(cost(lua, &args)?))?;
            let results: MultiValue = f.call(args)?;
            budget.charge(output_cost(&results))?;
            Ok(results)
        })
    })
}

/// `utf8.codes`, charged like the rest, whose returned iterator is charged too: by the bytes
/// it moved over, since one step skips any run of continuation bytes.
fn charged_codes(lua: &Lua, budget: &Rc<Budget>, codes: Function) -> mlua::Result<Function> {
    let budget = Rc::clone(budget);
    lua.create_function(move |lua, args: MultiValue| {
        guarded(lua, &budget, || {
            let _level = budget.enter()?;
            budget.charge(count(args.len()).saturating_add(input_bytes(lua, &args)?))?;
            let mut results: MultiValue = codes.call(args)?;
            if let Some(Value::Function(step)) = results.front().cloned() {
                results[0] = Value::Function(charged_step(lua, &budget, step)?);
            }
            Ok(results)
        })
    })
}

/// One step of the `utf8.codes` iteration: `step(s, position)` returns the next position, or
/// nothing at the end. Charged after the call by the distance moved.
fn charged_step(lua: &Lua, budget: &Rc<Budget>, step: Function) -> mlua::Result<Function> {
    let budget = Rc::clone(budget);
    lua.create_function(move |lua, args: MultiValue| {
        guarded(lua, &budget, || {
            let _level = budget.enter()?;
            let length = string_len(args.front());
            let from = integer(lua, args.get(1)).unwrap_or(0);
            let results: MultiValue = step.call(args)?;
            let to = integer(lua, results.front())
                .unwrap_or_else(|| i64::try_from(length).unwrap_or(i64::MAX));
            let moved = u64::try_from(to.saturating_sub(from)).unwrap_or(0);
            budget.charge((moved / BYTES_PER_INSTRUCTION).saturating_add(output_cost(&results)))?;
            Ok(results)
        })
    })
}

/// `pcall` wrapped so that a spent budget, including a memory error it caught, is raised
/// again instead of being returned to the script.
fn guarded_pcall(lua: &Lua, budget: &Rc<Budget>, pcall: Function) -> mlua::Result<Function> {
    let budget = Rc::clone(budget);
    lua.create_function(move |lua, args: MultiValue| {
        guarded(lua, &budget, || {
            let _level = budget.enter()?;
            budget.charge(count(args.len()))?;
            let results: MultiValue = pcall.call(args)?;
            budget.check()?;
            if caught_memory_error(&results) {
                return Err(budget.spend(Spent::Memory));
            }
            Ok(results)
        })
    })
}

/// Whether the results of `pcall` are a caught memory error.
fn caught_memory_error(results: &MultiValue) -> bool {
    match (results.front(), results.get(1)) {
        (Some(Value::Boolean(false)), Some(Value::String(message))) => {
            message.as_bytes().as_ref() == LUA_MEMORY_ERROR_MESSAGE
        }
        (Some(Value::Boolean(false)), Some(Value::Error(error))) => {
            crate::budget::is_memory_error(error)
        }
        _ => false,
    }
}

/// `setmetatable` that refuses a metatable with a `__gc` field (any value, even `false`:
/// Lua marks the object for finalization if the field exists). A field added later has no
/// effect: Lua only looks at the time the metatable is set.
fn no_finalizers(lua: &Lua, setmetatable: Function) -> mlua::Result<Function> {
    lua.create_function(move |_, (target, meta): (Value, Value)| {
        if let Value::Table(meta) = &meta {
            if !meta.raw_get::<Value>(GC_FIELD)?.is_nil() {
                return Err(mlua::Error::runtime(
                    "finalizers (`__gc`) are not allowed in mods",
                ));
            }
        }
        setmetatable.call::<Value>((target, meta))
    })
}

/// `string.find(s, text, init)` as a plain search in Rust: linear in the length of `s`, and
/// charged for it. Positions follow Lua's rules (1-based, negative `init` counts from the end).
fn plain_find(lua: &Lua, budget: &Rc<Budget>) -> mlua::Result<Function> {
    let budget = Rc::clone(budget);
    lua.create_function(
        move |lua, (haystack, needle, init): (mlua::String, mlua::String, Option<i64>)| {
            guarded(lua, &budget, || {
                let haystack = haystack.as_bytes();
                let needle = needle.as_bytes();
                let Some(start) = find_start(init.unwrap_or(1), haystack.len()) else {
                    return Ok(MultiValue::from_vec(vec![Value::Nil]));
                };
                let scanned = haystack.len() - start + needle.len();
                budget.charge(bytes_cost(scanned))?;
                Ok(match memmem::find(&haystack[start..], &needle) {
                    Some(offset) => {
                        let first = start + offset;
                        MultiValue::from_vec(vec![
                            Value::Integer(lua_index(first + 1)),
                            Value::Integer(lua_index(first + needle.len())),
                        ])
                    }
                    None => MultiValue::from_vec(vec![Value::Nil]),
                })
            })
        },
    )
}

/// The 0-based byte where `string.find` starts for the Lua position `init` in a string of
/// `len` bytes, or `None` if it starts past the end (Lua's `posrelatI`, then `init > len`).
fn find_start(init: i64, len: usize) -> Option<usize> {
    let len_i = i64::try_from(len).unwrap_or(i64::MAX);
    let position = if init > 0 {
        init
    } else if init == 0 || init < -len_i {
        1
    } else {
        len_i + init + 1
    };
    let start = usize::try_from(position - 1).ok()?;
    (start <= len).then_some(start)
}

/// A byte position as a Lua integer. Strings are far shorter than `i64::MAX`.
fn lua_index(position: usize) -> i64 {
    i64::try_from(position).unwrap_or(i64::MAX)
}

/// A count as instructions, one each.
fn count(items: usize) -> u64 {
    u64::try_from(items).unwrap_or(u64::MAX)
}

/// Instructions charged for `bytes` bytes of work.
fn bytes_cost(bytes: usize) -> u64 {
    count(bytes) / BYTES_PER_INSTRUCTION
}

/// The length of a string value, or 0 for anything else (numbers are short).
fn string_len(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::String(text)) => count(text.as_bytes().len()),
        _ => 0,
    }
}

/// An integer argument as the C library reads it: an integer, a float without fraction
/// (saturated), or a string that converts to one of those. `None` for anything else.
fn integer(lua: &Lua, value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Integer(number) => Some(*number),
        // No fraction; the cast saturates for values out of range.
        Value::Number(number) if number.fract() == 0.0 => Some(*number as i64),
        text @ Value::String(_) => match lua.coerce_number(text.clone()).ok()?? {
            number if number.fract() == 0.0 => Some(number as i64),
            _ => None,
        },
        _ => None,
    }
}

/// The table argument `value`, which must be a table without the metamethods the C table
/// functions would follow.
fn plain_table(value: Option<&Value>) -> mlua::Result<&Table> {
    let Some(Value::Table(table)) = value else {
        return Err(mlua::Error::runtime("a table function needs a table"));
    };
    if let Some(meta) = table.metatable() {
        for field in TABLE_METAMETHODS {
            if !meta.raw_get::<Value>(field)?.is_nil() {
                return Err(mlua::Error::runtime(format!(
                    "table functions do not take tables with `{field}` in mods"
                )));
            }
        }
    }
    Ok(table)
}

/// The border (`#t`) of `table` without metamethods.
fn raw_len(table: &Table) -> u64 {
    count(table.raw_len())
}

/// The default charge: the bytes of all string arguments.
fn input_bytes(_: &Lua, args: &MultiValue) -> mlua::Result<u64> {
    Ok(args
        .iter()
        .map(|value| string_len(Some(value)))
        .fold(0, u64::saturating_add)
        / BYTES_PER_INSTRUCTION)
}

/// The charge after a call: the bytes of its string results, plus one per result.
fn output_cost(results: &MultiValue) -> u64 {
    let bytes = results
        .iter()
        .map(|value| string_len(Some(value)))
        .fold(0, u64::saturating_add);
    (bytes / BYTES_PER_INSTRUCTION).saturating_add(count(results.len()))
}

/// `string.rep(s, n, sep)` writes `n` copies of `s` and `n - 1` of `sep`, and loops `n` times
/// even when both are empty: charged before it allocates, so a huge request is refused
/// without the work.
fn rep_cost(lua: &Lua, args: &MultiValue) -> mlua::Result<u64> {
    let repeats = u64::try_from(integer(lua, args.get(1)).unwrap_or(0).max(0)).unwrap_or(0);
    let piece = string_len(args.front());
    let separator = string_len(args.get(2));
    let bytes = repeats
        .saturating_mul(piece)
        .saturating_add(repeats.saturating_sub(1).saturating_mul(separator));
    Ok(repeats.max(bytes / BYTES_PER_INSTRUCTION))
}

/// `table.sort` makes about `n log2 n` comparisons, and comparing two strings reads them
/// byte by byte (`strcoll` piece by piece between `\0` bytes, which is slow), so every string
/// element is charged one instruction per byte for each of its about `log2 n` comparisons.
fn sort_cost(_: &Lua, args: &MultiValue) -> mlua::Result<u64> {
    let table = plain_table(args.front())?;
    let n = raw_len(table);
    let depth = u64::from(u64::BITS - n.leading_zeros());
    let comparisons = n.saturating_mul(depth);
    // Too expensive anyway: no need to read the elements.
    if comparisons > MAX_INSTRUCTIONS_AT_LOAD {
        return Ok(comparisons);
    }
    let mut bytes: u64 = 0;
    for index in 1..=n {
        bytes = bytes.saturating_add(string_len(Some(&table.raw_get::<Value>(index)?)));
    }
    Ok(comparisons.saturating_add(bytes.saturating_mul(depth)))
}

/// `table.move(a1, f, e, t, a2)` copies `e - f + 1` elements; both tables must be plain.
fn move_cost(lua: &Lua, args: &MultiValue) -> mlua::Result<u64> {
    plain_table(args.front())?;
    if let Some(destination) = args.get(4).filter(|value| !value.is_nil()) {
        plain_table(Some(destination))?;
    }
    Ok(span(integer(lua, args.get(1)), integer(lua, args.get(2))))
}

/// The elements from `i` to `j` of the plain table first argument, where `i` is the argument
/// at `range_at` (default 1) and `j` the next one (default the raw length).
fn element_count(lua: &Lua, args: &MultiValue, range_at: usize) -> mlua::Result<u64> {
    let table = plain_table(args.front())?;
    let first = integer(lua, args.get(range_at)).unwrap_or(1);
    let last = integer(lua, args.get(range_at + 1))
        .unwrap_or_else(|| i64::try_from(raw_len(table)).unwrap_or(i64::MAX));
    Ok(span(Some(first), Some(last)))
}

/// How many integers lie from `first` to `last`; 0 if either is missing or the range is empty.
fn span(first: Option<i64>, last: Option<i64>) -> u64 {
    match (first, last) {
        (Some(first), Some(last)) if last >= first => u64::try_from(last.saturating_sub(first))
            .unwrap_or(u64::MAX)
            .saturating_add(1),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_start_follows_lua() {
        assert_eq!(find_start(1, 5), Some(0));
        assert_eq!(find_start(0, 5), Some(0));
        assert_eq!(find_start(5, 5), Some(4));
        assert_eq!(find_start(6, 5), Some(5)); // just past the end: only "" matches
        assert_eq!(find_start(7, 5), None);
        assert_eq!(find_start(-1, 5), Some(4));
        assert_eq!(find_start(-5, 5), Some(0));
        assert_eq!(find_start(-99, 5), Some(0));
        assert_eq!(find_start(i64::MIN, 5), Some(0));
        assert_eq!(find_start(i64::MAX, 5), None);
    }

    #[test]
    fn costs_of_counts() {
        let lua = Lua::new();
        let args = |values: Vec<Value>| MultiValue::from_vec(values);
        let text = Value::String(lua.create_string("abcd").unwrap());
        let long = Value::String(lua.create_string("x".repeat(64)).unwrap());
        let empty = Value::String(lua.create_string("").unwrap());
        // At least one per copy; for longer pieces, the bytes of copies and separators.
        let rep = |values| rep_cost(&lua, &args(values)).unwrap();
        assert_eq!(
            rep(vec![text.clone(), Value::Integer(1000), text.clone()]),
            1000
        );
        assert_eq!(
            rep(vec![long.clone(), Value::Integer(1000), long]),
            (1000 * 64 + 999 * 64) / BYTES_PER_INSTRUCTION
        );
        assert_eq!(
            rep(vec![empty.clone(), Value::Integer(i64::MAX)]),
            i64::MAX as u64
        );
        assert_eq!(rep(vec![text.clone(), Value::Integer(-5)]), 0);
        assert_eq!(
            rep(vec![text.clone(), Value::Number(1e300)]),
            i64::MAX as u64
        );
        let seven_million = Value::String(lua.create_string("7000000").unwrap());
        assert_eq!(rep(vec![text.clone(), seven_million]), 7_000_000);

        let table = Value::Table(lua.create_sequence_from([1, 2, 3, 4, 5, 6, 7, 8]).unwrap());
        let huge = Value::String(lua.create_string("1e15").unwrap());
        assert_eq!(
            move_cost(&lua, &args(vec![table.clone(), Value::Integer(1), huge])).unwrap(),
            1_000_000_000_000_000
        );
        assert_eq!(
            element_count(
                &lua,
                &args(vec![table.clone(), Value::Integer(2), Value::Integer(10)]),
                1
            )
            .unwrap(),
            9
        );
        assert_eq!(
            element_count(&lua, &args(vec![table.clone()]), 1).unwrap(),
            8
        );
        assert_eq!(sort_cost(&lua, &args(vec![table.clone()])).unwrap(), 8 * 4);
        let words = Value::Table(lua.create_sequence_from(["ab", "cde"]).unwrap());
        // Two elements, depth 2: 4 comparisons plus 5 bytes read twice.
        assert_eq!(sort_cost(&lua, &args(vec![words])).unwrap(), 4 + 5 * 2);
        assert_eq!(output_cost(&args(vec![text, Value::Integer(1)])), 2);
    }

    #[test]
    fn tables_with_metamethods_are_refused() {
        let lua = Lua::new();
        for field in TABLE_METAMETHODS {
            let table = lua.create_table().unwrap();
            let meta = lua.create_table().unwrap();
            meta.raw_set(field, true).unwrap();
            table.set_metatable(Some(meta)).unwrap();
            assert!(plain_table(Some(&Value::Table(table))).is_err(), "{field}");
        }
        assert!(plain_table(Some(&Value::Integer(1))).is_err());
        assert!(plain_table(None).is_err());
        let fine = lua.create_table().unwrap();
        fine.set_metatable(Some(lua.create_table().unwrap()))
            .unwrap();
        assert!(plain_table(Some(&Value::Table(fine))).is_ok());
    }
}
