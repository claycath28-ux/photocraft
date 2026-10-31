use super::*;

fn chunk(kind: u16, data: &[u8]) -> Vec<u8> {
    let mut out = ((data.len() + 6) as u32).to_le_bytes().to_vec();
    out.extend(kind.to_le_bytes());
    out.extend(data);
    out
}

fn sprite(depth: u16, flags: u32, chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut h = vec![0; 128];
    h[4..6].copy_from_slice(&0xa5e0u16.to_le_bytes());
    h[6..8].copy_from_slice(&1u16.to_le_bytes());
    h[8..10].copy_from_slice(&4u16.to_le_bytes());
    h[10..12].copy_from_slice(&3u16.to_le_bytes());
    h[12..14].copy_from_slice(&depth.to_le_bytes());
    h[14..18].copy_from_slice(&flags.to_le_bytes());
    h[34..36].copy_from_slice(&[1, 1]);
    let len = 16 + chunks.iter().map(Vec::len).sum::<usize>();
    h.extend((len as u32).to_le_bytes());
    h.extend(0xf1fau16.to_le_bytes());
    h.extend((chunks.len() as u16).to_le_bytes());
    h.extend([0; 8]);
    for c in chunks {
        h.extend(c)
    }
    let len = h.len() as u32;
    h[..4].copy_from_slice(&len.to_le_bytes());
    h
}

fn layer(name: &str, kind: u16, level: u16, flags: u16, blend: u16, opacity: u8) -> Vec<u8> {
    let mut b = Vec::new();
    for n in [flags, kind, level, 0, 0, blend] {
        b.extend(n.to_le_bytes())
    }
    b.extend([opacity, 0, 0, 0]);
    b.extend((name.len() as u16).to_le_bytes());
    b.extend(name.as_bytes());
    chunk(0x2004, &b)
}

fn cel(index: u16, size: [u16; 2], at: [i16; 2], opacity: u8, data: &[u8], compressed: bool) -> Vec<u8> {
    let mut b = index.to_le_bytes().to_vec();
    for n in at {
        b.extend(n.to_le_bytes())
    }
    b.push(opacity);
    b.extend((if compressed { 2u16 } else { 0 }).to_le_bytes());
    b.extend([0; 7]);
    for n in size {
        b.extend(n.to_le_bytes())
    }
    b.extend(if compressed { miniz_oxide::deflate::compress_to_vec_zlib(data, 6) } else { data.to_vec() });
    chunk(0x2005, &b)
}

fn read(bytes: &[u8]) -> Result<ImportResult> {
    crate::import("t.aseprite", bytes)
}

#[test]
fn document_detection_requires_a_plausible_header_and_frame_not_just_two_magic_bytes() {
    let good = include_bytes!("../../tests/fixtures/aseprite/layers.aseprite");
    assert!(is_aseprite(good));
    let mut accidental = vec![0; 144];
    accidental[4..6].copy_from_slice(&[0xe0, 0xa5]);
    assert!(!is_aseprite(&accidental));
    assert!(!is_aseprite(b"ASEF\0\x01\0\0"));
    for n in 0..good.len() {
        assert!(!is_aseprite(&good[..n]))
    }
}

#[test]
fn raw_and_compressed_gray_and_rgba_keep_hidden_colors_offsets_opacity_and_locks() {
    for gray in [false, true] {
        for compressed in [false, true] {
            let px: &[u8] = if gray { &[91, 0, 202, 255] } else { &[32, 64, 96, 0, 1, 2, 3, 255] };
            let bytes = sprite(if gray { 16 } else { 32 }, 1, &[layer("Hidden", 0, 0, 4 | 8, 1, 128), cel(0, [2, 1], [-1, 2], 64, px, compressed)]);
            let r = read(&bytes).unwrap();
            let l = &r.document.layers[0];
            assert_eq!(l.blend, BlendMode::Multiply);
            assert_eq!(l.opacity, (128.0 / 255.0) * (64.0 / 255.0));
            assert_eq!(l.fill_opacity, 1.0);
            assert!(!l.visible && l.locks.all && l.locks.position && l.locks.transparency);
            let s = l.surface().unwrap();
            assert_eq!(s.to_interleaved(Rect::from_xywh(-1, 2, 2, 1)), px);
            assert_eq!(r.document.mode, if gray { ColorMode::Grayscale } else { ColorMode::Rgb });
        }
    }
}

