//! The Windows / D3D12 GPU sharing path.
//!
//! ANGLE, on Servo's side, is D3D11 while Godot is D3D12, so a shared DXGI NT
//! handle joins the two.
//!
//! 1. Allocate a `SHARED | SHARED_NTHANDLE` texture on ANGLE's `ID3D11Device`
//! 2. Export an NT handle with `IDXGIResource1::CreateSharedHandle`
//! 3. Open it on Godot's `ID3D12Device::OpenSharedHandle`
//! 4. Wrap it as a Godot texture with `RenderingDevice.texture_create_from_extension`
//! 5. Copy that into a texture Godot owns, which is what gets displayed
//!
//! Steps 1 and 2, and the per-frame blit, are shared with the Vulkan path and
//! live in [`super::angle`]. So is step 5, and
//! `super::create_owned_rd_texture` says why it is there.

use dpi::PhysicalSize;
use godot::classes::rendering_device::{DataFormat, TextureSamples, TextureType, TextureUsageBits};
use godot::classes::{RenderingDevice, RenderingServer, Texture2D, Texture2Drd};
use godot::prelude::*;
use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Resource};

use super::{angle, copy_rd_texture, create_owned_rd_texture, TextureBridge};
use crate::rendering_context::GodotRenderingContext;

pub struct D3d12Bridge {
    /// The shared texture on ANGLE's D3D11 device. Servo writes into this one.
    d3d11_texture: ID3D11Texture2D,
    /// The same memory opened on Godot's D3D12 device. Held to keep it alive.
    _d3d12_resource: ID3D12Resource,
    /// The shared resource wrapped as a Godot RD texture. The copy source.
    imported: Rid,
    /// A texture Godot allocated itself. The copy destination, and what is displayed.
    owned: Rid,
    texture: Gd<Texture2Drd>,
    size: PhysicalSize<u32>,
}

impl D3d12Bridge {
    pub fn new(context: &GodotRenderingContext, size: PhysicalSize<u32>) -> Result<Self, String> {
        let d3d11_device = angle::d3d11_device(context)?;
        let godot_device = super::godot_logical_device()?;

        // SAFETY: both devices are live, and the handle is closed once D3D12 has
        // opened it.
        let (d3d11_texture, d3d12_resource) = unsafe {
            let (texture, handle) = angle::create_shared_texture(&d3d11_device, size)?;
            let opened = open_on_d3d12(godot_device, handle);
            let _ = CloseHandle(handle);
            (texture, opened?)
        };

        let mut rendering_device = RenderingServer::singleton()
            .get_rendering_device()
            .ok_or("no RenderingDevice")?;

        let imported = import_rd_texture(&mut rendering_device, &d3d12_resource, size)?;
        let owned =
            create_owned_rd_texture(&mut rendering_device, DataFormat::R8G8B8A8_UNORM, size)?;

        let mut texture = Texture2Drd::new_gd();
        texture.set_texture_rd_rid(owned);

        Ok(Self {
            d3d11_texture,
            _d3d12_resource: d3d12_resource,
            imported,
            owned,
            texture,
            size,
        })
    }
}

impl TextureBridge for D3d12Bridge {
    fn texture(&self) -> Gd<Texture2D> {
        self.texture.clone().upcast()
    }

    fn update(&mut self, context: &GodotRenderingContext) -> Result<(), String> {
        angle::blit_into(context, &self.d3d11_texture, self.size)?;
        copy_rd_texture(self.imported, self.owned, self.size)
    }

    fn backend_name(&self) -> &'static str {
        "d3d12-shared-nt-handle"
    }

    fn release(&mut self, _context: &GodotRenderingContext) {
        self.texture.set_texture_rd_rid(Rid::Invalid);
        if let Some(mut rendering_device) = RenderingServer::singleton().get_rendering_device() {
            for rid in [&mut self.owned, &mut self.imported] {
                if rid.is_valid() {
                    rendering_device.free_rid(*rid);
                    *rid = Rid::Invalid;
                }
            }
        }
    }
}

/// Open ANGLE's shared handle on Godot's D3D12 device.
///
/// # Safety
///
/// `godot_device` must be a live `ID3D12Device*` and `handle` a DXGI NT handle.
unsafe fn open_on_d3d12(godot_device: u64, handle: HANDLE) -> Result<ID3D12Resource, String> {
    let device_pointer = godot_device as *mut core::ffi::c_void;
    let d3d12_device: &ID3D12Device = Interface::from_raw_borrowed(&device_pointer)
        .ok_or("Godot returned a null ID3D12Device")?;

    let mut opened: Option<ID3D12Resource> = None;
    d3d12_device
        .OpenSharedHandle(handle, &mut opened)
        .map_err(|error| format!("D3D12 OpenSharedHandle: {error}"))?;
    opened.ok_or_else(|| "D3D12 OpenSharedHandle returned null".into())
}

/// Wrap the shared `ID3D12Resource` as a Godot RD texture. Copy source only.
fn import_rd_texture(
    rendering_device: &mut Gd<RenderingDevice>,
    resource: &ID3D12Resource,
    size: PhysicalSize<u32>,
) -> Result<Rid, String> {
    let rid = rendering_device.texture_create_from_extension(
        TextureType::TYPE_2D,
        DataFormat::R8G8B8A8_UNORM,
        TextureSamples::SAMPLES_1,
        TextureUsageBits::SAMPLING_BIT | TextureUsageBits::CAN_COPY_FROM_BIT,
        resource.as_raw() as u64,
        size.width as u64,
        size.height as u64,
        1,
        1,
    );
    if rid.is_valid() {
        Ok(rid)
    } else {
        Err("texture_create_from_extension returned an invalid RID".into())
    }
}
