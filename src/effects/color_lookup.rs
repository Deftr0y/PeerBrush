//! Editable RGB Color Lookup using embedded ICC DeviceLink or 3D LUT data.
mod cube;
mod look;
mod three_dl;

use crate::color_profile::device_link::{self, DeviceLink};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::Read,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

pub(crate) const KIND: &str = "color_lookup";
const MAX_CUBE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_SOURCE_BYTES: usize = device_link::MAX_BYTES + MAX_CUBE_BYTES;
pub(crate) const MAX_NAME: usize = 4096;
pub(crate) const MAX_SETTINGS: usize =
    device_link::MAX_BYTES.div_ceil(3) * 4 + MAX_CUBE_BYTES.div_ceil(3) * 4 + 12 * MAX_NAME + 256;

pub(crate) struct Prepared {
    pub profile: Vec<u8>,
    pub cube: Vec<u8>,
    pub format: LutFormat,
    mapping: Mapping,
    swap: [bool; 2],
    compiled: OnceLock<Option<Vec<u8>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LutFormat {
    Cube,
    ThreeDl,
    Look,
}

impl LutFormat {
    fn key(self) -> &'static str {
        match self {
            Self::Cube => "cube",
            Self::ThreeDl => "3dl",
            Self::Look => "look",
        }
    }
}

enum Mapping {
    Link(DeviceLink),
    Cube(cube::Cube),
    ThreeDl(three_dl::ThreeDl),
    Look(crate::color_profile::clut::Clut),
}

type CacheEntry = ([String; 2], Arc<Prepared>);
// One exact-key cache entry bounds retained data and avoids decoding the same
// profile for every visible tile. It owns only derived data and source bytes.
static CACHE: Mutex<Option<CacheEntry>> = Mutex::new(None);

pub(crate) fn defaults() -> Value {
    json!({"name":"","profile":"","cube":"","file_name":"","data_order":"","table_order":"","lut_format":"","embed_profile":false})
}

pub(crate) fn text<'a>(settings: &'a Value, key: &str) -> Result<&'a str, String> {
    settings
        .get(key)
        .map(|name| {
            name.as_str()
                .filter(|name| {
                    name.len() <= MAX_NAME * 3 && name.encode_utf16().count() <= MAX_NAME
                })
                .ok_or_else(|| "Invalid Color Lookup name".into())
        })
        .unwrap_or(Ok(""))
}

pub(crate) fn prepare(settings: &Value) -> Result<Option<Arc<Prepared>>, String> {
    let fields = settings
        .as_object()
        .ok_or("Color Lookup settings must be an object")?;
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "name"
                | "profile"
                | "cube"
                | "file_name"
                | "data_order"
                | "table_order"
                | "lut_format"
                | "embed_profile"
        )
    }) {
        return Err("Unknown Color Lookup setting".into());
    }
    text(settings, "name")?;
    let file_name = text(settings, "file_name")?;
    let data_order = text(settings, "data_order")?;
    let table_order = text(settings, "table_order")?;
    let format_key = text(settings, "lut_format")?;
    let format = match format_key {
        "" | "cube" => LutFormat::Cube,
        "3dl" => LutFormat::ThreeDl,
        "look" => LutFormat::Look,
        _ => return Err("Unknown Color Lookup table format".into()),
    };
    let encoded = |key, max: usize| {
        fields
            .get(key)
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| value.len() <= max.div_ceil(3) * 4)
                    .ok_or("Color Lookup source must be bounded base64")
            })
            .transpose()
            .map(|value| value.unwrap_or(""))
    };
    let profile = encoded("profile", device_link::MAX_BYTES)?;
    let cube = encoded("cube", MAX_CUBE_BYTES)?;
    if fields.get("embed_profile").is_some_and(|flag| {
        !flag.is_boolean() || flag == true && (cube.is_empty() || !profile.is_empty())
    }) {
        return Err("A generated ICC profile requires a standalone Color Lookup table".into());
    }
    if cube.is_empty()
        && [file_name, data_order, table_order, format_key]
            .iter()
            .any(|value| !value.is_empty())
        || ![data_order, table_order]
            .into_iter()
            .all(|order| matches!(order, "" | "rgb") || !format_key.is_empty() && order == "bgr")
    {
        return Err("Invalid Color Lookup table metadata or channel order".into());
    }
    if profile.is_empty() && cube.is_empty() {
        return Ok(None);
    }
    // Earlier PeerBrush sources stored only RGB/unspecified orders and always
    // evaluated the CUBE file in its native order. Retain their appearance.
    let swap = if profile.is_empty() && !format_key.is_empty() {
        [
            match format {
                LutFormat::Cube | LutFormat::Look => table_order == "rgb",
                LutFormat::ThreeDl => table_order == "bgr",
            },
            data_order == "bgr",
        ]
    } else {
        [false; 2]
    };
    if let Some((key, prepared)) = &*CACHE.lock().unwrap() {
        if key[0] == profile && key[1] == cube && prepared.format == format && prepared.swap == swap
        {
            return Ok(Some(prepared.clone()));
        }
    }
    let sources = [profile, cube].map(str::to_owned);
    let profile = decode_source(profile, device_link::MAX_BYTES)?;
    let cube = decode_source(cube, MAX_CUBE_BYTES)?;
    let table = if cube.is_empty() {
        None
    } else {
        Some(match format {
            LutFormat::Cube => Mapping::Cube(cube::Cube::parse(&cube)?),
            LutFormat::ThreeDl => Mapping::ThreeDl(three_dl::ThreeDl::parse(&cube)?),
            LutFormat::Look => Mapping::Look(look::parse(&cube)?),
        })
    };
    // Photoshop can store its generated ICC transform alongside the original
    // LUT. Keep both intact and evaluate the embedded link when available.
    let mapping = if profile.is_empty() {
        table.ok_or("Missing Color Lookup table")?
    } else {
        Mapping::Link(DeviceLink::parse(&profile)?)
    };
    let prepared = Arc::new(Prepared {
        profile,
        cube,
        format,
        mapping,
        swap,
        compiled: OnceLock::new(),
    });
    *CACHE.lock().unwrap() = Some((sources, prepared.clone()));
    Ok(Some(prepared))
}

