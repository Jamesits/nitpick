use std::future::poll_fn;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nitpick_fixtures::LightLevelRequest;
use serde_json::json;

use super::*;
use crate::capture::{
    ByteOrder, Channel, Colour, Cursor, Field, PlaneLayout, Primaries, Rect, SampleKind, Source,
    SourceKind, TimeBase, Timestamp, Timestamps, Transfer, WhitePoint,
};
use crate::protocol::GamutReport;

fn qpc(ticks: u64) -> Timestamp {
    Timestamp {
        base: TimeBase::Qpc,
        ticks,
        ticks_per_second: 10_000_000,
    }
}

/// 2×2 px RGBA binary16 with 4 padding bytes per row, every API field.
#[allow(clippy::too_many_lines)] // One literal.
fn artifact(frame: u64) -> Artifact {
    let fields = [Channel::R, Channel::G, Channel::B, Channel::A]
        .into_iter()
        .zip((0..).step_by(16))
        .map(|(channel, shift)| Field {
            channel,
            shift,
            bits: 16,
            kind: SampleKind::Float,
        })
        .collect();
    let mut raw = Map::new();
    raw.insert(
        "ContentSize".into(),
        json!({"Width": 2, "Height": 2, "big": u64::MAX, "scale": 0.1}),
    );
    let mut config = Map::new();
    config.insert("pixel_format".into(), json!("R16G16B16A16Float"));
    Artifact {
        check: Check {
            result: CaptureOutcome::Pass,
            reason: None,
        },
        run: RunRecord {
            driver: "qrhi".into(),
            display: "qrhi:screen-1".into(),
            generation: 3,
            surface_size: [2, 2],
            mode: ModeReport {
                hdr_on: Some(true),
                gamut: Some(GamutReport::Rec2020),
                wire_depth: None,
                peak_nits: Some(1015.5),
                min_nits: None,
                sdr_white_nits: Some(240.0),
                pixel_ratio: 1.5,
                description: "HDR".into(),
            },
            frame,
            fixture: "state-frame-a-pq-full-10".into(),
            format: FormatRecord {
                encoding: "pq-full".into(),
                depth: 10,
            },
            metadata: Some(MetadataRequest {
                primaries: [[0.708, 0.292], [0.17, 0.797], [0.131, 0.046]],
                white: [0.3127, 0.329],
                max_mastering_nits: 4000.0,
                min_mastering_nits: 0.005,
                light_level: LightLevelRequest::FromFrame,
            }),
            delay_ms: 250,
            environment: Environment {
                os: Some("Windows 11 build 26200".into()),
                renderer: Some("Qt 6.12.0 D3D11".into()),
                ..Environment::default()
            },
        },
        acceptance: Acceptance {
            submitted: qpc(123_000_000),
            marker: 0xA5,
            marker_origin: [0, 36],
            frames: vec![qpc(123_400_000), qpc(123_456_789)],
            received: 4,
            deadline_ms: 30_000,
            heartbeat_ms: Some(100),
        },
        capture: Capture {
            descriptor: CaptureDescriptor {
                width: 2,
                height: 2,
                planes: vec![PlaneLayout {
                    row_pitch: 20,
                    subsampling: [1, 1],
                    block_bytes: 8,
                    byte_order: ByteOrder::Little,
                    fields,
                }],
                content: Some(Rect::new(0, 0, 2, 1)),
                cursor: Cursor::Shown {
                    rect: Rect::new(1, 1, 32, 32),
                },
                alpha: None,
                colour: Colour {
                    transfer: Some(Transfer::Linear),
                    primaries: Some(Primaries::Bt709),
                    white: Some(WhitePoint::D65),
                    range: None,
                    matrix: None,
                    // The capture contract is not known.
                    luminance: None,
                },
                timestamps: Timestamps {
                    frame: Some(qpc(123_456_789)),
                    arrival: None,
                },
                source: Source {
                    kind: SourceKind::Window,
                    native: "hwnd:0x1234".into(),
                },
            },
            // Padding bytes differ from sample bytes.
            planes: vec![(0u8..40).map(|b| b.wrapping_mul(37)).collect()],
            raw,
            raw_blobs: BTreeMap::from([("attachments".into(), vec![0, 255, 1])]),
            config,
            preview: Some(b"\x89PNG preview".to_vec()),
        },
    }
}

#[test]
fn artifact_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a");
    let a = artifact(7);
    a.capture.validate().unwrap();
    write(&dir, &a).unwrap();
    assert_eq!(read(&dir).unwrap(), a);

    // Unmodified samples, padding included; the preview is a separate file.
    assert_eq!(
        fs::read(dir.join("plane-0.bin")).unwrap(),
        a.capture.planes[0]
    );
    assert_eq!(
        fs::read(dir.join("raw-attachments.bin")).unwrap(),
        [0, 255, 1]
    );
    assert_eq!(
        fs::read(dir.join(PREVIEW)).unwrap(),
        a.capture.preview.clone().unwrap()
    );
    let mut files: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(
        files,
        ["plane-0.bin", PREVIEW, "raw-attachments.bin", RESULT]
    );
    let result: Value = serde_json::from_slice(&fs::read(dir.join(RESULT)).unwrap()).unwrap();
    assert_eq!(result["version"], json!(VERSION));
    assert_eq!(result["preview"], json!({"file": PREVIEW, "bytes": 12}));
    assert_eq!(result["descriptor"]["colour"]["luminance"], Value::Null);
    assert_eq!(
        result["descriptor"]["cursor"],
        json!({"state": "shown", "rect": {"x": 1, "y": 1, "width": 32, "height": 32}})
    );
    assert_eq!(result["acceptance"]["frames"].as_array().unwrap().len(), 2);
}

