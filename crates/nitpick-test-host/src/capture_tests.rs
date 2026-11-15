use std::collections::BTreeMap;

use half::f16;
use serde_json::json;

use super::*;

fn field(channel: Channel, shift: u32, bits: u32, kind: SampleKind) -> Field {
    Field {
        channel,
        shift,
        bits,
        kind,
    }
}

fn uint(channel: Channel, shift: u32, bits: u32) -> Field {
    field(channel, shift, bits, SampleKind::Uint)
}

fn float(channel: Channel, shift: u32, bits: u32) -> Field {
    field(channel, shift, bits, SampleKind::Float)
}

fn plane(
    row_pitch: usize,
    block_bytes: usize,
    byte_order: ByteOrder,
    fields: Vec<Field>,
) -> PlaneLayout {
    PlaneLayout {
        row_pitch,
        subsampling: [1, 1],
        block_bytes,
        byte_order,
        fields,
    }
}

fn unknown_colour() -> Colour {
    Colour {
        transfer: None,
        primaries: None,
        white: None,
        range: None,
        matrix: None,
        luminance: None,
    }
}

fn capture(width: usize, height: usize, planes: Vec<(PlaneLayout, Vec<u8>)>) -> Capture {
    let (layouts, data) = planes.into_iter().unzip();
    Capture {
        descriptor: CaptureDescriptor {
            width,
            height,
            planes: layouts,
            alpha: None,
            colour: unknown_colour(),
            timestamps: Timestamps {
                frame: None,
                arrival: None,
            },
            source: Source {
                kind: SourceKind::Window,
                native: "test".into(),
            },
        },
        planes: data,
        raw: Map::new(),
        raw_blobs: BTreeMap::new(),
        config: Map::new(),
        preview: None,
    }
}

/// Interleaved little-endian RGBA binary16.
fn rgba16f_fields() -> Vec<Field> {
    vec![
        float(Channel::R, 0, 16),
        float(Channel::G, 16, 16),
        float(Channel::B, 32, 16),
        float(Channel::A, 48, 16),
    ]
}

/// 3×2 px RGBA binary16 with 8 padding bytes `EE` after each row.
fn padded_rgba16f() -> (Capture, Vec<[f32; 4]>) {
    let (w, h, pitch) = (3, 2, 32);
    let mut data = vec![0xEE; pitch * h];
    let mut pixels = Vec::new();
    for y in 0..h {
        for x in 0..w {
            #[allow(clippy::cast_precision_loss)] // Small integers.
            let v = (x + 10 * y) as f32;
            let px = [v, -v - 0.5, 2.5 + v, 1.0];
            for (c, value) in px.iter().enumerate() {
                let at = y * pitch + x * 8 + c * 2;
                data[at..at + 2].copy_from_slice(&f16::from_f32(*value).to_le_bytes());
            }
            pixels.push(px);
        }
    }
    let layout = plane(pitch, 8, ByteOrder::Little, rgba16f_fields());
    (capture(w, h, vec![(layout, data)]), pixels)
}

const RGBA: [Channel; 4] = [Channel::R, Channel::G, Channel::B, Channel::A];

#[test]
fn row_pitch_padding_is_not_read() {
    let (c, pixels) = padded_rgba16f();
    let view = c.validate().unwrap();
    for (i, px) in pixels.iter().enumerate() {
        let (x, y) = (i % 3, i / 3);
        for (channel, want) in RGBA.into_iter().zip(px) {
            assert_eq!(
                view.sample(channel, x, y),
                Some(Sample::Float(*want)),
                "{x},{y}"
            );
        }
    }
}

#[test]
fn data_length_allows_only_the_last_row_without_padding() {
    let (c, _) = padded_rgba16f();
    // Rows of 24 B at a pitch of 32 B.
    for (len, valid) in [(56, true), (60, true), (64, true), (55, false), (65, false)] {
        let mut c = c.clone();
        c.planes[0].resize(len, 0xEE);
        assert_eq!(c.validate().is_ok(), valid, "{len} B");
        if !valid {
            assert_eq!(
                c.validate().unwrap_err(),
                DescriptorError::DataLength {
                    plane: 0,
                    len,
                    min: 56,
                    max: 64
                }
            );
        }
    }
}

