//! The Windows / D3D12 GPU sharing path.
//!
//! ANGLE, on Servo's side, is D3D11 while Godot is D3D12, so a shared DXGI NT
//! handle joins the two.
//!
//! 1. Allocate a `SHARED | SHARED_NTHANDLE` texture on ANGLE's `ID3D11Device`
//! 2. Export an NT handle with `IDXGIResource1::CreateSharedHandle`
//! 3. Open it on Godot's `ID3D12Device::OpenSharedHandle`
//! 4. Wrap it as a Godot texture with `RenderingDevice.texture_create_from_extension`
//!
//! Every frame the shared texture is wrapped as a temporary EGL pbuffer and
//! Servo's FBO is blitted into it, flipped vertically. The D3D11 texture itself
//! is reused, so the RID on Godot's side never changes.
//!
//! No keyed mutex. The pbuffers ANGLE allocates for itself carry one, the way
//! mozangle builds it, but Godot's `RenderingDevice` cannot acquire or release
//! it. Allocating the texture here instead leaves surfman's synchronization
//! mode as `None`.

use dpi::PhysicalSize;
use euclid::default::Size2D;
use glow::HasContext;
use godot::classes::rendering_device::{DataFormat, TextureSamples, TextureType, TextureUsageBits};
use godot::classes::{
    RdTextureFormat, RdTextureView, RenderingDevice, RenderingServer, Texture2D, Texture2Drd,
};
use godot::prelude::*;
use windows::core::{IUnknown, Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIResource1;

use super::TextureBridge;
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
        let d3d11_device = angle_d3d11_device(context)?;
        let godot_device = super::godot_logical_device()?;

        let (d3d11_texture, d3d12_resource) =
            unsafe { create_shared_texture(&d3d11_device, godot_device, size)? };

        let mut rendering_device = RenderingServer::singleton()
            .get_rendering_device()
            .ok_or("no RenderingDevice")?;

        let imported = import_rd_texture(&mut rendering_device, &d3d12_resource, size)?;
        let owned = create_owned_rd_texture(&mut rendering_device, size)?;

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
        let source_fbo = context.framebuffer();
        let device = context.device();
        let mut gl_context = context.context_mut();

        // Wrap the shared texture in something ANGLE can see: an EGL pbuffer.
        let surface_texture = unsafe {
            let raw = self.d3d11_texture.clone().into_raw();
            let com_ptr = wio::com::ComPtr::from_raw(raw as *mut _);
            device
                .create_surface_texture_from_texture(
                    &mut gl_context,
                    &Size2D::new(self.size.width as i32, self.size.height as i32),
                    com_ptr,
                )
                .map_err(|error| format!("create_surface_texture_from_texture: {error:?}"))?
        };

        let gl_texture = device
            .surface_texture_object(&surface_texture)
            .ok_or("ANGLE returned no GL texture for the shared pbuffer")?;

        let blit_result =
            unsafe { blit_flipped(context.glow(), source_fbo, gl_texture, self.size) };

        // The wrapper is thrown away every frame. The D3D11 texture inside stays
        // alive through its COM reference.
        let mut surface = device
            .destroy_surface_texture(&mut gl_context, surface_texture)
            .map_err(|(error, _)| format!("destroy_surface_texture: {error:?}"))?;
        device
            .destroy_surface(&mut gl_context, &mut surface)
            .map_err(|error| format!("destroy_surface: {error:?}"))?;
        blit_result?;

        // Copy the imported texture into the Godot-owned one.
        //
        // The detour looks pointless but is required. Godot's D3D12 driver only
        // accepts textures that own an allocation in `texture_create_shared()`
        // (the DEBUG_ENABLED check in `_texture_create_shared_from_slice`) and
        // rejects imported ones. `Texture2DRD` creates that shared view
        // internally, so handing it the imported texture renders white. The
        // Vulkan driver explicitly permits `created_from_extension`, which makes
        // this a gap in the D3D12 driver.
        //
        // The copy stays on the GPU, so no CPU round trip appears.
        let mut rendering_device = RenderingServer::singleton()
            .get_rendering_device()
            .ok_or("no RenderingDevice")?;
        let extent = Vector3::new(self.size.width as f32, self.size.height as f32, 1.0);
        let error = rendering_device.texture_copy(
            self.imported,
            self.owned,
            Vector3::ZERO,
            Vector3::ZERO,
            extent,
            0,
            0,
            0,
            0,
        );
        if error != godot::global::Error::OK {
            return Err(format!("texture_copy failed: {error:?}"));
        }
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "d3d12-shared-nt-handle"
    }

    fn release(&mut self) {
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

/// The Godot-owned texture that is actually displayed.
fn create_owned_rd_texture(
    rendering_device: &mut Gd<RenderingDevice>,
    size: PhysicalSize<u32>,
) -> Result<Rid, String> {
    let mut format = RdTextureFormat::new_gd();
    format.set_format(DataFormat::R8G8B8A8_UNORM);
    format.set_texture_type(TextureType::TYPE_2D);
    format.set_width(size.width);
    format.set_height(size.height);
    format.set_depth(1);
    format.set_array_layers(1);
    format.set_mipmaps(1);
    format.set_samples(TextureSamples::SAMPLES_1);
    format.set_usage_bits(
        TextureUsageBits::SAMPLING_BIT
            | TextureUsageBits::CAN_COPY_TO_BIT
            | TextureUsageBits::CAN_COPY_FROM_BIT,
    );

    let view = RdTextureView::new_gd();
    let rid = rendering_device.texture_create(&format, &view);
    if rid.is_valid() {
        Ok(rid)
    } else {
        Err("texture_create returned an invalid RID".into())
    }
}

/// Create a shared texture on ANGLE's D3D11 device and open it on Godot's D3D12 one.
///
/// # Safety
///
/// `godot_device` must be a live `ID3D12Device*`.
unsafe fn create_shared_texture(
    d3d11_device: &ID3D11Device,
    godot_device: u64,
    size: PhysicalSize<u32>,
) -> Result<(ID3D11Texture2D, ID3D12Resource), String> {
    let descriptor = D3D11_TEXTURE2D_DESC {
        Width: size.width,
        Height: size.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        // No KEYEDMUTEX: Godot has no way to acquire it.
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
    };

    let mut created: Option<ID3D11Texture2D> = None;
    d3d11_device
        .CreateTexture2D(&descriptor, None, Some(&mut created))
        .map_err(|error| format!("D3D11 CreateTexture2D: {error}"))?;
    let d3d11_texture = created.ok_or("D3D11 CreateTexture2D returned null")?;

    let dxgi_resource: IDXGIResource1 = d3d11_texture
        .cast()
        .map_err(|error| format!("cast to IDXGIResource1: {error}"))?;
    let nt_handle = dxgi_resource
        .CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null())
        .map_err(|error| format!("DXGI CreateSharedHandle: {error}"))?;

    let device_pointer = godot_device as *mut core::ffi::c_void;
    let d3d12_device: &ID3D12Device = Interface::from_raw_borrowed(&device_pointer)
        .ok_or("Godot returned a null ID3D12Device")?;

    let mut opened: Option<ID3D12Resource> = None;
    let open_result = d3d12_device
        .OpenSharedHandle(nt_handle, &mut opened)
        .map_err(|error| format!("D3D12 OpenSharedHandle: {error}"));

    // Both sides have opened it, so this duplicate can be closed.
    let _ = CloseHandle(nt_handle);
    open_result?;

    let d3d12_resource = opened.ok_or("D3D12 OpenSharedHandle returned null")?;
    Ok((d3d11_texture, d3d12_resource))
}

