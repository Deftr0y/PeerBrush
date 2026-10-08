// CPU-compatible byte transforms and 16-bit premultiplied Gaussian box passes.
struct Params {
    dims: vec4<u32>, // width, height, mode/axis, dispatch columns/radius
    factors: vec4<f32>,
    shadows: vec4<f32>,
    midtones: vec4<f32>,
    highlights: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> source: array<u32>;
@group(0) @binding(1) var<storage, read_write> destination: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> table: array<u32>;
@group(0) @binding(4) var<storage, read> original: array<u32>;

fn rgba(value: u32) -> vec4<u32> {
    return vec4<u32>(value & 255u, (value >> 8u) & 255u, (value >> 16u) & 255u, value >> 24u);
}
fn packed(value: vec4<u32>) -> u32 {
    return value.x | (value.y << 8u) | (value.z << 16u) | (value.w << 24u);
}
fn premult(index: u32) -> vec4<u32> {
    let first = source[index * 2u];
    let second = source[index * 2u + 1u];
    return vec4<u32>(first & 65535u, first >> 16u, second & 65535u, second >> 16u);
}
fn store_premult(index: u32, value: vec4<u32>) {
    destination[index * 2u] = value.x | (value.y << 16u);
    destination[index * 2u + 1u] = value.z | (value.w << 16u);
}
fn rounded_byte(value: f32) -> u32 {
    // Rust's round() rounds positive ties upwards, unlike WGSL round().
    return u32(floor(clamp(value, 0.0, 255.0) + 0.5));
}
fn bytes(value: vec3<f32>) -> vec3<u32> {
    return vec3<u32>(rounded_byte(value.x), rounded_byte(value.y), rounded_byte(value.z));
}
fn luminance(value: vec3<f32>) -> f32 {
    return value.x * 0.2126 + value.y * 0.7152 + value.z * 0.0722;
}
fn tone_weight(low: f32, high: f32, value: f32) -> f32 {
    let t = clamp((value - low) / (high - low), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}
fn remainder(value: f32, divisor: f32) -> f32 {
    return value - floor(value / divisor) * divisor;
}
fn hsl(rgb: vec3<f32>) -> vec3<f32> {
    let maximum = max(max(rgb.x, rgb.y), rgb.z);
    let minimum = min(min(rgb.x, rgb.y), rgb.z);
    let delta = maximum - minimum;
    var lightness = (maximum + minimum) * 0.5;
    var hue = 0.0;
    var saturation = 0.0;
    if delta > 0.00000011920928955078125 {
        if maximum == rgb.x { hue = remainder((rgb.y - rgb.z) / delta, 6.0); }
        else if maximum == rgb.y { hue = (rgb.z - rgb.x) / delta + 2.0; }
        else { hue = (rgb.x - rgb.y) / delta + 4.0; }
        saturation = delta / max(1.0 - abs(2.0 * lightness - 1.0), 0.00000011920928955078125);
    }
    hue = remainder(hue + params.factors.x, 6.0);
    saturation = clamp(saturation * (1.0 + params.factors.y), 0.0, 1.0);
    if params.factors.z < 0.0 { lightness *= 1.0 + params.factors.z; }
    else { lightness += (1.0 - lightness) * params.factors.z; }
    let c = (1.0 - abs(2.0 * lightness - 1.0)) * saturation;
    let x = c * (1.0 - abs(remainder(hue, 2.0) - 1.0));
    let m = lightness - c * 0.5;
    var color: vec3<f32>;
    switch i32(hue) {
        case 0: { color = vec3<f32>(c, x, 0.0); }
        case 1: { color = vec3<f32>(x, c, 0.0); }
        case 2: { color = vec3<f32>(0.0, c, x); }
        case 3: { color = vec3<f32>(0.0, x, c); }
        case 4: { color = vec3<f32>(x, 0.0, c); }
        default: { color = vec3<f32>(c, 0.0, x); }
    }
    return color + vec3<f32>(m);
}
fn balance(rgb: vec3<f32>) -> vec3<f32> {
    let luma = luminance(rgb);
    let shadow = 1.0 - tone_weight(0.08, 0.48, luma);
    let highlight = tone_weight(0.52, 0.92, luma);
    let middle = 1.0 - shadow - highlight;
    var adjusted = rgb + 0.5 * (params.shadows.xyz * shadow + params.midtones.xyz * middle + params.highlights.xyz * highlight);
    if params.factors.w != 0.0 {
        adjusted += vec3<f32>(luma - luminance(adjusted));
        var scale = 1.0;
        for (var channel = 0u; channel < 3u; channel++) {
            let chroma = adjusted[channel] - luma;
            if chroma > 0.0 { scale = min(scale, (1.0 - luma) / chroma); }
            else if chroma < 0.0 { scale = min(scale, -luma / chroma); }
        }
        adjusted = vec3<f32>(luma) + (adjusted - vec3<f32>(luma)) * scale;
    }
    return adjusted;
}
fn linear_index(id: vec3<u32>) -> u32 {
    return id.x + id.y * params.dims.w * 256u;
}
@compute @workgroup_size(256)
fn color(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = linear_index(id);
    if index >= params.dims.x * params.dims.y { return; }
    let p = rgba(source[index]);
    var result = p;
    if p.w != 0u {
        switch params.dims.z {
            case 0u, 1u: {
                result = vec4<u32>(table[p.x], table[p.y], table[p.z], p.w);
                if params.dims.z == 1u {
                    let rgb = vec3<f32>(result.xyz);
                    let gray = luminance(rgb);
                    result = vec4<u32>(bytes(vec3<f32>(gray) + (rgb - vec3<f32>(gray)) * params.factors.x), p.w);
                }
            }
            case 2u: {
                let gray = rounded_byte(luminance(vec3<f32>(p.xyz)));
                result = vec4<u32>(gray, gray, gray, p.w);
            }
            case 3u: { result = vec4<u32>(bytes(hsl(vec3<f32>(p.xyz) / 255.0) * 255.0), p.w); }
            case 4u: { result = vec4<u32>(bytes(balance(vec3<f32>(p.xyz) / 255.0) * 255.0), p.w); }
            default: {}
        }
    }
    destination[index] = packed(result);
}
@compute @workgroup_size(256)
fn init_blur(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = linear_index(id);
    if index >= params.dims.x * params.dims.y { return; }
    let p = rgba(source[index]);
    store_premult(index, vec4<u32>(p.xyz * p.w, p.w * 255u));
}
@compute @workgroup_size(256)
fn init_bloom(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = linear_index(id);
    if index >= params.dims.x * params.dims.y { return; }
    let p = rgba(source[index]);
    let emission = clamp((luminance(vec3<f32>(p.xyz) / 255.0) - params.factors.x) / max(1.0 - params.factors.x, 0.00000011920928955078125), 0.0, 1.0);
    let alpha = f32(p.w) * 255.0 * emission;
    let values = vec4<f32>(vec3<f32>(p.xyz) * alpha / 255.0, alpha);
    store_premult(index, vec4<u32>(floor(values + vec4<f32>(0.5))));
}

// A workgroup scans one 128-pixel strip plus both halos. Integer prefix sums
// give constant work per pixel at every radius, with clamped canvas edges.
var<workgroup> scan: array<vec4<u32>, 256>;
@compute @workgroup_size(256)
fn box_blur(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) thread: u32) {
    let vertical = params.dims.z != 0u;
    let radius = params.dims.w;
    let length = select(params.dims.x, params.dims.y, vertical);
    let position = u32(clamp(i32(group.x * 128u + thread) - i32(radius), 0, i32(length) - 1));
    let index = select(group.y * params.dims.x + position, position * params.dims.x + group.y, vertical);
    scan[thread] = premult(index);
    workgroupBarrier();
    var stride = 1u;
    while stride < 256u {
        var previous = vec4<u32>(0u);
        if thread >= stride { previous = scan[thread - stride]; }
        workgroupBarrier();
        scan[thread] += previous;
        workgroupBarrier();
        stride *= 2u;
    }
    let output_position = group.x * 128u + thread;
    if thread < 128u && output_position < length {
        var sum = scan[thread + 2u * radius];
        if thread > 0u { sum -= scan[thread - 1u]; }
        let count = 2u * radius + 1u;
        let out_index = select(group.y * params.dims.x + output_position, output_position * params.dims.x + group.y, vertical);
        store_premult(out_index, (sum + vec4<u32>(count / 2u)) / count);
    }
}
@compute @workgroup_size(256)
fn finish_blur(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = linear_index(id);
    if index >= params.dims.x * params.dims.y { return; }
    let values = premult(index);
    let alpha = min((values.w + 127u) / 255u, 255u);
    var result = vec4<u32>(0u, 0u, 0u, alpha);
    if alpha != 0u && values.w != 0u {
        result = vec4<u32>(min((values.xyz * 255u + vec3<u32>(values.w / 2u)) / values.w, vec3<u32>(255u)), alpha);
    }
    destination[index] = packed(result);
}
@compute @workgroup_size(256)
fn finish_bloom(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = linear_index(id);
    if index >= params.dims.x * params.dims.y { return; }
    let p = rgba(original[index]);
    let glow = vec4<f32>(premult(index));
    let base_alpha = f32(p.w) / 255.0;
    let glow_alpha = clamp(glow.w / 65025.0 * params.factors.y, 0.0, 1.0);
    let alpha = base_alpha + (1.0 - base_alpha) * glow_alpha;
    var result = p;
    if alpha > 0.0 {
        let out_alpha = rounded_byte(alpha * 255.0);
        var rgb = vec3<u32>(0u);
        if out_alpha != 0u {
            rgb = bytes(min(vec3<f32>(p.xyz) / 255.0 * base_alpha + glow.xyz / 65025.0 * params.factors.y, vec3<f32>(alpha)) / alpha * 255.0);
        }
        result = vec4<u32>(rgb, out_alpha);
    }
    destination[index] = packed(result);
}

fn rounded_word(value: f32) -> u32 {
    return u32(floor(clamp(value, 0.0, 65535.0) + 0.5));
}
fn words(value: vec3<f32>) -> vec3<u32> {
    return vec3<u32>(rounded_word(value.x), rounded_word(value.y), rounded_word(value.z));
}
// Native RGBA16 occupies two uints. Hidden colors and alpha remain exact; only
// color calculations use float, with CPU parity bounded to one native word.
@compute @workgroup_size(256)
fn color16(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = linear_index(id);
    if index >= params.dims.x * params.dims.y { return; }
    let p = premult(index); // unpack two words without premultiplying
    var result = p;
    if p.w != 0u {
        switch params.dims.z {
            case 1u: {
                let rgb = vec3<f32>(vec3<u32>(table[p.x], table[p.y], table[p.z]));
                let gray = luminance(rgb);
                result = vec4<u32>(words(vec3<f32>(gray) + (rgb - vec3<f32>(gray)) * params.factors.x), p.w);
            }
            case 3u: { result = vec4<u32>(words(hsl(vec3<f32>(p.xyz) / 65535.0) * 65535.0), p.w); }
            case 4u: { result = vec4<u32>(words(balance(vec3<f32>(p.xyz) / 65535.0) * 65535.0), p.w); }
            default: {}
        }
    }
    store_premult(index, result);
}
