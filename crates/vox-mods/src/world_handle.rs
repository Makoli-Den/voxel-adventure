//! What an event handler sees of the world: the event table and the world handle of one call
//! (decision 0014, section c).
//!
//! The handle is a fresh table whose two functions, `get_block` and `set_block`, are bound to
//! an `mlua` scope that ends with the call: a handle a script keeps in a global is dead in the
//! next call (calling it raises an error), so the host never lends the world past the call.
//!
//! Reads see the world as the host was given it (the state right after the player's edit);
//! writes are only queued as [`BlockEdit`]s, which the game applies after all mods returned.
//! Every function charges the budget like the library functions do and checks its arguments
//! itself, so a wrong argument gets a message in script terms, never a Rust type name.
//!
//! The world functions never call back into Lua, so they take no nesting level of the budget.
//!
//! Three mistakes disable the mod even if the script catches the error with `pcall`: a missing
//! capability, an unknown block name and more than [`MAX_EDITS_PER_CALL`] writes. They are
//! recorded in the call's [`CallRecord`], which the host reads after the call.

use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Lua, MultiValue, Scope, Table, Value};
use vox_world::{BlockId, BlockRegistry, World};

use crate::api::BlockEventKind;
use crate::budget::Budget;
use crate::error::{MAX_FAILURE_MESSAGE_BYTES, ModFailure};
use crate::library::{bytes_cost, count, guarded};
use crate::manifest::Capability;
use crate::text::printable;

/// The most block writes one mod's handler call may queue; one more disables that mod. It
/// limits one mod, not one event: every mod with a handler may use it, so the writes of a whole
/// event are bounded by [`MAX_EDITS_PER_EVENT`](crate::MAX_EDITS_PER_EVENT). Every write may
/// remesh up to four chunks on the main thread. Provisional: step S7 measures the cost of this
/// many scattered writes with their remeshing.
pub const MAX_EDITS_PER_CALL: usize = 64;

/// How many mods may each use their whole [`MAX_EDITS_PER_CALL`] in one event before the
/// event's total cap cuts in. A guess for a modest mod set; S7 measures what the total costs.
const MODS_AT_FULL_WRITES: usize = 4;

/// The most block writes one event returns over all mods together. The writes past it are
/// dropped (their mod keeps running) and a log line names how many, so a crowd of mods cannot
/// multiply [`MAX_EDITS_PER_CALL`] into a long frame. Provisional: step S7 measures the total
/// cost of writes per event and fixes this value.
pub const MAX_EDITS_PER_EVENT: usize = MODS_AT_FULL_WRITES * MAX_EDITS_PER_CALL;

/// Extra instructions charged for one call of a world function, besides one per argument and
/// result: it looks a chunk up in a hash map and a name up in the registry, which take about
/// as long as a few Lua instructions. Provisional (S7).
const WORLD_LOOKUP_INSTRUCTIONS: u64 = 4;

/// `world:get_block(x, y, z)`.
const GET_BLOCK: &str = "get_block";

/// `world:set_block(x, y, z, name)`.
const SET_BLOCK: &str = "set_block";

/// The names of the coordinates, in the order of the arguments and of a position array. They
/// are also the fields of the event table.
const AXES: [&str; 3] = ["x", "y", "z"];

/// The event table's field with the block name.
const BLOCK_FIELD: &str = "block";

/// Where the coordinates start among the arguments of a world function: `world:f(...)` passes
/// the handle itself first.
const FIRST_COORDINATE: usize = 1;

/// Where the block name is among the arguments of `set_block`, after the coordinates.
const NAME_ARGUMENT: usize = FIRST_COORDINATE + AXES.len();

/// A block event for the mods: what the player did, where, and to which block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockEvent {
    /// Which event.
    pub kind: BlockEventKind,
    /// The world block position of the edited cell.
    pub position: [i32; 3],
    /// For [`BlockEventKind::BlockBroken`] the block that was there, for
    /// [`BlockEventKind::BlockPlaced`] the block that was placed.
    pub block: BlockId,
}

/// One block write a mod queued during an event, for the game to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockEdit {
    /// The world block position to write.
    pub position: [i32; 3],
    /// What to write there.
    pub block: BlockId,
}

/// What a handler call may reach: the world, the registry to name blocks, and the
/// capabilities the mod declared.
#[derive(Clone, Copy)]
pub(crate) struct WorldAccess<'a> {
    pub(crate) world: &'a World,
    pub(crate) registry: &'a BlockRegistry,
    pub(crate) capabilities: &'a [Capability],
}

