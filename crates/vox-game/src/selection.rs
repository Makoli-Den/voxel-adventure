//! Which block the player places: the selection among the placeable blocks.
//!
//! Pure code with no window or input types. The number keys pick a block directly and the mouse
//! wheel cycles through all of them, so a game with more blocks than keys stays reachable.

use vox_world::BlockId;

/// How many number keys select a block: 1 to 9. Blocks past the ninth are reached with the
/// mouse wheel only.
pub const SLOT_KEYS: usize = 9;

/// A block the player may place, with the name to show for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placeable {
    /// The block written into the world.
    pub id: BlockId,
    /// Its registry name, for the panel.
    pub name: String,
}

/// The blocks the player may place and which one is selected. Never empty, so there is always a
/// selected block and it always has a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// The placeable blocks, in the order the keys and the wheel walk them.
    blocks: Vec<Placeable>,
    /// Position of the selected block in `blocks`; always below `blocks.len()`.
    index: usize,
}

impl Selection {
    /// A selection over `blocks` that starts on `start`. `None` if there are no blocks or
    /// `start` is not among them: the caller decides what that means.
    pub fn new(blocks: Vec<Placeable>, start: BlockId) -> Option<Self> {
        let index = blocks.iter().position(|b| b.id == start)?;
        Some(Self { blocks, index })
    }

    /// The block the place key puts down.
    pub fn selected(&self) -> BlockId {
        self.blocks[self.index].id
    }

    /// The registry name of the selected block.
    pub fn selected_name(&self) -> &str {
        &self.blocks[self.index].name
    }

    /// Selects the block of number key `slot`, counted from 0 (key 1 is slot 0). A slot with no
    /// block, because there are fewer blocks than keys, changes nothing. Returns whether the
    /// selection changed.
    pub fn select_slot(&mut self, slot: usize) -> bool {
        if slot >= self.blocks.len() || slot == self.index {
            return false;
        }
        self.index = slot;
        true
    }

    /// Moves the selection `steps` blocks forward (backward when negative), wrapping around at
    /// both ends. Returns whether the selection changed.
    pub fn cycle(&mut self, steps: i32) -> bool {
        let count = i64::try_from(self.blocks.len()).unwrap_or(i64::MAX);
        let moved = (self.index as i64 + i64::from(steps)).rem_euclid(count);
        // `moved` is in 0..count, which fits in usize.
        let moved = moved as usize;
        let changed = moved != self.index;
        self.index = moved;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(count: u16) -> Vec<Placeable> {
        (1..=count)
            .map(|n| Placeable {
                id: BlockId(n),
                name: format!("test:block{n}"),
            })
            .collect()
    }

    fn selection(count: u16, preferred: u16) -> Selection {
        Selection::new(blocks(count), BlockId(preferred)).unwrap()
    }

    #[test]
    fn no_blocks_make_no_selection() {
        assert_eq!(Selection::new(Vec::new(), BlockId(1)), None);
    }

    #[test]
    fn it_starts_on_the_given_block_and_none_that_is_absent() {
        assert_eq!(selection(5, 3).selected(), BlockId(3));
        assert_eq!(Selection::new(blocks(5), BlockId(99)), None);
    }

    #[test]
    fn the_selected_name_follows_the_selection() {
        let mut s = selection(5, 3);
        assert_eq!(s.selected_name(), "test:block3");
        s.cycle(1);
        assert_eq!(s.selected_name(), "test:block4");
    }

    #[test]
    fn a_number_key_selects_that_block() {
        let mut s = selection(5, 1);
        assert!(s.select_slot(3));
        assert_eq!(s.selected(), BlockId(4));
    }

    #[test]
    fn a_key_without_a_block_changes_nothing() {
        let mut s = selection(3, 2);
        assert!(!s.select_slot(3));
        assert!(!s.select_slot(SLOT_KEYS - 1));
        assert_eq!(s.selected(), BlockId(2));
    }

    #[test]
    fn selecting_the_selected_slot_reports_no_change() {
        let mut s = selection(3, 2);
        assert!(!s.select_slot(1));
    }

    #[test]
    fn the_wheel_wraps_at_both_ends() {
        let mut s = selection(4, 4);
        assert!(s.cycle(1));
        assert_eq!(s.selected(), BlockId(1));
        assert!(s.cycle(-1));
        assert_eq!(s.selected(), BlockId(4));
    }

    #[test]
    fn many_steps_wrap_several_times() {
        let mut s = selection(4, 1);
        s.cycle(9);
        assert_eq!(s.selected(), BlockId(2));
        s.cycle(-10);
        // Block 2 is index 1; 1 - 10 = -9 wraps to index 3, which is block 4.
        assert_eq!(s.selected(), BlockId(4));
        // The extreme steps must not overflow: 2^31 is a multiple of 4, so index 3 plus
        // (i32::MIN + 1) wraps to index 0.
        s.cycle(i32::MIN + 1);
        assert_eq!(s.selected(), BlockId(1));
    }

    #[test]
    fn a_full_turn_or_no_step_changes_nothing() {
        let mut s = selection(4, 2);
        assert!(!s.cycle(0));
        assert!(!s.cycle(4));
        assert!(!s.cycle(-8));
        assert_eq!(s.selected(), BlockId(2));
    }

    #[test]
    fn a_single_block_stays_selected() {
        let mut s = selection(1, 1);
        assert!(!s.cycle(1));
        assert!(!s.cycle(-3));
        assert_eq!(s.selected(), BlockId(1));
    }

    #[test]
    fn the_wheel_reaches_blocks_past_the_last_key() {
        let mut s = selection(12, 1);
        s.cycle(11);
        assert_eq!(s.selected(), BlockId(12));
    }

    #[test]
    fn the_keys_reach_the_first_nine_blocks_of_a_longer_list() {
        let mut s = selection(12, 1);
        assert!(s.select_slot(SLOT_KEYS - 1));
        assert_eq!(s.selected(), BlockId(9));
    }
}