#[test]
fn channel_order_follows_the_fields() {
    let rgba = |r, g, b, a| {
        vec![
            uint(Channel::R, r, 8),
            uint(Channel::G, g, 8),
            uint(Channel::B, b, 8),
            uint(Channel::A, a, 8),
        ]
    };
    // The same pixel, stored as BGRA and as RGBA bytes.
    let bgra = capture(
        1,
        1,
        vec![(
            plane(4, 4, ByteOrder::Little, rgba(16, 8, 0, 24)),
            vec![3, 2, 1, 4],
        )],
    );
    let rgba = capture(
        1,
        1,
        vec![(
            plane(4, 4, ByteOrder::Little, rgba(0, 8, 16, 24)),
            vec![1, 2, 3, 4],
        )],
    );
    for c in [bgra, rgba] {
        let view = c.validate().unwrap();
        let got: Vec<_> = RGBA.iter().map(|&ch| view.sample(ch, 0, 0)).collect();
        assert_eq!(got, [1, 2, 3, 4].map(|v| Some(Sample::Uint(v))));
    }
}

#[test]
fn packed_fields_follow_the_byte_order() {
    // DXGI R10G10B10A2: red in the lowest bits of a little-endian word.
    let word: u32 = 1 | (1023 << 10) | (512 << 20) | (2 << 30);
    let fields = vec![
        uint(Channel::R, 0, 10),
        uint(Channel::G, 10, 10),
        uint(Channel::B, 20, 10),
        uint(Channel::A, 30, 2),
    ];
    let c = capture(
        1,
        1,
        vec![(
            plane(4, 4, ByteOrder::Little, fields),
            word.to_le_bytes().to_vec(),
        )],
    );
    let view = c.validate().unwrap();
    let got: Vec<_> = RGBA.iter().map(|&ch| view.sample(ch, 0, 0)).collect();
    assert_eq!(got, [1, 1023, 512, 2].map(|v| Some(Sample::Uint(v))));

    // Big-endian 16-bit components: the first component is highest.
    let fields = vec![
        uint(Channel::R, 32, 16),
        uint(Channel::G, 16, 16),
        uint(Channel::B, 0, 16),
    ];
    let c = capture(
        1,
        1,
        vec![(
            plane(6, 6, ByteOrder::Big, fields),
            vec![0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC],
        )],
    );
    let view = c.validate().unwrap();
    let got: Vec<_> = RGBA[..3].iter().map(|&ch| view.sample(ch, 0, 0)).collect();
    assert_eq!(got, [0x1234, 0x5678, 0x9ABC].map(|v| Some(Sample::Uint(v))));
}

/// 3×3 px, 10-bit 4:2:0 in two planes with the samples in the high bits
/// of 16-bit words, as P010.
fn ycbcr_420() -> Capture {
    let (w, h) = (3, 3);
    let y_pitch = 8;
    let mut y_plane = vec![0xFF; y_pitch * h];
    for y in 0..h {
        for x in 0..w {
            let code = u16::try_from(64 + 100 * y + x).unwrap();
            let at = y * y_pitch + 2 * x;
            y_plane[at..at + 2].copy_from_slice(&(code << 6).to_le_bytes());
        }
    }
    // 2×2 blocks of 4 B: Cb, then Cr.
    let c_pitch = 8;
    let mut c_plane = vec![0; c_pitch * 2];
    for by in 0..2 {
        for bx in 0..2 {
            let cb = u32::try_from(500 + 10 * by + bx).unwrap();
            let cr = cb + 100;
            let at = by * c_pitch + 4 * bx;
            c_plane[at..at + 4].copy_from_slice(&((cb << 6) | (cr << 22)).to_le_bytes());
        }
    }
    let luma = plane(y_pitch, 2, ByteOrder::Little, vec![uint(Channel::Y, 6, 10)]);
    let chroma = PlaneLayout {
        subsampling: [2, 2],
        ..plane(
            c_pitch,
            4,
            ByteOrder::Little,
            vec![uint(Channel::Cb, 6, 10), uint(Channel::Cr, 22, 10)],
        )
    };
    let mut c = capture(w, h, vec![(luma, y_plane), (chroma, c_plane)]);
    c.descriptor.colour.matrix = Some(YcbcrMatrix::Bt2020Ncl);
    c.descriptor.colour.range = Some(Range::Narrow);
    c
}

#[test]
fn subsampled_planes_give_the_block_of_the_pixel() {
    let c = ycbcr_420();
    let view = c.validate().unwrap();
    for y in 0..3 {
        for x in 0..3 {
            let luma = u32::try_from(64 + 100 * y + x).unwrap();
            let cb = u32::try_from(500 + 10 * (y / 2) + x / 2).unwrap();
            assert_eq!(view.sample(Channel::Y, x, y), Some(Sample::Uint(luma)));
            assert_eq!(view.sample(Channel::Cb, x, y), Some(Sample::Uint(cb)));
            assert_eq!(view.sample(Channel::Cr, x, y), Some(Sample::Uint(cb + 100)));
        }
    }
    assert_eq!(view.sample(Channel::R, 0, 0), None);
    assert_eq!(view.sample(Channel::Y, 3, 0), None);
    assert_eq!(view.sample(Channel::Y, 0, 3), None);
}

