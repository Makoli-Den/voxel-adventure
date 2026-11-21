//! Block identifier.

/// Numeric block identifier.
///
/// Chunks store only numbers. The mapping between numbers and block names is kept
/// by a registry, so a world does not break when the set of mods changes.
/// `u16` allows up to 65,536 block kinds and costs 2 bytes per block in a chunk.
///
/// `#[repr(transparent)]` guarantees the same layout as a plain `u16`, which matters
/// because vertices that contain a `BlockId` are uploaded to the GPU as raw bytes.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BlockId(pub u16);

impl BlockId {
    /// Air. Zero, so a freshly created chunk is empty by default.
    pub const AIR: Self = Self(0);

    /// Returns `true` if the block is air.
    pub const fn is_air(self) -> bool {
        self.0 == Self::AIR.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_block_is_air() {
        assert!(BlockId::default().is_air());
    }

    #[test]
    fn non_zero_block_is_not_air() {
        assert!(!BlockId(1).is_air());
    }
}
