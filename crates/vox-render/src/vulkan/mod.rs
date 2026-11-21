//! Vulkan backend.
//!
//! Vulkan calls are `unsafe` by nature. This module tree is the one place in the
//! project where `unsafe` is allowed. Every block carries a `// SAFETY:` comment,
//! and every `unsafe fn` documents its contract in a `# Safety` section.
//! The target is plain Vulkan 1.0 so that old GPUs keep working.
#![allow(unsafe_code)]

mod buffer;
mod context;
mod depth;
mod frames;
mod gpu;
mod memory;
mod mesh;
mod pipeline;
mod render_pass;
mod renderer;
mod retired;
mod swapchain;

use ash::vk;
use raw_window_handle::HandleError;

use crate::RenderError;

pub(crate) use renderer::VulkanRenderer;

// Backend errors are wrapped here, so `RenderError` itself stays free of Vulkan types.

impl From<vk::Result> for RenderError {
    fn from(error: vk::Result) -> Self {
        Self::Backend(Box::new(error))
    }
}

impl From<ash::LoadingError> for RenderError {
    fn from(error: ash::LoadingError) -> Self {
        Self::Backend(Box::new(error))
    }
}

impl From<HandleError> for RenderError {
    fn from(error: HandleError) -> Self {
        Self::Backend(Box::new(error))
    }
}
