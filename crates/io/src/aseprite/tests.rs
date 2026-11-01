use super::*;
use photocraft_color::ColorMode;
use photocraft_doc::LayerContent;
use photocraft_geom::Rect;

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
    for extra in [layer("Tile", 2, 0, 3, 0, 255), layer("Bad mode", 0, 0, 3, 200, 255), chunk(0x2023, &[]), chunk(0x2006, &[]), chunk(0xffff, &[])] {
        assert!(read(&sprite(32, 1, &[plain.clone(), extra])).is_err());
    }
    let mut linked = cel(0, [1, 1], [0, 0], 255, &[0; 4], false);
    linked[13..15].copy_from_slice(&1u16.to_le_bytes());
    assert!(read(&sprite(32, 1, &[plain.clone(), linked])).is_err());
    let mut z = cel(0, [1, 1], [0, 0], 255, &[0; 4], false);
    z[15..17].copy_from_slice(&1i16.to_le_bytes());
    let result = read(&sprite(32, 1, &[plain, z])).unwrap();
    assert_eq!(result.document.sprite.unwrap().frames[0].cels.values().next().unwrap().z_index, 1);
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
    let custom = read(&sprite(32, 1, &[chunks[0].clone(), profile(1, 1, 100_000, &[])])).unwrap();
    assert!(custom.document.icc_profile.is_some());
    assert!(read(&sprite(32, 1, &[chunks[0].clone(), profile(2, 0, 0, b"bad")])).is_err());
}

#[test]
fn known_metadata_is_preserved_and_external_references_are_never_followed() {
    let mut external = 1u32.to_le_bytes().to_vec();
    external.extend([0; 8]);
    external.extend(1u32.to_le_bytes());
    external.extend([0; 8]);
    let name = b"/private/never-read";
    external.extend((name.len() as u16).to_le_bytes());
    external.extend(name);
    let mut user = 1u32.to_le_bytes().to_vec();
    user.extend(2u16.to_le_bytes());
    user.extend(b"OK");
    let chunks = [layer("L", 0, 0, 3, 0, 255), chunk(0x2020, &user), chunk(0x2008, &external)];
    let r = read(&sprite(32, 1, &chunks)).unwrap();
    assert_eq!(r.warnings.len(), 1);
    assert!(r.warnings.iter().any(|w| w.contains("no external files were read")));
    let s = r.document.sprite.as_ref().unwrap();
    assert_eq!(s.frames[0].metadata[0].bytes.as_slice(), user);
    let out = super::export(&r.document, &Interrupt::NONE).unwrap();
    let back = read(&out.bytes).unwrap();
    assert_eq!(back.document.sprite.as_ref().unwrap().frames[0].metadata.len(), 2);
}

fn palette(colors: &[[u8; 4]]) -> Vec<u8> {
    let mut b = (colors.len() as u32).to_le_bytes().to_vec();
    b.extend(0u32.to_le_bytes());
    b.extend((colors.len() as u32 - 1).to_le_bytes());
    b.extend([0; 8]);
    for (i, color) in colors.iter().enumerate() {
        b.extend(1u16.to_le_bytes());
        b.extend(color);
        let name = format!("Color {i}");
        b.extend((name.len() as u16).to_le_bytes());
        b.extend(name.as_bytes());
    }
    chunk(0x2019, &b)
}

fn frames(depth: u16, frames: &[(u16, Vec<Vec<u8>>)]) -> Vec<u8> {
    let mut b = sprite(depth, 3, &[])[..128].to_vec();
    b[6..8].copy_from_slice(&(frames.len() as u16).to_le_bytes());
    for (duration, chunks) in frames {
        let len = 16 + chunks.iter().map(Vec::len).sum::<usize>();
        b.extend((len as u32).to_le_bytes());
        b.extend(0xf1fau16.to_le_bytes());
        b.extend((chunks.len() as u16).to_le_bytes());
        b.extend(duration.to_le_bytes());
        b.extend([0; 6]);
        for c in chunks {
            b.extend(c);
        }
    }
    let len = b.len() as u32;
    b[..4].copy_from_slice(&len.to_le_bytes());
    b
}

