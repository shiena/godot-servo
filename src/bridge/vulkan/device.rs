//! Godot's Vulkan objects, as something `ash` can call through.
//!
//! Godot already created the instance, the physical device and the logical
//! device; `get_driver_resource()` hands out the raw handles. Loading the entry
//! points is all that is left. `Entry::load()` opens the system Vulkan loader a
//! second time, which is a reference count on the one Godot is already using,
//! and `vkGetInstanceProcAddr` from it resolves against any instance.

use std::ffi::CStr;

use ash::vk::{self, Handle};
use godot::classes::rendering_device::DriverResource;
use godot::classes::RenderingServer;
use godot::obj::Singleton;

use crate::bridge::{driver_resource, godot_logical_device};

pub struct VulkanDevice {
    /// Owns the loader the entry points below were resolved from. Nothing reads
    /// it, but dropping it would invalidate them.
    _entry: ash::Entry,
    pub instance: ash::Instance,
    pub device: ash::Device,
    pub physical_device: vk::PhysicalDevice,
}

impl VulkanDevice {
    pub fn from_godot() -> Result<Self, String> {
        let instance_handle = driver_resource(DriverResource::TOPMOST_OBJECT, "VkInstance")?;
        let physical_handle = driver_resource(DriverResource::PHYSICAL_DEVICE, "VkPhysicalDevice")?;
        let device_handle = godot_logical_device()?;

        // SAFETY: the handles come straight from Godot's own live device, and
        // the loader is the same one Godot resolved them through.
        unsafe {
            let entry = ash::Entry::load()
                .map_err(|error| format!("could not load the Vulkan loader: {error}"))?;
            let instance =
                ash::Instance::load(entry.static_fn(), vk::Instance::from_raw(instance_handle));
            let device = ash::Device::load(instance.fp_v1_0(), vk::Device::from_raw(device_handle));

            Ok(Self {
                _entry: entry,
                instance,
                device,
                physical_device: vk::PhysicalDevice::from_raw(physical_handle),
            })
        }
    }

    /// Check that the entry points a path calls through are really on the device.
    ///
    /// The device is the only authority worth asking. An extension can be listed
    /// and its functions still be absent, and resolving through
    /// `vkGetInstanceProcAddr` instead can hand back a stub that answers rather
    /// than fails — `godot-xreal` measured exactly that on an Adreno 710, an
    /// `AHardwareBuffer` properties call that "succeeded" with no memory types.
    /// `ash` turns an unresolved pointer into a panicking placeholder, so a
    /// wrapper this has not cleared must never be called.
    pub fn require_functions(&self, names: &[&CStr]) -> Result<(), String> {
        for name in names {
            // SAFETY: a lookup on a live device, with a nul-terminated name.
            let resolved = unsafe {
                self.instance
                    .get_device_proc_addr(self.device.handle(), name.as_ptr())
            };
            if resolved.is_none() {
                return Err(format!(
                    "{} does not resolve on Godot's Vulkan device, so its extension is not \
                     enabled there",
                    name.to_string_lossy()
                ));
            }
        }
        Ok(())
    }

    /// The index of a memory type the imported or exported allocation can use.
    ///
    /// Device-local for preference, but not as a requirement: which types an
    /// external handle is compatible with is the driver's call, and on some it
    /// does not intersect the device-local ones.
    pub fn memory_type_index(&self, allowed: u32) -> Result<u32, String> {
        // An empty mask is its own diagnosis. It is what an import extension
        // reports when its entry point resolved to a stub rather than to the
        // driver's implementation.
        if allowed == 0 {
            return Err(
                "the driver offered no memory type for the handle. The extension behind it is \
                 very likely not enabled on Godot's device, however it answered"
                    .into(),
            );
        }

        // SAFETY: the physical device came from Godot and is live.
        let properties = unsafe {
            self.instance
                .get_physical_device_memory_properties(self.physical_device)
        };
        let usable = |index: u32| allowed & (1 << index) != 0;
        let device_local = |index: u32| {
            properties.memory_types[index as usize]
                .property_flags
                .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        };

        (0..properties.memory_type_count)
            .find(|index| usable(*index) && device_local(*index))
            .or_else(|| (0..properties.memory_type_count).find(|index| usable(*index)))
            .ok_or_else(|| format!("no Vulkan memory type is in mask {allowed:#x}"))
    }

    /// Let the device finish with a shared image before it is destroyed.
    ///
    /// Godot may still have frames in flight that reference it. `force_sync()`
    /// drains the rendering server's queue first, so by the time the wait starts
    /// the render thread is no longer submitting and there is no second caller
    /// to race with.
    pub fn wait_until_idle(&self) {
        RenderingServer::singleton().force_sync();
        // SAFETY: nothing else holds the queues, per the comment above.
        let _ = unsafe { self.device.device_wait_idle() };
    }
}
