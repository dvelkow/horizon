struct Hud { size: vec4<f32> }
@group(0) @binding(0) var<uniform> hud: Hud;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;
struct Out {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}
@vertex fn vs_main(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>, @location(3) depth: f32) -> Out {
    let pixels = pos * hud.size.z;
    var out: Out;
    out.clip = vec4(pixels.x / hud.size.x * 2.0 - 1.0, 1.0 - pixels.y / hud.size.y * 2.0, depth, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}
@fragment fn fs_main(in: Out) -> @location(0) vec4<f32> {
    // u >= 2: flat vertex colour masked by the texture's alpha (enchantment glint).
    // Implicit derivatives must be evaluated before fragment-dependent branches
    // on WebGPU. The same sample serves both the glint mask and ordinary HUD.
    let glint = in.uv.x >= 2.0;
    let uv = in.uv - select(vec2(0.0), vec2(2.0, 0.0), glint);
    let texel = textureSample(atlas, atlas_sampler, uv);
    if glint {
        let m = texel.a * in.color.a;
        if m < 0.004 { discard; }
        return vec4(in.color.rgb, m);
    }
    let c = texel * in.color;
    if c.a < 0.004 { discard; }
    return c;
}
@fragment fn fs_crosshair(in: Out) -> @location(0) vec4<f32> {
    let c = textureSample(atlas, atlas_sampler, in.uv) * in.color;
    if c.a == 0.0 { discard; }
    return c;
}