#[test]
fn float_samples_keep_sign_and_magnitude() {
    let values = [-0.25f32, 12.5, f32::INFINITY];
    let data: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let fields = vec![
        float(Channel::R, 0, 32),
        float(Channel::G, 32, 32),
        float(Channel::B, 64, 32),
    ];
    let c = capture(1, 1, vec![(plane(12, 12, ByteOrder::Little, fields), data)]);
    let view = c.validate().unwrap();
    let got: Vec<_> = RGBA[..3].iter().map(|&ch| view.sample(ch, 0, 0)).collect();
    assert_eq!(got, values.map(|v| Some(Sample::Float(v))));
}

#[test]
fn unknown_fields_stay_unknown() {
    let (c, _) = padded_rgba16f();
    // binary16 storage alone does not give a transfer or a scale.
    let before = c.descriptor.clone();
    c.validate().unwrap();
    assert_eq!(c.descriptor, before);
    let value = serde_json::to_value(&c.descriptor.colour).unwrap();
    assert_eq!(
        value,
        json!({"transfer": null, "primaries": null, "white": null, "range": null,
               "matrix": null, "luminance": null})
    );
    assert_eq!(
        serde_json::from_value::<Colour>(value).unwrap(),
        c.descriptor.colour
    );
}

#[test]
fn descriptor_json_form() {
    let mut c = ycbcr_420();
    c.descriptor.alpha = None;
    c.descriptor.colour = Colour {
        transfer: Some(Transfer::Pq),
        primaries: Some(Primaries::Custom {
            red: [0.708, 0.292],
            green: [0.17, 0.797],
            blue: [0.131, 0.046],
        }),
        white: Some(WhitePoint::Custom([0.3127, 0.329])),
        range: Some(Range::Narrow),
        matrix: Some(YcbcrMatrix::Bt2020Ncl),
        luminance: Some(LuminanceScale::Absolute {
            nits_per_unit: 10_000.0,
        }),
    };
    c.descriptor.timestamps.frame = Some(Timestamp {
        base: TimeBase::Qpc,
        ticks: u64::MAX,
        ticks_per_second: 10_000_000,
    });
    c.validate().unwrap();
    let value = serde_json::to_value(&c.descriptor).unwrap();
    assert_eq!(
        value["colour"],
        json!({
            "transfer": "pq",
            "primaries": {"custom": {"red": [0.708, 0.292], "green": [0.17, 0.797],
                                     "blue": [0.131, 0.046]}},
            "white": {"custom": [0.3127, 0.329]},
            "range": "narrow",
            "matrix": "bt2020_ncl",
            "luminance": {"kind": "absolute", "nits_per_unit": 10000.0},
        })
    );
    assert_eq!(
        value["planes"][1],
        json!({"row_pitch": 8, "subsampling": [2, 2], "block_bytes": 4, "byte_order": "little",
               "fields": [{"channel": "cb", "shift": 6, "bits": 10, "kind": "uint"},
                          {"channel": "cr", "shift": 22, "bits": 10, "kind": "uint"}]})
    );
    assert_eq!(
        value["timestamps"],
        json!({"frame": {"base": "qpc", "ticks": u64::MAX, "ticks_per_second": 10_000_000},
               "arrival": null})
    );
    let back: CaptureDescriptor = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(back, c.descriptor);
    let mut extra = value;
    extra["colour"]["gamma"] = json!(2.4);
    assert!(serde_json::from_value::<CaptureDescriptor>(extra).is_err());
}

type Mutation = fn(&mut Capture);