fn decode_source(encoded: &str, limit: usize) -> Result<Vec<u8>, String> {
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| "Invalid Color Lookup source base64")?;
    if bytes.len() > limit {
        return Err("Color Lookup source exceeds its size limit".into());
    }
    Ok(bytes)
}

pub(crate) fn from_profile(profile: &[u8], label: &str) -> Result<Value, String> {
    if profile.is_empty() || profile.len() > device_link::MAX_BYTES {
        return Err("Color Lookup requires a nonempty ICC DeviceLink within 512 KiB".into());
    }
    let mut settings = defaults();
    settings["name"] = json!(label);
    settings["profile"] = json!(STANDARD.encode(profile));
    prepare(&settings)?;
    Ok(settings)
}

pub(crate) fn from_table(
    format: LutFormat,
    cube: &[u8],
    profile: &[u8],
    label: &str,
    file_name: &str,
) -> Result<Value, String> {
    if cube.is_empty() || cube.len() > MAX_CUBE_BYTES || profile.len() > device_link::MAX_BYTES {
        return Err(
            "Color Lookup requires a nonempty table within 2 MiB and ICC profile within 512 KiB"
                .into(),
        );
    }
    let mut settings = defaults();
    settings["name"] = json!(label);
    settings["file_name"] = json!(file_name);
    settings["profile"] = json!(STANDARD.encode(profile));
    settings["cube"] = json!(STANDARD.encode(cube));
    settings["lut_format"] = json!(format.key());
    settings["embed_profile"] = json!(profile.is_empty());
    prepare(&settings)?;
    Ok(settings)
}

#[cfg(test)]
pub(crate) fn from_cube(
    cube: &[u8],
    profile: &[u8],
    label: &str,
    file_name: &str,
) -> Result<Value, String> {
    from_table(LutFormat::Cube, cube, profile, label, file_name)
}

pub(crate) fn uses_table(settings: &Value) -> bool {
    settings["cube"]
        .as_str()
        .is_some_and(|cube| !cube.is_empty())
}

pub(crate) fn source_size(settings: &Value, field: &str) -> usize {
    settings[field].as_str().map_or(0, str::len)
}

