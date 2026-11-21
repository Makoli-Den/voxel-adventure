//! Deferred freeing: GPU resources that frames still in flight may read.
//!
//! When a chunk mesh is replaced or removed, frames that were already submitted can
//! still draw its buffer. Waiting for the whole device to go idle on every change
//! would stall the CPU and the GPU, so the old mesh is parked here instead, stamped
//! with the frame number, and dropped once the fences prove no frame can use it.
//!
//! This module is pure bookkeeping, generic over what it holds, so the policy is
//! unit tested without a GPU.
//!
//! # Frame numbers and why the policy is safe
//! Frames are numbered from 0 in the order they are submitted. Frame `k` runs in slot
//! `k % frames_in_flight`, and before frame `n` is recorded the CPU waits on its slot's
//! fence, which the last submit to that slot signals: frame `n - frames_in_flight`.
//! Each earlier frame `k` had its fence waited on before frame `k + frames_in_flight`
//! was recorded, so after that wait every frame `0..=n - frames_in_flight` is known to
//! be finished: [`completed_after_wait`] frames in all. (No queue ordering is assumed:
//! each frame's completion is proven by its own fence.)
//!
//! A resource retired while `submitted` frames have been submitted can be used by
//! frames `0..submitted` only: it was removed from the renderer before the next frame
//! is recorded, and recording and submitting happen in one call, so no recorded but
//! unsubmitted command buffer refers to it. (A command buffer whose submit failed is
//! never submitted later; the renderer is dropped after an error.) The stamp is
//! therefore `submitted`, and the resource may be freed once `completed >= stamp`.
//!
//! Two more moments prove frames finished without the frame loop's wait: after
//! `vkDeviceWaitIdle` all submitted frames are done, and a non-blocking poll that finds
//! every slot's fence signaled proves the same ([`completed_when_polled`]). The second
//! keeps the list bounded while no frames are drawn, for example in a minimized window.

/// Resources waiting until the GPU can no longer use them.
pub struct Retired<T> {
    /// Each item with its stamp: the number of frames submitted when it was retired.
    items: Vec<(u64, T)>,
}

impl<T> Retired<T> {
    /// An empty list.
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Parks `item`, which frames `0..submitted` may still use.
    pub fn retire(&mut self, item: T, submitted: u64) {
        self.items.push((submitted, item));
    }

    /// Drops every item that no unfinished frame can use, given that frames
    /// `0..completed` are known to be finished.
    // HOT PATH: runs every frame. With nothing retired it is one length check; otherwise
    // one pass over a list that holds at most the meshes changed in the last few frames.
    pub fn release(&mut self, completed: u64) {
        if !self.items.is_empty() {
            self.items.retain(|(stamp, _)| !can_free(*stamp, completed));
        }
    }

    /// Whether nothing is waiting.
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// How many items are still waiting.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.items.len()
    }
}

/// Whether an item retired with `stamp` (frames `0..stamp` may use it) can be freed when
/// frames `0..completed` are known to be finished.
pub fn can_free(stamp: u64, completed: u64) -> bool {
    completed >= stamp
}

/// How many frames are known to be finished when the fences of all slots were just
/// polled without waiting: all `submitted` of them if every fence is signaled, otherwise
/// nothing new is known (`None`).
///
/// Why "every slot fence signaled" proves that every submitted frame finished:
/// - A slot's fence is reset only right before the submit that signals it again, and a
///   failed submit ends the renderer's use. So a signaled fence means the last frame
///   submitted in that slot has finished (or the slot was never used: fences start
///   signaled).
/// - Frame `k` uses slot `k % frames_in_flight` (derived from the frame number, see
///   `frames::slot_index`), so the last `frames_in_flight` submitted frames lie in
///   distinct slots, and each is the last frame of its slot: all of them finished.
/// - Every earlier frame `k` had its fence waited on before frame `k + frames_in_flight`
///   was recorded, which is one of the frames already submitted, so it finished too.
///
/// A fence that becomes signaled between two polls only makes the answer more true.
pub fn completed_when_polled(submitted: u64, all_fences_signaled: bool) -> Option<u64> {
    all_fences_signaled.then_some(submitted)
}