fn linked(index: u16, at: [i16; 2], frame: u16) -> Vec<u8> {
    let mut b = index.to_le_bytes().to_vec();
    for n in at {
        b.extend(n.to_le_bytes());
    }
    b.push(128);
    b.extend(1u16.to_le_bytes());
    b.extend([0; 7]);
    b.extend(frame.to_le_bytes());
    chunk(0x2005, &b)
}

#[test]
fn animated_indexed_sprite_roundtrips_indices_links_palettes_tags_and_native_edits() {
    let first = [[0, 0, 0, 255], [255, 0, 0, 255], [255, 0, 0, 255], [0, 0, 255, 128]];
    let mut second = first;
    second[1] = [0, 255, 0, 255];
    let mut tag = 1u16.to_le_bytes().to_vec();
    tag.extend([0; 8]);
    tag.extend(0u16.to_le_bytes());
    tag.extend(1u16.to_le_bytes());
    tag.push(2);
    tag.extend(3u16.to_le_bytes());
    tag.extend([0; 6]);
    tag.extend([1, 2, 3, 0]);
    tag.extend(4u16.to_le_bytes());
    tag.extend(b"Walk");
    let bytes = frames(
        8,
        &[
            (17, vec![layer("Ink", 0, 0, 3, 0, 255), palette(&first), cel(0, [2, 1], [-1, 0], 255, &[1, 2], true), chunk(0x2018, &tag)]),
            (83, vec![palette(&second), linked(0, [0, 1], 0)]),
            (29, vec![]),
        ],
    );
    let mut doc = read(&bytes).unwrap().document;
    assert_eq!(doc.mode, ColorMode::Indexed);
    let id = doc.layers[0].id;
    assert_eq!(doc.sprite.as_ref().unwrap().frames[0].cels[&id].indices.as_ref().unwrap().to_interleaved(Rect::from_xywh(-1, 0, 2, 1)), [1, 2]);
    doc.select_sprite_frame(1).unwrap();
    assert_eq!(doc.layers[0].surface().unwrap().pixel(0, 1), [0.0, 1.0, 0.0, 1.0]);
    let LayerContent::Raster(surface) = &mut doc.layers[0].content else { panic!("raster") };
    surface.write_pixel(0, 1, &[0.0, 0.0, 1.0, 128.0 / 255.0]);
    doc.capture_sprite_frame().unwrap();
    let native = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    let back = photocraft_format::load_from_bytes(&native).unwrap();
    let out = super::export(&back, &Interrupt::NONE).unwrap();
    let mut back = read(&out.bytes).unwrap().document;
    let s = back.sprite.as_ref().unwrap();
    assert_eq!(s.frames.iter().map(|f| f.duration_ms).collect::<Vec<_>>(), [17, 83, 29]);
    assert_eq!(s.tags[0].name, "Walk");
    assert_eq!(s.tags[0].repeat, 3);
    assert_eq!(s.tags[0].direction, 2);
    let id = back.layers[0].id;
    for (i, rect) in [(0, Rect::from_xywh(-1, 0, 2, 1)), (1, Rect::from_xywh(0, 1, 2, 1))] {
        assert_eq!(s.frames[i].cels[&id].indices.as_ref().unwrap().to_interleaved(rect), [3, 2]);
    }
    assert_eq!(s.frames[1].cels[&id].linked_frame, Some(0));
    assert!(s.frames[2].cels.is_empty());
    assert_eq!(s.frames[0].palette[1].rgba, first[1]);
    assert_eq!(s.frames[1].palette[1].rgba, second[1]);
    back.select_sprite_frame(2).unwrap();
    assert_eq!(back.layers[0].surface().unwrap().tile_count(), 0);
}

