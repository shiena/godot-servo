// The canvas_item counterpart of servo_external.gdshader, for a Control.
//
// Same reason: on Android's Compatibility renderer the shared buffer arrives as
// a GL_TEXTURE_EXTERNAL_OES, which a `sampler2D` reads as black. Godot accepts
// `samplerExternalOES` as a uniform type and #defines it to `sampler2D` outside
// GLES3.
//
// The node keeps its texture assigned so it still has something to size itself
// by; what is drawn comes from this uniform instead.
shader_type canvas_item;

uniform samplerExternalOES servo_texture;

void fragment() {
	COLOR = texture(servo_texture, UV);
}
