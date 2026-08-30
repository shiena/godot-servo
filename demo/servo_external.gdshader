// Android の AHardwareBuffer 経路で使うシェーダ。
//
// 共有バッファは GL_TEXTURE_EXTERNAL_OES として届くので、`sampler2D` では読めない
// (黒くなる)。Godot は `samplerExternalOES` を uniform の型として受け付け、
// GLES3 以外では `sampler2D` に #define される。
shader_type spatial;
render_mode unshaded, cull_disabled;

uniform samplerExternalOES servo_texture;

void fragment() {
	ALBEDO = texture(servo_texture, UV).rgb;
}