#[test]
fn groups_keep_bottom_to_top_order_and_header_compositing_policy() {
    for flags in [0, 1, 3] {
        let r = read(&sprite(
            32,
            flags,
            &[
                layer("Bottom", 0, 0, 3, 0, 0),
                layer("Group", 1, 0, 3 | 32, 2, 127),
                layer("A", 0, 1, 3, 0, 255),
                layer("Nested", 1, 1, 3, 0, 255),
                layer("B", 0, 2, 3, 0, 255),
                layer("C", 0, 1, 3, 0, 255),
                layer("Top", 0, 0, 3, 0, 255),
            ],
        ))
        .unwrap();
        let layers = r.document.layers;
        assert_eq!(layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Bottom", "Group", "Top"]);
        assert_eq!(layers[0].opacity, if flags == 0 { 1.0 } else { 0.0 });
        let g = &layers[1];
        assert_eq!(g.blend, if flags & 2 == 0 { BlendMode::PassThrough } else { BlendMode::Screen });
        assert_eq!(g.opacity, if flags & 2 == 0 { 1.0 } else { 127.0 / 255.0 });
        let LayerContent::Group(group) = &g.content else { panic!("group") };
        assert!(!group.expanded);
        assert_eq!(group.children.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["A", "Nested", "C"]);
        assert_eq!(group.children[1].children().unwrap()[0].name, "B");
    }
}

#[test]
fn unsupported_rendering_features_are_errors_instead_of_flattening_or_dropping_them() {
    let plain = layer("L", 0, 0, 3, 0, 255);
    for (offset, value) in [(6, 2u16), (12, 8), (8, 0)] {
        let mut b = sprite(32, 1, std::slice::from_ref(&plain));
        b[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        assert!(read(&b).is_err());
    }
    for extra in [
        layer("Tile", 2, 0, 3, 0, 255),
        layer("Reference", 0, 0, 67, 0, 255),
        layer("Bad mode", 0, 0, 3, 200, 255),
        chunk(0x2023, &[]),
        chunk(0x2006, &[]),
        chunk(0xffff, &[]),
    ] {
        assert!(read(&sprite(32, 1, &[plain.clone(), extra])).is_err());
    }
    let mut linked = cel(0, [1, 1], [0, 0], 255, &[0; 4], false);
    linked[13..15].copy_from_slice(&1u16.to_le_bytes());
    assert!(read(&sprite(32, 1, &[plain.clone(), linked])).is_err());
    let mut z = cel(0, [1, 1], [0, 0], 255, &[0; 4], false);
    z[15..17].copy_from_slice(&1i16.to_le_bytes());
    assert!(read(&sprite(32, 1, &[plain, z])).is_err());
}

#[test]
fn nesting_references_and_tile_allocation_are_bounded() {
    let plain = layer("L", 0, 0, 3, 0, 255);
    for chunks in [
        vec![layer("Orphan", 0, 1, 3, 0, 255)],
        vec![plain.clone(), layer("Bad child", 0, 1, 3, 0, 255)],
        vec![plain.clone(), cel(1, [1, 1], [0, 0], 255, &[0; 4], false)],
        vec![plain.clone(), cel(0, [65535, 65535], [-1, -1], 255, &[], true)],
        vec![plain.clone(), cel(0, [1, 1], [0, 0], 255, &[0; 4], false), cel(0, [1, 1], [0, 0], 255, &[0; 4], false)],
    ] {
        assert!(read(&sprite(32, 1, &chunks)).is_err())
    }
    let chunks: Vec<_> = (0..=photocraft_doc::MAX_GROUP_DEPTH + 1).map(|n| layer("G", 1, n as u16, 3, 0, 255)).collect();
    assert!(read(&sprite(32, 1, &chunks)).is_err());
    let chunks = vec![plain; MAX_LAYERS + 1];
    assert!(read(&sprite(32, 1, &chunks)).is_err());
}

#[test]
fn zlib_is_checked_for_exact_size_checksum_and_trailing_bytes() {
    let plain = layer("L", 0, 0, 3, 0, 255);
    for px in [vec![0; 3], vec![0; 5], vec![0; 100_000]] {
        assert!(read(&sprite(32, 1, &[plain.clone(), cel(0, [1, 1], [0, 0], 255, &px, true)])).is_err());
    }
    let good = cel(0, [1, 1], [0, 0], 255, &[3, 2, 1, 255], true);
    let mut bad = good.clone();
    *bad.last_mut().unwrap() ^= 1;
    let mut tail = good.clone();
    tail.push(0);
    let len = tail.len() as u32;
    tail[..4].copy_from_slice(&len.to_le_bytes());
    for c in [bad, tail] {
        assert!(read(&sprite(32, 1, &[plain.clone(), c])).is_err())
    }
    assert!(read(&sprite(32, 1, &[plain, good])).is_ok());
}

fn profile(kind: u16, flags: u16, gamma: u32, icc: &[u8]) -> Vec<u8> {
    let mut b = kind.to_le_bytes().to_vec();
    b.extend(flags.to_le_bytes());
    b.extend(gamma.to_le_bytes());
    b.extend([0; 8]);
    if kind == 2 {
        b.extend((icc.len() as u32).to_le_bytes());
        b.extend(icc)
    }
    chunk(0x2007, &b)
}

#[test]
fn profiles_are_preserved_or_generated_for_the_actual_color_model() {
    for depth in [16, 32] {
        for linear in [false, true] {
            let p = profile(1, u16::from(linear), if linear { 65536 } else { 0 }, &[]);
            let doc = read(&sprite(depth, 1, &[layer("L", 0, 0, 3, 0, 255), p])).unwrap().document;
            let p = photocraft_cms::Profile::parse(doc.icc_profile.as_ref().unwrap()).unwrap();
            assert_eq!(p.color_space, if depth == 16 { photocraft_cms::ColorSpace::Gray } else { photocraft_cms::ColorSpace::Rgb });
        }
    }
    let icc = photocraft_cms::Builtin::DisplayP3.profile().to_bytes();
    let chunks = [layer("L", 0, 0, 3, 0, 255), profile(2, 0, 0, &icc)];
    assert_eq!(read(&sprite(32, 1, &chunks)).unwrap().document.icc_profile.unwrap().as_slice(), icc.as_slice());
    assert!(read(&sprite(16, 1, &chunks)).is_err());
    assert!(read(&sprite(32, 1, &[chunks[0].clone(), profile(1, 1, 100_000, &[])])).is_err());
    assert!(read(&sprite(32, 1, &[chunks[0].clone(), profile(2, 0, 0, b"bad")])).is_err());
}

#[test]
fn known_metadata_is_explicitly_warned_about_and_never_followed() {
    let chunks = [layer("L", 0, 0, 3, 0, 255), chunk(0x2008, b"/private/never-read"), chunk(0x2020, b"properties"), chunk(0x2020, b"more properties")];
    let r = read(&sprite(32, 1, &chunks)).unwrap();
    assert_eq!(r.warnings.len(), 2);
    assert!(r.warnings.iter().any(|w| w.contains("no external files were read")));
}

#[test]
fn truncation_and_bad_chunk_sizes_are_errors_and_cancellation_is_cooperative() {
    let good = include_bytes!("../../tests/fixtures/aseprite/layers.aseprite");
    for n in 0..good.len() {
        assert!(read(&good[..n]).is_err(), "prefix {n}")
    }
    for size in [0u32, 1, 5, u32::MAX] {
        let mut b = good.to_vec();
        b[144..148].copy_from_slice(&size.to_le_bytes());
        assert!(read(&b).is_err());
    }
    let yes = || true;
    assert!(matches!(crate::import_with("t.aseprite", good, &Interrupt::cancel_only(&yes)), Err(IoError::Cancelled)));
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let cancel = || calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 3;
    assert!(matches!(crate::import_with("t.aseprite", good, &Interrupt::cancel_only(&cancel)), Err(IoError::Cancelled)));
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(128))]
    #[test]
    fn mutated_records_never_panic(edits in proptest::collection::vec((0usize..304,proptest::prelude::any::<u8>()),0..12)) {
        let mut b=include_bytes!("../../tests/fixtures/aseprite/layers.aseprite").to_vec();
        for (i,v) in edits { b[i]=v }
        let _=read(&b);
    }
}