pub(crate) fn read_file(path: &Path) -> Result<Value, String> {
    let format = path
        .extension()
        .and_then(|extension| extension.to_str())
        .and_then(|extension| match extension.to_ascii_lowercase().as_str() {
            "cube" => Some(LutFormat::Cube),
            "3dl" => Some(LutFormat::ThreeDl),
            "look" => Some(LutFormat::Look),
            _ => None,
        });
    let limit = if format.is_some() {
        MAX_CUBE_BYTES
    } else {
        device_link::MAX_BYTES
    };
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut bytes))
        .map_err(|_| "Could not read the Color Lookup source")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Color Lookup");
    if let Some(format) = format {
        from_table(format, &bytes, &[], name, name)
    } else {
        from_profile(&bytes, name)
    }
}

impl Prepared {
    pub(crate) fn compiled_profile(&self) -> Option<&[u8]> {
        self.compiled
            .get_or_init(|| match &self.mapping {
                Mapping::Cube(cube) => cube.compile_profile(self.swap),
                Mapping::ThreeDl(table) => table.compile_profile(self.swap),
                Mapping::Look(grid) => crate::color_profile::device_link_mpe::compile(
                    grid,
                    crate::color_profile::device_link_mpe::Input::Domain([0.; 3], [1.; 3]),
                    self.swap,
                ),
                Mapping::Link(_) => None,
            })
            .as_deref()
    }

    pub(crate) fn sample(&self, mut color: [u16; 3], max: u16) -> [u16; 3] {
        if self.swap[0] {
            color.swap(0, 2);
        }
        let mut output = match &self.mapping {
            Mapping::Link(link) => link.sample(color, max),
            Mapping::Cube(cube) => cube.sample(color, max),
            Mapping::ThreeDl(table) => table.sample(color, max),
            Mapping::Look(grid) => {
                let max = f64::from(max);
                grid.sample(color.map(|value| f64::from(value) / max))
                    .map(|value| (value.clamp(0., 1.) * max).round() as u16)
            }
        };
        if self.swap[1] {
            output.swap(0, 2);
        }
        output
    }

    pub(crate) fn apply8(&self, rgba: &mut [u8]) {
        for pixel in rgba.chunks_exact_mut(4) {
            let rgb = self.sample([pixel[0], pixel[1], pixel[2]].map(u16::from), 255);
            pixel[..3].copy_from_slice(&rgb.map(|channel| channel as u8));
        }
    }

