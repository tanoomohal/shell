// Instanced rectangles in physical pixel space, with optional rounded
// corners. Used for cell backgrounds, the cursor, selection, and all chrome.

struct Params {
    screen: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;

struct QuadIn {
    // x, y, width, height in physical pixels, y growing downward.
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
    // Corner radius in physical pixels. Zero means a hard-edged rect.
    @location(2) corner_radius: f32,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Position relative to the quad's center, for the distance field.
    @location(1) local: vec2<f32>,
    @location(2) half_size: vec2<f32>,
    @location(3) corner_radius: f32,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, quad: QuadIn) -> VsOut {
    // Triangle-strip corners: (0,0) (1,0) (0,1) (1,1).
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let p = quad.rect.xy + corner * quad.rect.zw;
    let half_size = quad.rect.zw * 0.5;

    var out: VsOut;
    out.pos = vec4<f32>(
        p.x / params.screen.x * 2.0 - 1.0,
        1.0 - p.y / params.screen.y * 2.0,
        0.0,
        1.0,
    );
    out.color = quad.color;
    out.local = p - (quad.rect.xy + half_size);
    out.half_size = half_size;
    out.corner_radius = quad.corner_radius;
    return out;
}

/// Signed distance to a rounded box centered at the origin.
fn sd_rounded_box(p: vec2<f32>, half_size: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half_size + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0, 0.0))) - r;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Hard-edged quads return early rather than going through the distance
    // field. Cell backgrounds tile edge-to-edge, and antialiasing their
    // borders would leave visible seams between adjacent cells.
    if (in.corner_radius <= 0.0) {
        return in.color;
    }

    let r = min(in.corner_radius, min(in.half_size.x, in.half_size.y));
    let d = sd_rounded_box(in.local, in.half_size, r);
    // One pixel of coverage-based antialiasing across the boundary.
    let alpha = 1.0 - smoothstep(-0.5, 0.5, d);
    return vec4<f32>(in.color.rgb, in.color.a * alpha);
}