/// How many frames are known to be finished right after waiting on the fence of the slot
/// that frame `submitted` (the next one to be recorded) is about to reuse.
///
/// That fence was last signaled by frame `submitted - frames_in_flight`, so frames
/// `0..=submitted - frames_in_flight` are done; before that many frames exist, none is.
pub fn completed_after_wait(submitted: u64, frames_in_flight: usize) -> u64 {
    // Lossless: the number of frames in flight is a small constant.
    (submitted + 1).saturating_sub(frames_in_flight as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IN_FLIGHT: usize = 2;

    #[test]
    fn nothing_is_known_finished_before_the_slots_were_used_once() {
        assert_eq!(completed_after_wait(0, IN_FLIGHT), 0);
        assert_eq!(completed_after_wait(1, IN_FLIGHT), 0);
    }

    #[test]
    fn the_wait_for_frame_n_proves_frames_up_to_n_minus_in_flight() {
        // Before frame 2 the slot of frame 0 is waited on: frame 0 is done.
        assert_eq!(completed_after_wait(2, IN_FLIGHT), 1);
        assert_eq!(completed_after_wait(10, IN_FLIGHT), 9);
        assert_eq!(completed_after_wait(10, 3), 8);
    }

    #[test]
    fn an_item_is_kept_while_a_frame_that_may_use_it_runs() {
        // Retired after 5 submits: frames 0..=4 may use it.
        assert!(!can_free(5, 4));
        assert!(can_free(5, 5));
        assert!(can_free(5, 6));
    }

    #[test]
    fn an_item_retired_before_any_frame_is_free_at_once() {
        assert!(can_free(0, 0));
    }

    #[test]
    fn a_mesh_replaced_between_frames_lives_until_the_wait_of_the_second_next_frame() {
        let mut retired = Retired::new();
        // Frames 0..=4 are submitted, then the mesh is replaced.
        retired.retire("old mesh", 5);
        // Frame 5 waits on the fence of frame 3; frame 4 may still read the mesh.
        retired.release(completed_after_wait(5, IN_FLIGHT));
        assert_eq!(retired.len(), 1);
        // Frame 6 waits on the fence of frame 4, the last frame that could use it.
        retired.release(completed_after_wait(6, IN_FLIGHT));
        assert_eq!(retired.len(), 0);
    }

    #[test]
    fn release_drops_only_the_items_that_are_free() {
        let mut retired = Retired::new();
        retired.retire(1, 3);
        retired.retire(2, 7);
        retired.release(3);
        assert_eq!(retired.len(), 1);
        retired.release(7);
        assert_eq!(retired.len(), 0);
        assert!(retired.is_empty());
    }

    #[test]
    fn all_fences_signaled_proves_every_submitted_frame_finished() {
        assert_eq!(completed_when_polled(12, true), Some(12));
        assert_eq!(completed_when_polled(0, true), Some(0));
    }

    #[test]
    fn one_unsignaled_fence_proves_nothing_new() {
        assert_eq!(completed_when_polled(12, false), None);
    }

    #[test]
    fn a_poll_with_all_fences_signaled_frees_even_the_newest_retired_item() {
        let mut retired = Retired::new();
        // Retired after 12 submits, so frames up to 11 may use it; the poll shows all 12 done.
        retired.retire("old mesh", 12);
        if let Some(completed) = completed_when_polled(12, true) {
            retired.release(completed);
        }
        assert!(retired.is_empty());
    }

    #[test]
    fn a_poll_with_a_busy_fence_keeps_everything() {
        let mut retired = Retired::new();
        retired.retire("old mesh", 12);
        if let Some(completed) = completed_when_polled(12, false) {
            retired.release(completed);
        }
        assert_eq!(retired.len(), 1);
    }
}
