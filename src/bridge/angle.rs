//! The ANGLE side of the Windows sharing paths.
//!
//! Servo draws through ANGLE, which is D3D11, so both Windows backends start the
//! same way: allocate a shared texture on ANGLE's `ID3D11Device`, export it as a
//! DXGI NT handle, and hand that to whatever Godot is running on. Only the far
//! side differs — `ID3D12Device::OpenSharedHandle` for D3D12,
//! `VkImportMemoryWin32HandleInfoKHR` for Vulkan.
//!
//! Every frame the same texture is wrapped as a temporary EGL pbuffer so ANGLE
//! can see it, and Servo's FBO is blitted in, flipped. The D3D11 texture itself
//! is reused, so neither the handle nor the RID on Godot's side ever changes.

use dpi::PhysicalSize;
use euclid::default::Size2D;
use windows::core::{IUnknown, Interface, PCWSTR};
use windows::Win32::Foundation::{GENERIC_ALL, HANDLE};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIResource1;

use crate::rendering_context::GodotRenderingContext;

/// ANGLE's `ID3D11Device`, the one surfman draws through.
pub fn d3d11_device(context: &GodotRenderingContext) -> Result<ID3D11Device, String> {
    let native_device = context.device().native_device();
    if native_device.d3d11_device.is_null() {
        return Err(
            "surfman is not using the ANGLE backend, so there is no D3D11 device. \
             Was the servo `no-wgl` feature enabled?"
                .into(),
        );
    }
    // SAFETY: surfman hands back a live `ID3D11Device` it holds a reference to.
    unsafe {
        IUnknown::from_raw(native_device.d3d11_device as *mut _)
            .cast::<ID3D11Device>()
            .map_err(|error| format!("cast to ID3D11Device: {error}"))
    }
}

/// Allocate a texture on ANGLE's device that another API can open.
///
/// The returned handle is a duplicate onto the same memory. The caller closes it
/// once the far side has opened it; the texture stays alive through its own COM
/// reference.
///
/// No keyed mutex. The pbuffers ANGLE allocates for itself carry one, the way
/// mozangle builds it, but neither Godot's `RenderingDevice` nor Vulkan can
/// acquire or release it. Allocating the texture here instead leaves surfman's
/// synchronization mode as `None`.
///
/// # Safety
///
/// `device` must be a live `ID3D11Device`.
pub unsafe fn create_shared_texture(
    device: &ID3D11Device,
    size: PhysicalSize<u32>,
) -> Result<(ID3D11Texture2D, HANDLE), String> {
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
        // No KEYEDMUTEX: nothing on the other side can acquire it.
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
    };

    let mut created: Option<ID3D11Texture2D> = None;
    device
        .CreateTexture2D(&descriptor, None, Some(&mut created))
        .map_err(|error| format!("D3D11 CreateTexture2D: {error}"))?;
    let texture = created.ok_or("D3D11 CreateTexture2D returned null")?;

    let resource: IDXGIResource1 = texture
        .cast()
        .map_err(|error| format!("cast to IDXGIResource1: {error}"))?;
    let handle = resource
        .CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null())
        .map_err(|error| format!("DXGI CreateSharedHandle: {error}"))?;

    Ok((texture, handle))
}

/// Blit Servo's frame into the shared texture, flipped.
///
/// The texture lives on ANGLE's D3D11 device but not in its GL namespace, so it
/// is wrapped as an EGL pbuffer for the duration of the blit. The wrapper is
/// thrown away every frame; the texture inside stays alive through its COM
/// reference.
pub fn blit_into(
    context: &GodotRenderingContext,
    texture: &ID3D11Texture2D,
    size: PhysicalSize<u32>,
) -> Result<(), String> {
    let source_fbo = context.framebuffer();
    let device = context.device();
    let mut gl_context = context.context_mut();

    // SAFETY: `texture` is a live D3D11 texture on the device surfman is using,
    // and the wrapper is torn down again below whatever the blit does.
    let surface_texture = unsafe {
        let raw = texture.clone().into_raw();
        let com_ptr = wio::com::ComPtr::from_raw(raw as *mut _);
        device
            .create_surface_texture_from_texture(
                &mut gl_context,
                &Size2D::new(size.width as i32, size.height as i32),
                com_ptr,
            )
            .map_err(|error| format!("create_surface_texture_from_texture: {error:?}"))?
    };

    let gl_texture = device
        .surface_texture_object(&surface_texture)
        .ok_or("ANGLE returned no GL texture for the shared pbuffer")?;

    // SAFETY: Servo's context is current and the texture above is live.
    let blit_result = unsafe { super::blit_flipped(context.glow(), source_fbo, gl_texture, size) };

    let mut surface = device
        .destroy_surface_texture(&mut gl_context, surface_texture)
        .map_err(|(error, _)| format!("destroy_surface_texture: {error:?}"))?;
    device
        .destroy_surface(&mut gl_context, &mut surface)
        .map_err(|error| format!("destroy_surface: {error:?}"))?;
    blit_result
}