#[test]
fn forward_links_resolve_and_link_cycles_or_missing_targets_fail() {
    let good = frames(32, &[(17, vec![layer("L", 0, 0, 3, 0, 255), linked(0, [-2, 1], 1)]), (83, vec![cel(0, [1, 1], [0, 0], 255, &[1, 2, 3, 255], true)])]);
    let doc = read(&good).unwrap().document;
    assert_eq!(doc.layers[0].surface().unwrap().to_interleaved(Rect::from_xywh(-2, 1, 1, 1)), [1, 2, 3, 255]);
    for target in [0, 2, 65535] {
        let b = frames(32, &[(17, vec![layer("L", 0, 0, 3, 0, 255), linked(0, [0, 0], target)])]);
        assert!(read(&b).is_err());
    }
}

#[test]
fn legacy_palette_component_ranges_and_modern_palette_precedence_follow_the_spec() {
    for (kind, sample, expected) in [(0x0004, 200, 200), (0x0011, 63, 255)] {
        let old = chunk(kind, &[1, 0, 0, 1, sample, 0, 0]);
        let image = cel(0, [1, 1], [0, 0], 255, &[0], false);
        let bytes = sprite(8, 1, &[layer("Background", 0, 0, 11, 0, 255), old.clone(), image.clone()]);
        assert_eq!(read(&bytes).unwrap().document.sprite.as_ref().unwrap().frames[0].palette[0].rgba, [expected, 0, 0, 255]);
        for before in [true, false] {
            let modern = palette(&[[0, 255, 0, 255]]);
            let (a, b) = if before { (old.clone(), modern) } else { (modern, old.clone()) };
            let bytes = sprite(8, 1, &[layer("Background", 0, 0, 11, 0, 255), a, b, image.clone()]);
            assert_eq!(read(&bytes).unwrap().document.sprite.as_ref().unwrap().frames[0].palette[0].rgba, [0, 255, 0, 255]);
        }
    }
}

#[test]
fn export_places_sprite_data_before_layers_and_completes_tileset_user_data_sequences() {
    fn kinds(bytes: &[u8]) -> Vec<u16> {
        let mut r = reader::Reader::new(&bytes[144..]);
        let mut out = Vec::new();
        while r.remaining() != 0 {
            let n = r.u32().unwrap() as usize - 6;
            out.push(r.u16().unwrap());
            r.take(n).unwrap();
        }
        out
    }
    let doc = read(include_bytes!("../../tests/fixtures/aseprite/animation.aseprite")).unwrap().document;
    let out = super::export(&doc, &Interrupt::NONE).unwrap();
    let frame_len = u32::from_le_bytes(out.bytes[128..132].try_into().unwrap()) as usize;
    let order = kinds(&out.bytes[..128 + frame_len]);
    assert_eq!(&order[..4], &[0x2019, 0x2020, 0x2004, 0x2020]);
    let doc = read(include_bytes!("../../tests/fixtures/aseprite/tiles.aseprite")).unwrap().document;
    let out = super::export(&doc, &Interrupt::NONE).unwrap();
    let order = kinds(&out.bytes);
    assert_eq!(&order[..5], &[0x2023, 0x2020, 0x2020, 0x2020, 0x2004]);
}

#[test]
fn empty_documents_roundtrip_without_inventing_layers() {
    let doc = photocraft_doc::Document::new("Empty", photocraft_geom::Size::new(4, 3), ColorMode::Rgb, photocraft_color::SampleType::U8);
    let out = super::export(&doc, &Interrupt::NONE).unwrap();
    let back = read(&out.bytes).unwrap().document;
    assert!(back.layers.is_empty());
    assert!(back.sprite.as_ref().unwrap().frames[0].cels.is_empty());
    assert_eq!(back.size, doc.size);
}

