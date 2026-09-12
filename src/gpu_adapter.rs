//! Puts Servo's ANGLE device on the same GPU as Godot's.
//!
//! surfman opens ANGLE on "the first adapter that is not Intel"
//! (`Connection::create_hardware_adapter`), and Godot picks its own device by
//! type score, or by whatever `--gpu-index` says. On a machine with more than
//! one GPU the two can land on different adapters, and a DXGI NT handle only
//! reopens on the adapter that exported it. The share would then fail and
//! `bridge::create` would fall back to CPU readback — working, but a full frame
//! copy per frame.
//!
//! So Godot is asked which adapter it is on, and surfman is handed that one.
//! Both renderers can answer: D3D12 through `ID3D12Device::GetAdapterLuid`, and
//! Vulkan through `VkPhysicalDeviceIDProperties::deviceLUID`. Anything else
//! (the Compatibility renderer, a driver with no RenderingDevice) leaves the
//! choice to surfman.

use godot::classes::RenderingServer;
use godot::prelude::*;
use windows::core::Interface;
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Direct3D12::ID3D12Device;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};

/// The adapter Godot is rendering on, if it can be identified.
pub fn godot_adapter() -> Option<surfman::Adapter> {
    let driver = RenderingServer::singleton()
        .get_current_rendering_driver_name()
        .to_string();
    let luid = match driver.as_str() {
        "d3d12" => d3d12_luid(),
        "vulkan" => vulkan_luid(),
        // No RenderingDevice, so no device to match, and no shared texture
        // either: the Compatibility renderer takes the readback path anyway.
        _ => return None,
    }?;

    let Some(adapter) = adapter_with_luid(luid) else {
        godot_warn!(
            "godot-servo: no DXGI adapter matches the one Godot renders on; \
             Servo may end up on another GPU, which costs the shared texture."
        );
        return None;
    };

    // SAFETY: `into_raw` hands over the reference this function owns, which is
    // exactly what `ComPtr::from_raw` takes. The two `IDXGIAdapter` types are
    // the same COM interface; surfman's is the `winapi` one.
    let owned = unsafe {
        wio::com::ComPtr::from_raw(adapter.into_raw() as *mut winapi::shared::dxgi::IDXGIAdapter)
    };
    Some(surfman::Adapter::from_dxgi_adapter(owned))
}

/// Godot's adapter under the D3D12 renderer.
fn d3d12_luid() -> Option<LUID> {
    let handle = crate::bridge::godot_logical_device().ok()?;
    let pointer = handle as *mut core::ffi::c_void;

    // SAFETY: under this renderer Godot's logical device is a live
    // `ID3D12Device`, and the borrow does not outlive the call below.
    unsafe {
        let device: &ID3D12Device = Interface::from_raw_borrowed(&pointer)?;
        Some(device.GetAdapterLuid())
    }
}

/// Godot's adapter under the Vulkan renderer.
///
/// `deviceLUID` is what the Windows drivers fill in to name the DXGI adapter a
/// physical device belongs to, which is what makes the two comparable.
fn vulkan_luid() -> Option<LUID> {
    use ash::vk;

    let device = crate::bridge::vulkan::device::VulkanDevice::from_godot().ok()?;
    let mut id_properties = vk::PhysicalDeviceIDProperties::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut id_properties);

    // SAFETY: the instance and the physical device are Godot's own live
    // handles, and `get_physical_device_properties2` is core since Vulkan 1.1.
    unsafe {
        device
            .instance
            .get_physical_device_properties2(device.physical_device, &mut properties);
    }

    if id_properties.device_luid_valid == vk::FALSE {
        return None;
    }
    let low: [u8; 4] = id_properties.device_luid[0..4].try_into().ok()?;
    let high: [u8; 4] = id_properties.device_luid[4..8].try_into().ok()?;
    Some(LUID {
        LowPart: u32::from_ne_bytes(low),
        HighPart: i32::from_ne_bytes(high),
    })
}

/// The DXGI adapter with this LUID, out of everything on the system.
fn adapter_with_luid(luid: LUID) -> Option<IDXGIAdapter> {
    // SAFETY: plain DXGI enumeration. `EnumAdapters1` answers with an error
    // once the index runs past the last adapter, which ends the loop.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut index = 0u32;
        loop {
            let adapter = factory.EnumAdapters1(index).ok()?;
            let description = adapter.GetDesc1().ok()?;
            if description.AdapterLuid.LowPart == luid.LowPart
                && description.AdapterLuid.HighPart == luid.HighPart
            {
                return adapter.cast().ok();
            }
            index += 1;
        }
    }
}