    pub(crate) fn apply16(&self, rgba: &mut [u16]) {
        for pixel in rgba.chunks_exact_mut(4) {
            let rgb = self.sample([pixel[0], pixel[1], pixel[2]], u16::MAX);
            pixel[..3].copy_from_slice(&rgb);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const CUBE: &[u8] = b"TITLE \"Grade #1\"\r\nLUT_3D_SIZE 2\r\n0 0 0\r\n0 1 0\r\n0 0 1\r\n0 1 1\r\n1 0 0\r\n1 1 0\r\n1 0 1\r\n1 1 1\r\n";
    pub(crate) const THREE_DL: &[u8] = b"# Blue-fastest\n0 85 170 255\n0 0 0\n1023 0 0\n0 0 1023\n1023 0 1023\n0 1023 0\n1023 1023 0\n0 1023 1023\n1023 1023 1023\n";

    pub(crate) fn look_fixture() -> String {
        use std::fmt::Write;
        let mut source = String::from(
            "<?xml version=\"1.0\"?>\n<look><shaders><base><visible>\"1\"</visible></base></shaders><mask/>\n<LUT><size>\"2\"</size><data>\"\n",
        );
        for b in [0f32, 1.] {
            for g in [0f32, 1.] {
                for r in [0f32, 1.] {
                    for byte in [b, r, g].into_iter().flat_map(f32::to_le_bytes) {
                        write!(source, "{byte:02x}").unwrap();
                    }
                    source.push('\n');
                }
            }
        }
        source.push_str("\"</data></LUT></look>");
        source
    }

    #[test]
    fn look_uses_baked_float_grid_and_rejects_masks_and_unknown_payloads() {
        let look = look_fixture();
        let settings = from_table(
            LutFormat::Look,
            look.as_bytes(),
            &[],
            "Look proof",
            "proof.look",
        )
        .unwrap();
        let table = prepare(&settings).unwrap().unwrap();
        assert_eq!(
            table.sample([12345, 23456, 34567], 65535),
            [34567, 12345, 23456]
        );
        assert_eq!(table.sample([23, 129, 201], 255), [201, 23, 129]);
        assert_eq!(table.cube, look.as_bytes());
        let profile = DeviceLink::parse(table.compiled_profile().unwrap()).unwrap();
        assert_eq!(
            profile.sample([12345, 23456, 34567], 65535),
            [34567, 12345, 23456]
        );
        let metadata = look.replace("<mask/>", "<mask>\"0\"</mask><!-- <LUT><size>0</size></LUT> --><metadata><size>4</size><data>DEADBEEF</data></metadata>");
        let metadata = metadata.replace("version=\"1.0\"", "version=\"1.0\" encoding=\"UTF-16\"");
        for (bom, encode) in [
            ([0xff, 0xfe], u16::to_le_bytes as fn(u16) -> [u8; 2]),
            ([0xfe, 0xff], u16::to_be_bytes),
        ] {
            let bytes = bom
                .into_iter()
                .chain(metadata.encode_utf16().flat_map(encode))
                .collect::<Vec<_>>();
            let settings =
                from_table(LutFormat::Look, &bytes, &[], "UTF-16 proof", "proof.look").unwrap();
            let table = prepare(&settings).unwrap().unwrap();
            assert_eq!(table.sample([23, 129, 201], 255), [201, 23, 129]);
            assert_eq!(table.cube, bytes);
        }
        for invalid in [
            look.replace("<mask/>", "<mask><pixels>1</pixels></mask>"),
            look.replace("<base>", "<base><mask>123</mask>"),
            look.replace("<mask/>", "<mask>payload</mask>"),
            look.replace("<LUT>", "<LUT><size>2</size>"),
            look.replace("<size>\"2\"</size>", "<size>2<!-- extra -->3</size>"),
            look.replace("<LUT>", "<LUT xmlns=\"urn:unsupported\">"),
            look.replace("<look>", "<look><unrecognized/>"),
            look.replace("<data>\"\n00000000", "<data>\"\n0000807f"),
            look.replace("<data>\"\n00000000", "<data>\"\n00000Z00"),
            look.replace("\"</data>", "0\"</data>"),
            look.replace("<look>", "<!DOCTYPE look SYSTEM \"local.xml\"><look>"),
        ] {
            assert!(look::parse(invalid.as_bytes()).is_err());
        }
    }

    #[test]
    fn generated_device_links_follow_lookup_orders_domains_and_shapers() {
        let domain = [
            b"DOMAIN_MIN 0 -.25 0\nDOMAIN_MAX 1 .75 1\n".as_slice(),
            CUBE,
        ]
        .concat();
        let header = b"# Blue-fastest\n0 85 170 255\n";
        let shaper = [b"0 0 1023 1023\n".as_slice(), &THREE_DL[header.len()..]].concat();
        for (format, bytes, order) in [
            (LutFormat::Cube, domain.as_slice(), "rgb"),
            (LutFormat::ThreeDl, shaper.as_slice(), "bgr"),
        ] {
            let mut settings = from_table(format, bytes, &[], "Compiled proof", "").unwrap();
            settings["table_order"] = json!(order);
            settings["data_order"] = json!("bgr");
            let source = prepare(&settings).unwrap().unwrap();
            let profile = source.compiled_profile().unwrap();
            assert!(crate::color_profile::device_link_mpe::is_peerbrush_profile(
                profile
            ));
            let link = DeviceLink::parse(profile).unwrap();
            for (rgb, max) in [
                ([17, 143, 240], 255),
                ([12345, 23456, 34567], 65535),
                ([65535, 1, 0], 65535),
                ([0, 65535, 65535], 65535),
            ] {
                assert_eq!(link.sample(rgb, max), source.sample(rgb, max));
            }
            let mut invalid = profile.to_vec();
            let clut = profile
                .windows(4)
                .position(|bytes| bytes == b"clut")
                .unwrap();
            invalid[clut + 28..clut + 32].copy_from_slice(&f32::NAN.to_be_bytes());
            assert!(DeviceLink::parse(&invalid).is_err());
        }
        let narrow = [
            b"DOMAIN_MIN 0 1e-40 0\nDOMAIN_MAX 1 2e-40 1\n".as_slice(),
            CUBE,
        ]
        .concat();
        let settings = from_cube(&narrow, &[], "Narrow domain", "").unwrap();
        assert!(
            prepare(&settings)
                .unwrap()
                .unwrap()
                .compiled_profile()
                .is_none()
        );
    }

    #[test]
    fn three_dl_keeps_native_words_and_validates_mesh_and_shaper() {
        let table = three_dl::ThreeDl::parse(THREE_DL).unwrap();
        assert_eq!(
            table.sample([12345, 23456, 34567], u16::MAX),
            [34567, 12345, 23456]
        );
        let mut mesh = b"3DMESH\nMesh 1 16\n0 0 65535 65535\n".to_vec();
        for r in [0, 32768, 65535] {
            for g in [0, 32768, 65535] {
                for b in [0, 32768, 65535] {
                    mesh.extend(format!("{b} {r} {g}\n").as_bytes());
                }
            }
        }
        mesh.extend(b"LUT8\ngamma 1.0\n");
        let shaped = three_dl::ThreeDl::parse(&mesh).unwrap();
        assert_eq!(
            shaped.sample([10000, 32768, 60000], u16::MAX),
            [65535, 0, 32769]
        );
        assert!(three_dl::ThreeDl::parse(&mesh[..40]).is_err());
        assert!(three_dl::ThreeDl::parse(&[THREE_DL, b"0 0 0\n"].concat()).is_err());
        assert!(three_dl::ThreeDl::parse(&[THREE_DL, b"0 0 -1\n"].concat()).is_err());
        let gamma = mesh.len() - 4;
        mesh[gamma] = b'2';
        assert!(three_dl::ThreeDl::parse(&mesh).is_err());
    }

    #[test]
    fn cube_orders_follow_measured_photoshop_channel_swaps_and_keep_legacy_appearance() {
        let cube = b"LUT_3D_SIZE 2\n0 .25 .125\n1 .25 .125\n0 .75 .125\n1 .75 .125\n0 .25 .375\n1 .25 .375\n0 .75 .375\n1 .75 .375\n";
        let mut settings = from_cube(cube, &[], "Order proof", "").unwrap();
        for (data, table, rgb8, rgb16) in [
            ("rgb", "bgr", [64, 128, 80], [12345, 28112, 16834]),
            ("bgr", "bgr", [80, 128, 64], [16834, 28112, 12345]),
            ("rgb", "rgb", [192, 128, 48], [34567, 28112, 11278]),
            ("bgr", "rgb", [48, 128, 192], [11278, 28112, 34567]),
        ] {
            settings["data_order"] = json!(data);
            settings["table_order"] = json!(table);
            let prepared = prepare(&settings).unwrap().unwrap();
            assert_eq!(prepared.sample([64, 128, 192], 255), rgb8);
            assert_eq!(prepared.sample([12345, 23456, 34567], 65535), rgb16);
        }
        settings["data_order"] = json!("rgb");
        settings.as_object_mut().unwrap().remove("lut_format");
        let legacy = prepare(&settings).unwrap().unwrap();
        assert_eq!(legacy.sample([64, 128, 192], 255), [64, 128, 80]);
    }

    #[test]
    fn cube_lookup_respects_rgb_order_domain_and_native_words() {
        let table = cube::Cube::parse(CUBE).unwrap();
        assert_eq!(
            table.sample([12345, 23456, 34567], u16::MAX),
            [34567, 12345, 23456]
        );
        assert_eq!(table.sample([23, 129, 201], 255), [201, 23, 129]);
        let domain = [b"DOMAIN_MIN 0 0.2 0\nDOMAIN_MAX 1 0.8 1\n".as_slice(), CUBE].concat();
        let table = cube::Cube::parse(&domain).unwrap();
        assert_eq!(table.sample([20, 102, 123], 255), [123, 20, 85]);
        assert_eq!(table.sample([20, 0, 123], 255), [123, 20, 0]);
        assert_eq!(table.sample([20, 255, 123], 255), [123, 20, 255]);
        for bad in [
            [&CUBE[..CUBE.len() - 7], b""].concat(),
            [CUBE, b"0 0 0\n"].concat(),
            [b"LUT_3D_SIZE 34\n".as_slice(), CUBE].concat(),
            [b"LUT_1D_SIZE 2\n".as_slice(), CUBE].concat(),
            [b"DOMAIN_MIN 1 0 0\n".as_slice(), CUBE].concat(),
        ] {
            assert!(cube::Cube::parse(&bad).is_err());
        }
    }
}