#[test]
#[allow(clippy::too_many_lines)] // One table.
fn invalid_descriptors_are_rejected() {
    use DescriptorError as E;
    let field_error = |plane, channel, problem| E::Field {
        plane,
        channel,
        problem,
    };
    let rgba: Vec<(Mutation, E)> = vec![
        (|c| c.descriptor.width = 0, E::EmptyImage),
        (
            |c| {
                c.planes.push(vec![]);
            },
            E::PlaneCount {
                descriptor: 1,
                data: 2,
            },
        ),
        (
            |c| c.descriptor.planes[0].block_bytes = 17,
            E::BlockBytes { plane: 0 },
        ),
        (
            |c| c.descriptor.planes[0].subsampling = [2, 1],
            E::Subsampling { plane: 0 },
        ),
        (
            |c| c.descriptor.planes[0].fields[1].bits = 0,
            field_error(0, Channel::G, FieldProblem::ZeroWidth),
        ),
        (
            |c| c.descriptor.planes[0].fields[3].bits = 24,
            field_error(0, Channel::A, FieldProblem::Width),
        ),
        (
            |c| c.descriptor.planes[0].fields[3].shift = 56,
            field_error(0, Channel::A, FieldProblem::OutsideBlock),
        ),
        (
            |c| c.descriptor.planes[0].fields[1].shift = 8,
            field_error(0, Channel::G, FieldProblem::Overlap),
        ),
        (
            |c| c.descriptor.planes[0].row_pitch = 16,
            E::RowPitch {
                plane: 0,
                pitch: 16,
                row: 24,
            },
        ),
        (
            |c| c.descriptor.planes[0].row_pitch = usize::MAX,
            E::Overflow { plane: 0 },
        ),
        (
            |c| c.descriptor.planes[0].fields[3].channel = Channel::R,
            E::DuplicateChannel(Channel::R),
        ),
        (
            |c| c.descriptor.planes[0].fields[2].channel = Channel::Y,
            E::Channels,
        ),
        (|c| c.descriptor.planes[0].fields.truncate(2), E::Channels),
        (
            |c| c.descriptor.planes[0].fields[1].kind = SampleKind::Uint,
            E::MixedSamples,
        ),
        (
            |c| {
                c.descriptor.planes[0].fields.pop();
                c.descriptor.alpha = Some(Alpha::Straight);
            },
            E::AlphaWithoutField,
        ),
        (
            |c| c.descriptor.colour.range = Some(Range::Full),
            E::RangeOfFloat,
        ),
        (
            |c| c.descriptor.colour.matrix = Some(YcbcrMatrix::Bt709),
            E::MatrixOfRgb,
        ),
        (
            |c| {
                c.descriptor.colour.primaries = Some(Primaries::Custom {
                    red: [0.64, 0.33],
                    green: [0.3, f64::NAN],
                    blue: [0.15, 0.06],
                });
            },
            E::Chromaticity,
        ),
        (
            |c| c.descriptor.colour.white = Some(WhitePoint::Custom([0.6, 0.5])),
            E::Chromaticity,
        ),
        (
            |c| {
                c.descriptor.colour.luminance =
                    Some(LuminanceScale::Absolute { nits_per_unit: 0.0 });
            },
            E::Luminance,
        ),
        (
            |c| {
                c.descriptor.colour.transfer = Some(Transfer::Pq);
                c.descriptor.colour.luminance = Some(LuminanceScale::Absolute {
                    nits_per_unit: 80.0,
                });
            },
            E::Luminance,
        ),
        (
            |c| {
                c.descriptor.colour.transfer = Some(Transfer::Pq);
                c.descriptor.colour.luminance = Some(LuminanceScale::Relative);
            },
            E::Luminance,
        ),
        (
            |c| {
                c.descriptor.timestamps.arrival = Some(Timestamp {
                    base: TimeBase::Monotonic,
                    ticks: 1,
                    ticks_per_second: 0,
                });
            },
            E::Timestamp,
        ),
        (
            |c| {
                c.raw_blobs.insert("../x".into(), vec![]);
            },
            E::BlobName("../x".into()),
        ),
    ];
    for (i, (mutate, want)) in rgba.into_iter().enumerate() {
        let (mut c, _) = padded_rgba16f();
        mutate(&mut c);
        assert_eq!(c.validate().unwrap_err(), want, "case {i}");
    }

    // A luma plane cannot be subsampled.
    let mut c = ycbcr_420();
    c.descriptor.planes[0].subsampling = [1, 2];
    assert_eq!(c.validate().unwrap_err(), E::Subsampling { plane: 0 });
    // Chroma subsampling 3 is not supported.
    let mut c = ycbcr_420();
    c.descriptor.planes[1].subsampling = [3, 1];
    assert_eq!(c.validate().unwrap_err(), E::Subsampling { plane: 1 });
}

#[test]
fn valid_descriptors_with_known_colour() {
    let (mut c, _) = padded_rgba16f();
    c.descriptor.alpha = Some(Alpha::Ignored);
    c.descriptor.colour = Colour {
        transfer: Some(Transfer::Linear),
        primaries: Some(Primaries::Bt709),
        white: Some(WhitePoint::D65),
        range: None,
        matrix: None,
        luminance: Some(LuminanceScale::Absolute {
            nits_per_unit: 80.0,
        }),
    };
    c.validate().unwrap();
    // HLG has no fixed scale.
    c.descriptor.colour.transfer = Some(Transfer::Hlg);
    c.descriptor.colour.luminance = Some(LuminanceScale::Relative);
    c.validate().unwrap();
}

#[test]
fn blob_names() {
    for (name, valid) in [
        ("attachments", true),
        ("spa-pod_2", true),
        ("", false),
        ("Upper", false),
        ("a/b", false),
        ("..", false),
        (&"x".repeat(65), false),
    ] {
        assert_eq!(valid_blob_name(name), valid, "{name:?}");
    }
}