/// Blit Servo's FBO into the shared texture, flipping it vertically.
///
/// No explicit semaphore is available, so a `glFlush` afterwards stands in for
/// synchronization.
///
/// # Safety
///
/// `gl`'s context must be current and `gl_texture` must be valid.
unsafe fn blit_flipped(
    gl: &glow::Context,
    source_fbo: u32,
    gl_texture: glow::Texture,
    size: PhysicalSize<u32>,
) -> Result<(), String> {
    let destination = gl.create_framebuffer().map_err(|error| error.to_string())?;
    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(destination));
    gl.framebuffer_texture_2d(
        glow::DRAW_FRAMEBUFFER,
        glow::COLOR_ATTACHMENT0,
        glow::TEXTURE_2D,
        Some(gl_texture),
        0,
    );

    let source = std::num::NonZeroU32::new(source_fbo).map(glow::NativeFramebuffer);
    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, source);

    let width = size.width as i32;
    let height = size.height as i32;
    // GL's origin is bottom-left, D3D's is top-left. Flip on the way through.
    gl.blit_framebuffer(
        0,
        0,
        width,
        height,
        0,
        height,
        width,
        0,
        glow::COLOR_BUFFER_BIT,
        glow::NEAREST,
    );
    gl.flush();

    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
    gl.delete_framebuffer(destination);
    Ok(())
}

fn angle_d3d11_device(context: &GodotRenderingContext) -> Result<ID3D11Device, String> {
    let native_device = context.device().native_device();
    if native_device.d3d11_device.is_null() {
        return Err(
            "surfman is not using the ANGLE backend, so there is no D3D11 device. \
             Was the servo `no-wgl` feature enabled?"
                .into(),
        );
    }
    unsafe {
        IUnknown::from_raw(native_device.d3d11_device as *mut _)
            .cast::<ID3D11Device>()
            .map_err(|error| format!("cast to ID3D11Device: {error}"))
    }
}