/// What one handler call did through its world handle.
#[derive(Debug, Default)]
pub(crate) struct CallRecord {
    /// The queued writes, in the order the script made them.
    pub(crate) edits: Vec<BlockEdit>,
    /// The first mistake that disables the mod whatever the script does next.
    pub(crate) violation: Option<ModFailure>,
}

impl CallRecord {
    /// Records `failure` (the first one stays) and returns the error to raise.
    fn violate(&mut self, failure: ModFailure) -> mlua::Error {
        let first = self.violation.get_or_insert(failure);
        mlua::Error::runtime(first.to_string())
    }

    /// Fails again with the recorded mistake, if there is one: a script that caught it gets
    /// nothing more from the world.
    fn check(&self) -> mlua::Result<()> {
        match &self.violation {
            Some(failure) => Err(mlua::Error::runtime(failure.to_string())),
            None => Ok(()),
        }
    }
}

/// The event table a handler gets: `{ x = ..., y = ..., z = ..., block = "ns:name" }`. A fresh
/// table per mod, so one mod cannot change what the next one sees.
pub(crate) fn event_table(lua: &Lua, event: &BlockEvent, block_name: &str) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    for (axis, value) in AXES.into_iter().zip(event.position) {
        table.raw_set(axis, value)?;
    }
    table.raw_set(BLOCK_FIELD, block_name)?;
    Ok(table)
}

/// The world handle of one call: a table with `get_block` and `set_block`, both bound to
/// `scope`, so they stop working when the call ends.
pub(crate) fn world_handle<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    lua: &Lua,
    budget: &Rc<Budget>,
    access: WorldAccess<'env>,
    record: &'env RefCell<CallRecord>,
) -> mlua::Result<Table> {
    let handle = lua.create_table()?;
    let read_budget = Rc::clone(budget);
    let get = scope.create_function(move |lua, args: MultiValue| {
        guarded(lua, &read_budget, || {
            get_block(lua, &read_budget, access, record, &args)
        })
    })?;
    let write_budget = Rc::clone(budget);
    let set = scope.create_function(move |lua, args: MultiValue| {
        guarded(lua, &write_budget, || {
            set_block(&write_budget, access, record, &args)
        })
    })?;
    handle.raw_set(GET_BLOCK, get)?;
    handle.raw_set(SET_BLOCK, set)?;
    Ok(handle)
}

/// `world:get_block(x, y, z)`: the name of the block, or `nil` if its chunk is not loaded.
fn get_block(
    lua: &Lua,
    budget: &Budget,
    access: WorldAccess<'_>,
    record: &RefCell<CallRecord>,
    args: &MultiValue,
) -> mlua::Result<Value> {
    budget.charge(count(args.len()).saturating_add(WORLD_LOOKUP_INSTRUCTIONS))?;
    allow(access, record, GET_BLOCK, Capability::WorldRead)?;
    let position = position(args, GET_BLOCK)?;
    // An id the registry does not know is never written by the game; it reads like no chunk.
    let Some(name) = access
        .world
        .block_at(position)
        .and_then(|id| access.registry.name(id))
    else {
        return Ok(Value::Nil);
    };
    budget.charge(bytes_cost(name.len()).saturating_add(1))?;
    Ok(Value::String(lua.create_string(name)?))
}

/// `world:set_block(x, y, z, name)`: queues a write and returns `true`, or returns `false`
/// if the chunk is not loaded (nothing is queued).
fn set_block(
    budget: &Budget,
    access: WorldAccess<'_>,
    record: &RefCell<CallRecord>,
    args: &MultiValue,
) -> mlua::Result<bool> {
    let name_bytes = match args.get(NAME_ARGUMENT) {
        Some(Value::String(name)) => name.as_bytes().len(),
        _ => 0,
    };
    budget.charge(
        count(args.len())
            .saturating_add(WORLD_LOOKUP_INSTRUCTIONS)
            .saturating_add(bytes_cost(name_bytes)),
    )?;
    allow(access, record, SET_BLOCK, Capability::WorldWrite)?;
    let position = position(args, SET_BLOCK)?;
    let block = block_id(access.registry, record, args.get(NAME_ARGUMENT))?;
    if access.world.block_at(position).is_none() {
        return Ok(false);
    }
    let mut record = record.borrow_mut();
    if record.edits.len() >= MAX_EDITS_PER_CALL {
        return Err(record.violate(ModFailure::TooManyEdits {
            limit: MAX_EDITS_PER_CALL,
        }));
    }
    record.edits.push(BlockEdit { position, block });
    Ok(true)
}

