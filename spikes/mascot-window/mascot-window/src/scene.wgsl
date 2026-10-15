// The test picture the spike sends: soft-edged shapes with every alpha level at their borders, written as PREMULTIPLIED
// colour (rgb already multiplied by alpha), because that is what the renderer hands to outputs (render-api's
// AlphaEncoding::Premultiplied). Colour numbers are display-encoded values, stored as-is in a non-sRGB 8-bit target.
//
// `truth` mode draws the same picture composited over `bg` into an opaque frame, which is the reference for what a
// receiver should end up showing after it blends our frame over that background.

struct Params {
    size: vec2<f32>,
    t: f32,
    frame: u32,
    ticks_hi: u32,
    ticks_lo: u32,
    truth: u32,
    pad: u32,
    bg: vec4<f32>,
}

@group(0) @binding(0) var<uniform> p: Params;

const CELL: f32 = 16.0;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle that covers the whole target.
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

// `src` over `dst`, both premultiplied.
fn over(src: vec4<f32>, dst: vec4<f32>) -> vec4<f32> {
    return src + dst * (1.0 - src.a);
}

// Coverage of a disc with a smooth edge `feather` pixels wide, ending at radius `r`.
fn disc(px: vec2<f32>, centre: vec2<f32>, r: f32, feather: f32) -> f32 {
    return 1.0 - smoothstep(r - feather, r, length(px - centre));
}

// Coverage of an axis-aligned box with rounded corners and a 1.5 pixel anti-aliased edge.
fn round_box(px: vec2<f32>, centre: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let q = abs(px - centre) - half_size + vec2<f32>(radius);
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
    return clamp(0.5 - d / 1.5, 0.0, 1.0);
}

fn bit_of(value: u32, index: u32) -> f32 {
    return f32((value >> index) & 1u);
}

// One cell of a tag row: 1.0 for white, 0.0 for black. Mirrors frame_tag.rs.
fn tag_ticks(cell: u32) -> f32 {
    if cell < 32u {
        return bit_of(p.ticks_hi, 31u - cell);
    }
    return bit_of(p.ticks_lo, 63u - cell);
}

fn tag_frame(cell: u32) -> f32 {
    if cell < 32u {
        return bit_of(p.frame, 31u - cell);
    }
    return 1.0 - bit_of(p.frame, 63u - cell);
}

// A flat patch with a known straight colour and alpha, used to check the exact bytes that arrive.
fn known_patch(px: vec2<f32>, index: f32, straight: vec3<f32>, alpha: f32) -> vec4<f32> {
    let origin = vec2<f32>(40.0 + index * 64.0, p.size.y - 96.0);
    let inside = all(px >= origin) && all(px < origin + vec2<f32>(48.0));
    return select(vec4<f32>(0.0), vec4<f32>(straight * alpha, alpha), inside);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = frag.xy;

    // Tag rows: opaque black or white cells.
    let cell = u32(floor(px.x / CELL));
    if cell < 64u {
        if px.y < CELL {
            let v = tag_ticks(cell);
            return vec4<f32>(v, v, v, 1.0);
        }
        if px.y < 2.0 * CELL || px.y >= p.size.y - CELL {
            let v = tag_frame(cell);
            return vec4<f32>(v, v, v, 1.0);
        }
    }

    // Stamp column: every picture row of x in 1024..1040 holds the low byte of the frame number as a grey level, so a
    // receiver can see exactly where in the image two frames meet.
    if px.x >= 1024.0 && px.x < 1040.0 && px.y >= 2.0 * CELL && px.y < p.size.y - CELL {
        let g = f32(p.frame & 255u) / 255.0;
        return vec4<f32>(g, g, g, 1.0);
    }

    // Picture area, bottom layer first.
    var acc = vec4<f32>(0.0);
    // A half-transparent black slab: pure alpha, no colour, like a drop shadow.
    let slab = round_box(px, vec2<f32>(p.size.x * 0.62, p.size.y * 0.58), vec2<f32>(280.0, 150.0), 40.0) * 0.5;
    acc = over(vec4<f32>(0.0, 0.0, 0.0, slab), acc);
    // A saturated orange disc with a narrow feather: edge pixels keep their hue only if alpha is handled right.
    let orange = disc(px, vec2<f32>(p.size.x * 0.33, p.size.y * 0.55), 200.0, 30.0);
    acc = over(vec4<f32>(vec3<f32>(1.0, 0.45, 0.10) * orange, orange), acc);
    // A white disc with a wide feather that circles: white over dark is where a wrong blend shows as a halo.
    let centre = vec2<f32>(p.size.x * 0.5 + 250.0 * cos(p.t), p.size.y * 0.45 + 120.0 * sin(p.t));
    let white = disc(px, centre, 240.0, 70.0);
    acc = over(vec4<f32>(vec3<f32>(white), white), acc);
    // Known-value patches: straight colour and alpha, so their stored bytes are alpha-scaled.
    acc = over(known_patch(px, 0.0, vec3<f32>(1.0, 0.5, 0.0), 0.5), acc);
    acc = over(known_patch(px, 1.0, vec3<f32>(0.0, 0.6, 1.0), 0.25), acc);
    acc = over(known_patch(px, 2.0, vec3<f32>(1.0, 1.0, 1.0), 0.1), acc);
    acc = over(known_patch(px, 3.0, vec3<f32>(1.0, 0.0, 0.0), 1.0), acc);
    acc = over(known_patch(px, 4.0, vec3<f32>(0.0, 0.0, 0.0), 0.0), acc);

    if p.truth != 0u {
        return vec4<f32>(acc.rgb + p.bg.rgb * (1.0 - acc.a), 1.0);
    }
    return acc;
}