#[test]
fn round_trip_without_optional_parts() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a");
    let mut a = artifact(1);
    a.check = Check {
        result: CaptureOutcome::Fail,
        reason: Some("plane 0: row pitch".into()),
    };
    a.run.metadata = None;
    a.capture.raw_blobs.clear();
    a.capture.preview = None;
    a.capture.raw.clear();
    write(&dir, &a).unwrap();
    assert_eq!(read(&dir).unwrap(), a);
}

#[test]
fn write_does_not_replace_or_leave_partial_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a");
    write(&dir, &artifact(1)).unwrap();
    let e = write(&dir, &artifact(2)).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(read(&dir).unwrap().run.frame, 1);

    let mut bad = artifact(3);
    bad.capture.raw_blobs.insert("../escape".into(), vec![1]);
    let other = tmp.path().join("b");
    assert_eq!(
        write(&other, &bad).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    let names: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["a"]);
}

#[test]
fn read_rejects_changed_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a");
    write(&dir, &artifact(1)).unwrap();
    let result = fs::read(dir.join(RESULT)).unwrap();
    let original: Value = serde_json::from_slice(&result).unwrap();
    let edits: [fn(&mut Value); 4] = [
        |v| v["version"] = json!(VERSION + 1),
        |v| v["unexpected"] = json!(1),
        |v| v["planes"][0]["file"] = json!("../plane-0.bin"),
        |v| v["planes"][0]["bytes"] = json!(41),
    ];
    for (i, edit) in edits.into_iter().enumerate() {
        let mut v = original.clone();
        edit(&mut v);
        fs::write(dir.join(RESULT), serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(read(&dir).is_err(), "edit {i}");
    }
    fs::write(dir.join(RESULT), &result).unwrap();
    read(&dir).unwrap();
    fs::remove_file(dir.join("raw-attachments.bin")).unwrap();
    assert_eq!(read(&dir).unwrap_err().kind(), io::ErrorKind::NotFound);
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

/// Waits for the next write result of `w`.
fn next_saved(w: &mut ArtifactWriter) -> Option<Saved> {
    runtime().block_on(async {
        tokio::time::timeout(Duration::from_secs(10), poll_fn(|cx| w.poll_saved(cx)))
            .await
            .expect("a write result")
    })
}

#[test]
fn writer_names_and_writes_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("artifacts");
    let mut w = ArtifactWriter::start(root.clone(), 4).unwrap();
    assert_eq!(next_saved(&mut w), None);
    assert_eq!(w.submit(artifact(7)).unwrap(), "000001-frame-7");
    assert_eq!(w.submit(artifact(9)).unwrap(), "000002-frame-9");
    for (name, frame) in [("000001-frame-7", 7), ("000002-frame-9", 9)] {
        assert_eq!(
            next_saved(&mut w),
            Some(Saved {
                name: name.into(),
                frame,
                result: Ok(())
            })
        );
        assert_eq!(read(&root.join(name)).unwrap(), artifact(frame));
    }
    assert_eq!(next_saved(&mut w), None);
}

#[test]
fn start_needs_an_empty_root() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("old"), b"").unwrap();
    let e = ArtifactWriter::start(tmp.path().to_owned(), 1)
        .err()
        .unwrap();
    assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
}

#[test]
fn queue_is_bounded_until_results_are_taken() {
    let (gate, held) = std_mpsc::channel::<Result<(), String>>();
    let stored = Arc::new(Mutex::new(Vec::new()));
    let store = {
        let stored = Arc::clone(&stored);
        Box::new(move |name: &str, _: &Artifact| {
            stored.lock().unwrap().push(name.to_owned());
            held.recv().unwrap()
        })
    };
    let mut w = ArtifactWriter::with_store(2, store);
    w.submit(artifact(1)).unwrap();
    w.submit(artifact(2)).unwrap();
    let e = w.submit(artifact(3)).unwrap_err();
    assert!(e.contains("full"), "{e}");

    gate.send(Err("disk full".into())).unwrap();
    assert_eq!(
        next_saved(&mut w),
        Some(Saved {
            name: "000001-frame-1".into(),
            frame: 1,
            result: Err("disk full".into())
        })
    );
    // A rejected artifact takes no name.
    assert_eq!(w.submit(artifact(4)).unwrap(), "000003-frame-4");
    assert!(w.submit(artifact(5)).is_err());
    gate.send(Ok(())).unwrap();
    gate.send(Ok(())).unwrap();
    let names: Vec<_> = (0..2).map(|_| next_saved(&mut w).unwrap().name).collect();
    assert_eq!(names, ["000002-frame-2", "000003-frame-4"]);
    drop(w);
    assert_eq!(
        *stored.lock().unwrap(),
        ["000001-frame-1", "000002-frame-2", "000003-frame-4"]
    );
}

#[test]
fn stopped_writer_fails_the_queued_artifacts() {
    let store = Box::new(|_: &str, _: &Artifact| -> Result<(), String> {
        panic!("store failure");
    });
    let mut w = ArtifactWriter::with_store(2, store);
    w.submit(artifact(1)).unwrap();
    let saved = next_saved(&mut w).unwrap();
    assert_eq!(saved.name, "000001-frame-1");
    assert_eq!(saved.result, Err("the artifact writer stopped".into()));
    assert_eq!(next_saved(&mut w), None);
    assert!(w.submit(artifact(2)).is_err());
}