#[test]
fn explicit_empty_cels_are_distinct_from_absent_cels_and_survive_roundtrips() {
    for compressed in [false, true] {
        let bytes = frames(32, &[(17, vec![layer("L", 0, 0, 3, 0, 255), cel(0, [0, 65535], [-1, 2], 127, &[], compressed)]), (83, vec![])]);
        let doc = read(&bytes).unwrap().document;
        let s = doc.sprite.as_ref().unwrap();
        let c = s.frames[0].cels.values().next().unwrap();
        assert_eq!(c.rect, Rect::from_xywh(-1, 2, 0, 65535));
        assert_eq!(c.pixels.tile_count(), 0);
        assert!(s.frames[1].cels.is_empty());
        let native = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
        let back = photocraft_format::load_from_bytes(&native).unwrap();
        let output = super::export(&back, &Interrupt::NONE).unwrap();
        let back = read(&output.bytes).unwrap().document;
        assert_eq!(back.sprite.as_ref().unwrap().frames[0].cels.len(), 1);
        assert!(back.sprite.as_ref().unwrap().frames[1].cels.is_empty());
    }
}

#[test]
fn indexed_tilemaps_keep_nonzero_empty_index_and_tile_samples() {
    let mut definition = layer("Tiles", 2, 0, 3, 0, 255);
    definition.extend(42u32.to_le_bytes());
    let len = definition.len() as u32;
    definition[..4].copy_from_slice(&len.to_le_bytes());
    let mut set = 42u32.to_le_bytes().to_vec();
    set.extend(6u32.to_le_bytes());
    set.extend(2u32.to_le_bytes());
    set.extend(2u16.to_le_bytes());
    set.extend(2u16.to_le_bytes());
    set.extend([0; 16]);
    set.extend(1u16.to_le_bytes());
    set.push(b'T');
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&[3, 3, 3, 3, 1, 2, 4, 0], 6);
    set.extend((compressed.len() as u32).to_le_bytes());
    set.extend(compressed);
    let mut map = vec![0; 16];
    map[6] = 255;
    map[7..9].copy_from_slice(&3u16.to_le_bytes());
    map.extend(2u16.to_le_bytes());
    map.extend(1u16.to_le_bytes());
    map.extend(32u16.to_le_bytes());
    for n in [0x1fffffff_u32, 0x80000000, 0x40000000, 0x20000000] {
        map.extend(n.to_le_bytes());
    }
    map.extend([0; 10]);
    let ids = [0u32, 1].into_iter().flat_map(u32::to_le_bytes).collect::<Vec<_>>();
    map.extend(miniz_oxide::deflate::compress_to_vec_zlib(&ids, 6));
    let mut bytes = sprite(
        8,
        3,
        &[
            definition,
            palette(&[[255, 255, 0, 255], [255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 0, 0], [0, 0, 255, 128]]),
            chunk(0x2023, &set),
            chunk(0x2005, &map),
        ],
    );
    bytes[28] = 3;
    let doc = read(&bytes).unwrap().document;
    let s = doc.sprite.as_ref().unwrap();
    let c = s.frames[0].cels.values().next().unwrap();
    assert_eq!(c.indices.as_ref().unwrap().to_interleaved(Rect::from_xywh(0, 0, 4, 2)), [3, 3, 1, 2, 3, 3, 4, 0]);
    assert_eq!(c.pixels.pixel(0, 0)[3], 0.0);
    assert_eq!(c.pixels.pixel(3, 1), [1.0, 1.0, 0.0, 1.0]);
    let out = super::export(&doc, &Interrupt::NONE).unwrap();
    assert!(read(&out.bytes).is_ok());
}