/// Fails if an earlier mistake of this call disabled the mod, or if the mod did not declare
/// `capability`, which `function` needs (that is a mistake too).
fn allow(
    access: WorldAccess<'_>,
    record: &RefCell<CallRecord>,
    function: &'static str,
    capability: Capability,
) -> mlua::Result<()> {
    let mut record = record.borrow_mut();
    record.check()?;
    if access.capabilities.contains(&capability) {
        Ok(())
    } else {
        Err(record.violate(ModFailure::MissingCapability {
            function,
            capability,
        }))
    }
}

/// The position given to `function`: the handle first (a call with `:`), then three integer
/// coordinates within `i32`. An ordinary error otherwise, which the script may catch.
fn position(args: &MultiValue, function: &str) -> mlua::Result<[i32; 3]> {
    if !matches!(args.front(), Some(Value::Table(_))) {
        return Err(mlua::Error::runtime(format!(
            "call `world:{function}` with a colon, as a method of the world handle"
        )));
    }
    let mut position = [0; AXES.len()];
    for (index, (slot, axis)) in position.iter_mut().zip(AXES).enumerate() {
        *slot = coordinate(args.get(FIRST_COORDINATE + index)).ok_or_else(|| {
            mlua::Error::runtime(format!(
                "`world:{function}`: {axis} must be an integer from {} to {}",
                i32::MIN,
                i32::MAX
            ))
        })?;
    }
    Ok(position)
}

/// A coordinate: an integer, or a float without fraction, within `i32`. Strings are refused,
/// although Lua's own functions would convert them: a coordinate given as text is a mistake.
fn coordinate(value: Option<&Value>) -> Option<i32> {
    match value? {
        Value::Integer(number) => i32::try_from(*number).ok(),
        Value::Number(number)
            if number.fract() == 0.0
                && *number >= f64::from(i32::MIN)
                && *number <= f64::from(i32::MAX) =>
        {
            // In range and without fraction, so the cast is exact.
            Some(*number as i32)
        }
        _ => None,
    }
}

/// The id of the block named by `value`. A name that is not a string is an ordinary error; a
/// string the registry does not know is a mistake that disables the mod.
fn block_id(
    registry: &BlockRegistry,
    record: &RefCell<CallRecord>,
    value: Option<&Value>,
) -> mlua::Result<BlockId> {
    let Some(Value::String(name)) = value else {
        return Err(mlua::Error::runtime(format!(
            "`world:{SET_BLOCK}`: the block name must be a string like `{}`",
            vox_world::AIR_NAME
        )));
    };
    let bytes = name.as_bytes();
    if let Some(id) = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| registry.id(text))
    {
        return Ok(id);
    }
    // Cut before converting, so a huge name costs no more than the message.
    let shown = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_FAILURE_MESSAGE_BYTES)]);
    Err(record.borrow_mut().violate(ModFailure::UnknownBlock {
        name: printable(&shown, MAX_FAILURE_MESSAGE_BYTES),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_are_integers_within_i32() {
        let lua = Lua::new();
        let text = Value::String(lua.create_string("1").unwrap());
        assert_eq!(coordinate(Some(&Value::Integer(-7))), Some(-7));
        assert_eq!(coordinate(Some(&Value::Number(3.0))), Some(3));
        assert_eq!(
            coordinate(Some(&Value::Integer(i64::from(i32::MIN)))),
            Some(i32::MIN)
        );
        assert_eq!(
            coordinate(Some(&Value::Number(f64::from(i32::MAX)))),
            Some(i32::MAX)
        );
        for bad in [
            Value::Integer(i64::from(i32::MAX) + 1),
            Value::Number(f64::from(i32::MIN) - 1.0),
            Value::Number(0.5),
            Value::Number(f64::NAN),
            Value::Number(f64::INFINITY),
            Value::Nil,
            Value::Boolean(true),
            text,
        ] {
            assert_eq!(coordinate(Some(&bad)), None, "{bad:?}");
        }
        assert_eq!(coordinate(None), None);
    }

    #[test]
    fn the_first_mistake_stays() {
        let mut record = CallRecord::default();
        record.violate(ModFailure::TooManyEdits { limit: 1 });
        record.violate(ModFailure::Memory);
        assert_eq!(
            record.violation,
            Some(ModFailure::TooManyEdits { limit: 1 })
        );
        assert!(record.check().is_err());
        assert!(CallRecord::default().check().is_ok());
    }
}
