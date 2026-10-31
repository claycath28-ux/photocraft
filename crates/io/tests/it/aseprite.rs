//! Single-frame Aseprite documents keep editable layers (#3811).

use photocraft_doc::LayerContent;

const LAYERS: &[u8] = include_bytes!("../fixtures/aseprite/layers.aseprite");

#[test]
fn aseprite_layers_open_with_their_tree_offsets_and_original_pixels() {
    let r = photocraft_io::import("layers.aseprite", LAYERS).unwrap();
    assert!(r.source_read_only);
    assert!(!r.preview_only);
    let doc = r.document;
    assert_eq!((doc.size.width, doc.size.height), (2, 2));
    assert_eq!(doc.layers.len(), 2);
    assert_eq!(doc.layers[0].name, "Base");
    let LayerContent::Group(group) = &doc.layers[1].content else { panic!("editable group") };
    assert_eq!(group.children[0].name, "Top");
    let top = group.children[0].surface().unwrap();
    assert_eq!(top.pixel(0, 0), [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(top.pixel(1, 0), [0.0, 0.0, 1.0, 128.0 / 255.0]);
    assert_eq!(top.pixel(1, 1), [0.0, 1.0, 0.0, 1.0]);
    let flat = photocraft_compose::flatten(&doc);
    let expected = [[1.0, 0.0, 0.0, 1.0], [127.0 / 255.0, 0.0, 128.0 / 255.0, 1.0], [1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]];
    for (a, b) in flat.px.iter().zip(expected) {
        for c in 0..4 {
            assert!((a[c] - b[c]).abs() < 1.0 / 255.0 + 1e-6, "{a:?} != {b:?}")
        }
    }
}

#[test]
fn aseprite_native_and_psd_round_trips_preserve_pixels_and_layers() {
    let r = photocraft_io::import("layers.ase", LAYERS).unwrap();
    let doc = r.document;
    for ext in ["pcraft", "psd"] {
        let out = photocraft_io::export(&doc, ext, &Default::default()).unwrap();
        let back = photocraft_io::import(&format!("back.{ext}"), &out.bytes).unwrap().document;
        assert_eq!(back.layers.len(), doc.layers.len());
        assert_eq!(back.layers[1].children().unwrap()[0].name, "Top");
        for (a, b) in photocraft_compose::flatten(&doc).px.iter().zip(photocraft_compose::flatten(&back).px) {
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() < 1.0 / 255.0 + 1e-6, "{ext}: {a:?} != {b:?}")
            }
        }
    }
    assert!(photocraft_io::import("renamed.psd", LAYERS).is_ok());
    assert!(photocraft_io::import("not-a-sprite.aseprite", b"ASEF\0\x01").is_err());
}

#[test]
fn aseprite_independent_writer_grayscale_cel_has_exact_samples() {
    let bytes = include_bytes!("../fixtures/aseprite/gray.aseprite");
    let r = photocraft_io::import("gray.aseprite", bytes).unwrap();
    assert_eq!(r.document.mode, photocraft_color::ColorMode::Grayscale);
    let surface = r.document.layers[0].surface().unwrap();
    let region = photocraft_geom::Rect::from_xywh(-1, 1, 3, 1);
    assert_eq!(surface.to_interleaved(region), [32, 0, 128, 128, 240, 255]);
    let out = photocraft_io::export(&r.document, "pcraft", &Default::default()).unwrap();
    let back = photocraft_io::import("back.pcraft", &out.bytes).unwrap().document;
    assert_eq!(back.layers[0].surface().unwrap().to_interleaved(region), surface.to_interleaved(region));
}

/// Opt-in import timing and boundary-tile accounting on a 25 MP source; not a CI budget.
#[test]
#[ignore = "25 MP release-mode import benchmark"]
fn aseprite_import_25mp_release_benchmark() {
    // Retain the original Base layer, replace its cel with a large compressed checkerboard.
    let mut pixels = vec![0; 5000 * 5000 * 4];
    for (i, p) in pixels.chunks_exact_mut(4).enumerate() {
        p.copy_from_slice(if (i / 5000 / 32 + i % 5000 / 32) % 2 == 0 { &[32, 64, 96, 255] } else { &[192, 160, 128, 255] });
    }
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&pixels, 6);
    drop(pixels);
    let mut bytes = LAYERS[..128].to_vec();
    let layer = &LAYERS[144..172];
    let mut cel = vec![0; 26];
    let cel_len = (cel.len() + compressed.len()) as u32;
    cel[..4].copy_from_slice(&cel_len.to_le_bytes());
    cel[4..6].copy_from_slice(&0x2005u16.to_le_bytes());
    cel[12] = 255;
    cel[13..15].copy_from_slice(&2u16.to_le_bytes());
    cel[22..24].copy_from_slice(&5000u16.to_le_bytes());
    cel[24..26].copy_from_slice(&5000u16.to_le_bytes());
    let frame_len = (16 + layer.len() + cel.len() + compressed.len()) as u32;
    bytes.extend(frame_len.to_le_bytes());
    bytes.extend(0xf1fau16.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend([0; 8]);
    bytes.extend(layer);
    bytes.extend(cel);
    bytes.extend(compressed);
    let len = bytes.len() as u32;
    bytes[..4].copy_from_slice(&len.to_le_bytes());
    bytes[8..10].copy_from_slice(&5000u16.to_le_bytes());
    bytes[10..12].copy_from_slice(&5000u16.to_le_bytes());
    let start = std::time::Instant::now();
    let r = photocraft_io::import("25mp.aseprite", &bytes).unwrap();
    let elapsed = start.elapsed();
    let surface = r.document.layers[0].surface().unwrap();
    assert_eq!(surface.pixel(0, 0), [32.0 / 255.0, 64.0 / 255.0, 96.0 / 255.0, 1.0]);
    assert_eq!(surface.content_bounds(), r.document.bounds());
    eprintln!("25 MP Aseprite import: {elapsed:?}, {} source bytes", bytes.len());
}
