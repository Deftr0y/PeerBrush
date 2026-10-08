//! Run explicitly on a native compute device: cargo test --test gpu_effects -- --ignored --nocapture.
use peerbrush::{
    effects::{self, Image},
    gpu,
};
use serde_json::{json, Value};

fn pattern(width: u32, height: u32) -> Image {
    let mut state = 0x4caaff13u32;
    let mut bytes = Vec::with_capacity(width as usize * height as usize * 4);
    for pixel in 0..width * height {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        let mut rgba = state.to_le_bytes();
        rgba[3] = [0, 1, 7, 32, 127, 254, 255][pixel as usize % 7];
        if pixel % 11 == 0 {
            rgba[..3].fill(255);
        }
        bytes.extend(rgba);
    }
    Image {
        width,
        height,
        bytes,
    }
}
fn parity(backend: &gpu::Backend, input: &Image, kind: &str, settings: &Value, tolerance: u8) {
    let mut reference = Image {
        width: input.width,
        height: input.height,
        bytes: input.bytes.clone(),
    };
    let mut computed = Image {
        width: input.width,
        height: input.height,
        bytes: input.bytes.clone(),
    };
    effects::apply_cpu(&mut reference, kind, settings).unwrap();
    assert!(
        backend.apply(&mut computed, kind, settings).unwrap(),
        "{kind} must actually dispatch compute"
    );
    for (index, (&actual, &expected)) in computed.bytes.iter().zip(&reference.bytes).enumerate() {
        assert!(
            actual.abs_diff(expected) <= tolerance,
            "{kind} {settings} {}x{} byte {index}: GPU {actual}, CPU {expected}",
            input.width,
            input.height
        );
    }
    if !matches!(kind, "blur" | "bloom") {
        for (source, result) in input
            .bytes
            .chunks_exact(4)
            .zip(computed.bytes.chunks_exact(4))
        {
            assert_eq!(source[3], result[3], "numeric effects preserve alpha");
            if source[3] == 0 {
                assert_eq!(source, result, "hidden transparent colors are preserved");
            }
        }
    }
}

#[test]
#[ignore = "requires a native compute adapter; CPU-only CI tests fallback separately"]
fn actual_device_matches_cpu_for_edges_faint_alpha_and_tones() {
    let backend = gpu::headless().expect("native compute adapter");
    println!("GPU parity adapter: {}", backend.adapter);
    for (width, height) in [(1, 1), (1, 137), (139, 1), (7, 3), (129, 131), (513, 73)] {
        let input = pattern(width, height);
        for radius in [0.5, 1.0, 8.0, 31.0, 64.0] {
            parity(&backend, &input, "blur", &json!({"radius":radius}), 0);
        }
        for spread in [0.0, 8.0, 64.0] {
            parity(
                &backend,
                &input,
                "bloom",
                &json!({"threshold":0.65,"strength":1.5,"spread":spread}),
                1,
            );
        }
    }
    let input = pattern(257, 67);
    for (kind, settings, tolerance) in [
        ("levels", json!({"black":0.12,"white":0.87,"gamma":1.6}), 0),
        (
            "curves",
            json!({"points":[[0,0.1],[0.3,0.2],[0.7,0.95],[1,0.9]]}),
            0,
        ),
        ("invert", json!({}), 0),
        ("grayscale", json!({}), 1),
        (
            "adjust",
            json!({"brightness":0.13,"contrast":1.4,"saturation":2.1}),
            1,
        ),
        (
            "hsl",
            json!({"hue":-174,"saturation":0.37,"lightness":-0.2}),
            1,
        ),
        (
            "hsl",
            json!({"hue":168,"saturation":-0.63,"lightness":0.27}),
            1,
        ),
        (
            "color_balance",
            json!({"shadows":[0.8,-0.3,0.1],"midtones":[-0.2,0.6,0.2],"highlights":[0.2,0.1,-0.7],"preserve_luminosity":true}),
            1,
        ),
        (
            "color_balance",
            json!({"shadows":[-0.5,0.3,0.1],"midtones":[0.2,-0.6,0.2],"highlights":[0.2,0.1,0.7],"preserve_luminosity":false}),
            1,
        ),
    ] {
        parity(&backend, &input, kind, &settings, tolerance);
    }
    let mut unchanged = pattern(4, 4);
    let source = unchanged.bytes.clone();
    assert!(!backend
        .apply(&mut unchanged, "liquify", &json!({}))
        .unwrap());
    assert_eq!(unchanged.bytes, source);
    assert!(backend
        .apply(&mut unchanged, "blur", &json!({"radius":999}))
        .is_err());
    assert_eq!(unchanged.bytes, source);
}

