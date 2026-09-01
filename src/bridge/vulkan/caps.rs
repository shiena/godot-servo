//! Whether this Godot can enable, and report, the Vulkan device extensions.
//!
//! Importing external memory needs device extensions, and a device's extensions
//! are fixed when it is created — long before a GDExtension is loaded. Two
//! pieces of API make that reachable from a project, both added by
//! [godotengine/godot#114940](https://github.com/godotengine/godot/pull/114940):
//!
//! - the `rendering/rendering_device/vulkan/additional_device_extensions`
//!   project setting, the only way to get them enabled at all, and
//! - `RenderingDevice.get_device_enabled_extensions()`, which reports what
//!   actually came back.
//!
//! Neither exists in a stock Godot. The setting alone would not be enough to go
//! on either: Godot registers those extensions as optional, so a name listed
//! there says nothing about whether the driver honoured it. The method is
//! therefore both the capability probe and the answer, and its absence is what
//! sends this whole path back to CPU readback.

use godot::classes::RenderingServer;
use godot::prelude::*;

/// The project setting that asks Godot for extra device extensions.
pub const SETTING: &str = "rendering/rendering_device/vulkan/additional_device_extensions";

/// The method that reports what the device ended up with.
const METHOD: &str = "get_device_enabled_extensions";

/// The device extensions Godot's Vulkan device has enabled.
///
/// `Err` carries the reason there is no answer, in a form the caller prints.
pub fn enabled_extensions() -> Result<Vec<String>, String> {
    let mut rendering_device = RenderingServer::singleton()
        .get_rendering_device()
        .ok_or("there is no RenderingDevice")?;

    if !rendering_device.has_method(METHOD) {
        return Err(format!(
            "this Godot has no RenderingDevice.{METHOD}(), so a project has no way \
             to enable the device extensions an imported texture needs \
             (godotengine/godot#114940)"
        ));
    }

    // Not in the compiled bindings, so this goes through a dynamic call. The
    // `has_method` check above is what keeps that from raising an error.
    let names: PackedStringArray = rendering_device
        .call(METHOD, &[])
        .try_to()
        .map_err(|error| format!("{METHOD}() returned something unexpected: {error}"))?;

    Ok(names.as_slice().iter().map(GString::to_string).collect())
}

/// Check what a sharing path needs against what the device enabled.
pub fn require(enabled: &[String], needed: &[&str]) -> Result<(), String> {
    let missing: Vec<&str> = needed
        .iter()
        .copied()
        .filter(|name| !enabled.iter().any(|enabled| enabled == name))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }

    Err(format!(
        "the Vulkan device has not enabled {}. Add {} to the '{SETTING}' project \
         setting and restart. Godot registers the names there as optional, so one \
         the driver does not support is dropped rather than refused, and turns up \
         here as missing",
        missing.join(" and "),
        if missing.len() == 1 { "it" } else { "them" },
    ))
}
