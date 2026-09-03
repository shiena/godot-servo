// The shader used by the Android AHardwareBuffer path.
//
// The shared buffer arrives as a GL_TEXTURE_EXTERNAL_OES, which a `sampler2D`
// cannot read: it comes out black. Godot accepts `samplerExternalOES` as a uniform
// type and #defines it to `sampler2D` outside GLES3.
shader_type spatial;
render_mode unshaded, cull_disabled;

uniform samplerExternalOES servo_texture;

void fragment() {
	ALBEDO = texture(servo_texture, UV).rgb;
}