#[test]
fn precise_reference_and_pixel_aspect_survive_export_without_replacing_original_samples() {
    let mut extra = 1u32.to_le_bytes().to_vec();
    for n in [65536i32, 0, 2 * 65536, 65536] {
        extra.extend(n.to_le_bytes());
    }
    extra.extend([0; 16]);
    let mut bytes = sprite(32, 3, &[layer("Reference", 0, 0, 67, 0, 255), cel(0, [1, 1], [0, 0], 255, &[12, 34, 56, 255], true), chunk(0x2006, &extra)]);
    bytes[34..36].copy_from_slice(&[2, 1]);
    let doc = read(&bytes).unwrap().document;
    assert!(doc.layers[0].locks.all);
    assert_eq!(doc.layers[0].surface().unwrap().to_interleaved(Rect::from_xywh(1, 0, 2, 1)), [12, 34, 56, 255, 12, 34, 56, 255]);
    let native = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    let back = photocraft_format::load_from_bytes(&native).unwrap();
    let out = super::export(&back, &Interrupt::NONE).unwrap();
    let back = read(&out.bytes).unwrap().document;
    let s = back.sprite.as_ref().unwrap();
    let cel = s.frames[0].cels.values().next().unwrap();
    assert_eq!(cel.rect, Rect::from_xywh(0, 0, 1, 1));
    assert_eq!(cel.precise, Some([65536, 0, 2 * 65536, 65536]));
    assert_eq!(s.pixel_aspect, [2, 1]);
    assert_eq!(cel.pixels.to_interleaved(cel.rect), [12, 34, 56, 255]);
}

#[test]
fn typed_properties_and_slice_keys_are_retained_and_malformed_metadata_is_rejected() {
    let mut props = 1u32.to_le_bytes().to_vec();
    props.extend(0u32.to_le_bytes());
    props.extend(1u32.to_le_bytes());
    props.extend(5u16.to_le_bytes());
    props.extend(b"value");
    props.extend(0x11u16.to_le_bytes());
    props.extend(2u32.to_le_bytes());
    props.extend(7u16.to_le_bytes());
    props.extend(42u32.to_le_bytes());
    props.extend(84u32.to_le_bytes());
    let mut user = 4u32.to_le_bytes().to_vec();
    user.extend(((props.len() + 4) as u32).to_le_bytes());
    user.extend(props);
    let mut slices = 1u32.to_le_bytes().to_vec();
    slices.extend(3u32.to_le_bytes());
    slices.extend([0; 4]);
    slices.extend(3u16.to_le_bytes());
    slices.extend(b"Box");
    for n in [0i32, -1, 2, 10, 12, -2, -3, 4, 5, 6, 7] {
        slices.extend(n.to_le_bytes());
    }
    let bytes = sprite(32, 3, &[layer("L", 0, 0, 3, 0, 255), chunk(0x2020, &user), chunk(0x2022, &slices)]);
    let doc = read(&bytes).unwrap().document;
    let output = super::export(&doc, &Interrupt::NONE).unwrap();
    let back = read(&output.bytes).unwrap().document;
    let chunks = &back.sprite.as_ref().unwrap().frames[0].metadata;
    assert!(chunks.iter().any(|c| c.bytes.as_slice() == user));
    assert!(chunks.iter().any(|c| c.bytes.as_slice() == slices));
    for (kind, data) in [(0x2020, vec![255; 4]), (0x2022, vec![255; 16]), (0x2008, vec![255; 12])] {
        assert!(read(&sprite(32, 3, &[layer("L", 0, 0, 3, 0, 255), chunk(kind, &data)])).is_err());
    }
}

#[test]
fn every_documented_blend_identifier_survives_aseprite_and_native_roundtrips() {
    for mode in 0..19u16 {
        let doc = read(&sprite(32, 3, &[layer("Mode", 0, 0, 3, mode, 128), cel(0, [1, 1], [0, 0], 64, &[32, 64, 96, 255], true)])).unwrap().document;
        let output = super::export(&doc, &Interrupt::NONE).unwrap();
        let back = read(&output.bytes).unwrap().document;
        assert_eq!(back.layers[0].blend, blend_mode(mode).unwrap());
        assert!((back.layers[0].opacity - doc.layers[0].opacity).abs() < 1e-6);
        assert_eq!(back.sprite.as_ref().unwrap().frames[0].cels.values().next().unwrap().opacity, 64);
        let expected = photocraft_compose::flatten(&doc);
        let actual = photocraft_compose::flatten(&back);
        assert_eq!(expected.px, actual.px);
    }
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
