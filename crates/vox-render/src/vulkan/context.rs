//! Vulkan instance and window surface.

use std::ffi::{CStr, c_char};

use ash::{khr, vk};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use crate::RenderError;

/// The Khronos validation layer, enabled in debug builds when installed.
const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";

/// The Vulkan library entry point and the instance created from it.
///
/// Must be dropped after everything created from the instance (surface, device).
pub struct Context {
    entry: ash::Entry,
    instance: ash::Instance,
}

impl Context {
    /// Loads Vulkan and creates an instance able to present to the given display.
    pub fn new(display: RawDisplayHandle) -> Result<Self, RenderError> {
        // SAFETY: loading the library has no preconditions beyond it being a genuine Vulkan loader.
        let entry = unsafe { ash::Entry::load()? };

        let extensions = ash_window::enumerate_required_extensions(display)?;
        let layers = enabled_layers(&entry)?;

        let app_info = vk::ApplicationInfo::default()
            .application_name(c"Voxel Adventure")
            .api_version(vk::make_api_version(0, 1, 0, 0));
        let create_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(extensions)
            .enabled_layer_names(&layers);

        // SAFETY: `create_info` and everything it points to outlive the call.
        let instance = unsafe { entry.create_instance(&create_info, None)? };
        Ok(Self { entry, instance })
    }

    /// The Vulkan library entry point.
    pub fn entry(&self) -> &ash::Entry {
        &self.entry
    }

    /// The Vulkan instance.
    pub fn instance(&self) -> &ash::Instance {
        &self.instance
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: the surface and device were destroyed first, guaranteed by the
        // field order of `VulkanRenderer`, and nothing uses the instance afterwards.
        unsafe { self.instance.destroy_instance(None) };
    }
}

/// Returns the validation layer name in debug builds if it is installed, otherwise nothing.
fn enabled_layers(entry: &ash::Entry) -> Result<Vec<*const c_char>, RenderError> {
    if !cfg!(debug_assertions) {
        return Ok(Vec::new());
    }
    // SAFETY: `entry` holds a successfully loaded Vulkan library, and the call takes no handles.
    let available = unsafe { entry.enumerate_instance_layer_properties()? };
    let found = available.iter().any(|layer| {
        layer
            .layer_name_as_c_str()
            .is_ok_and(|n| n == VALIDATION_LAYER)
    });
    Ok(if found {
        vec![VALIDATION_LAYER.as_ptr()]
    } else {
        Vec::new()
    })
}

/// A Vulkan surface: the window area the GPU presents images to.
pub struct Surface {
    loader: khr::surface::Instance,
    handle: vk::SurfaceKHR,
}

impl Surface {
    /// Creates a surface for a native window.
    ///
    /// # Safety
    /// `display` and `window` must be valid handles of a live window, and that window
    /// must stay alive for as long as the returned surface exists.
    pub unsafe fn new(
        context: &Context,
        display: RawDisplayHandle,
        window: RawWindowHandle,
    ) -> Result<Self, RenderError> {
        // SAFETY: the caller guarantees the handles are valid and outlive the surface.
        let handle = unsafe {
            ash_window::create_surface(context.entry(), context.instance(), display, window, None)?
        };
        let loader = khr::surface::Instance::new(context.entry(), context.instance());
        Ok(Self { loader, handle })
    }

    /// Function table for surface queries.
    pub fn loader(&self) -> &khr::surface::Instance {
        &self.loader
    }

    /// The raw surface handle.
    pub fn handle(&self) -> vk::SurfaceKHR {
        self.handle
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: the swapchain created from this surface was destroyed first
        // (field order of `VulkanRenderer`).
        unsafe { self.loader.destroy_surface(self.handle, None) };
    }
}