fn native_pattern(width: u32, height: u32) -> peerbrush::depth16::Image16 {
    let mut state = 0x721aad1fu32;
    let mut words = Vec::with_capacity(width as usize * height as usize * 4);
    for pixel in 0..width * height {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        let red = (state & 65535) as u16;
        let green = (state >> 16) as u16;
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        let blue = (state & 65535) as u16;
        let rgb = if pixel % 17 == 0 {
            [30001, 30002, 30003]
        } else {
            [red, green, blue]
        };
        let alpha = [0, 1, 33, 12345, 65535][pixel as usize % 5];
        words.extend([rgb[0], rgb[1], rgb[2], alpha]);
    }
    peerbrush::depth16::Image16 {
        width,
        height,
        words,
    }
}
#[test]
#[ignore = "requires native compute adapter"]
fn native16_pointwise_device_matches_cpu_without_projection_and_across_tiles() {
    use peerbrush::depth16::color;
    let backend = gpu::headless().unwrap();
    println!("Native16 adapter: {}", backend.adapter);
    for (kind, settings) in [
        ("hsl", json!({"hue":-174,"saturation":0.4,"lightness":-0.2})),
        (
            "hsl",
            json!({"hue":168,"saturation":-0.63,"lightness":0.27}),
        ),
        (
            "adjust",
            json!({"brightness":0.13,"contrast":1.4,"saturation":2.1}),
        ),
        (
            "color_balance",
            json!({"shadows":[0.8,-0.3,0.1],"midtones":[-0.2,0.6,0.2],"highlights":[0.2,0.1,-0.7],"preserve_luminosity":true}),
        ),
        (
            "color_balance",
            json!({"shadows":[-0.5,0.3,0.1],"midtones":[0.2,-0.6,0.2],"highlights":[0.2,0.1,0.7],"preserve_luminosity":false}),
        ),
    ] {
        let source = native_pattern(257, 67);
        let mut cpu = source.clone();
        let mut computed = source.clone();
        color::apply_cpu(&mut cpu, kind, &settings).unwrap();
        assert!(backend.apply16(&mut computed, kind, &settings).unwrap());
        for (index, (a, b)) in computed.words.iter().zip(&cpu.words).enumerate() {
            assert!(
                a.abs_diff(*b) <= 1,
                "{kind} native word{index}: GPU {a}, CPU {b}"
            );
        }
        for (a, b) in computed
            .words
            .chunks_exact(4)
            .zip(source.words.chunks_exact(4))
        {
            assert_eq!(a[3], b[3]);
            if b[3] == 0 {
                assert_eq!(a, b);
            }
        }
        assert!(
            computed.words.chunks_exact(4).any(|p| p[0] % 257 != 0),
            "native color detail survives"
        );
    }
    let source = native_pattern(1024, 1025);
    let settings = json!({"hue":47,"saturation":0.4,"lightness":0.1});
    let mut cpu = source.clone();
    let mut computed = source.clone();
    color::apply_cpu(&mut cpu, "hsl", &settings).unwrap();
    assert!(backend.apply16(&mut computed, "hsl", &settings).unwrap());
    assert!(
        computed
            .words
            .iter()
            .zip(&cpu.words)
            .all(|(a, b)| a.abs_diff(*b) <= 1),
        "tile boundary preserves full native precision"
    );
    let mut unchanged = native_pattern(3, 1);
    let words = unchanged.words.clone();
    assert!(!backend
        .apply16(&mut unchanged, "blur", &json!({"radius":8}))
        .unwrap());
    assert_eq!(unchanged.words, words);
    assert!(backend
        .apply16(&mut unchanged, "hsl", &json!({"hue":999}))
        .is_err());
    assert_eq!(unchanged.words, words);
}
