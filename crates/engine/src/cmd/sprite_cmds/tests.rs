use super::*;
use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{
    Document, Layer, LayerContent,
    sprite::{SpriteCel, SpriteFrame, SpriteLayer},
};
use photocraft_geom::{Rect, Size};
use photocraft_raster::Surface;

fn session() -> Session {
    let mut doc = Document::new("Animation", Size::new(2, 1), ColorMode::Rgb, SampleType::U8);
    let mut surface = Surface::new(PixelFormat::RGBA8);
    surface.write_interleaved(Rect::from_xywh(0, 0, 1, 1), &[255, 0, 0, 255]);
    let layer = Layer::new("Ink", LayerContent::Raster(surface.clone()));
    let id = layer.id;
    doc.layers.push(layer);
    let first =
        Arc::new(SpriteCel { rect: Rect::from_xywh(0, 0, 1, 1), pixels: surface, indices: None, opacity: 255, z_index: 0, linked_frame: None, precise: None });
    let second = Arc::new(SpriteCel { linked_frame: Some(0), ..first.as_ref().clone() });
    doc.sprite = Some(Sprite {
        frames: vec![
            Arc::new(SpriteFrame { duration_ms: 17, palette: Arc::new(vec![]), cels: [(id, first)].into(), metadata: vec![] }),
            Arc::new(SpriteFrame { duration_ms: 83, palette: Arc::new(vec![]), cels: [(id, second)].into(), metadata: vec![] }),
        ],
        current: 0,
        layers: vec![SpriteLayer { id, flags: 3, opacity: 255, uuid: None }],
        indexed: false,
        transparent_index: 0,
        pixel_aspect: [1, 1],
        grid: [0; 4],
        tags: vec![],
        profile: None,
    });
    doc.timeline = Some(photocraft_doc::Timeline::new(2, 10.0));
    let mut s = Session::new();
    s.add_document(doc, None);
    s
}

#[test]
fn sprite_edits_and_frame_navigation_keep_cels_and_undo() {
    let mut s = session();
    assert_eq!(s.execute("sprite.info", json!({})).unwrap()["durationsMs"], json!([17, 83]));
    s.execute("sprite.setDuration", json!({"durationMs":47})).unwrap();
    let n = s.active().unwrap().history.past_len();
    s.execute("timeline.setFrame", json!({"frame":1})).unwrap();
    assert_eq!(s.active().unwrap().history.past_len(), n, "navigation is not a history step");
    let id = s.active().unwrap().doc.layers[0].id.0;
    s.execute("sprite.setCel", json!({"layer":id,"x":-2,"y":1,"opacity":127,"zIndex":-3})).unwrap();
    let frame = &s.active().unwrap().doc.sprite.as_ref().unwrap().frames[1];
    assert_eq!(frame.cels[&LayerId(id)].rect, Rect::from_xywh(-2, 1, 1, 1));
    s.execute("sprite.unlinkCel", json!({"layer":id})).unwrap();
    assert!(s.active().unwrap().doc.sprite.as_ref().unwrap().frames[1].cels[&LayerId(id)].linked_frame.is_none());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.sprite.as_ref().unwrap().frames[1].cels[&LayerId(id)].linked_frame, Some(0));
    s.execute("sprite.duplicateFrame", json!({})).unwrap();
    assert_eq!(s.execute("sprite.info", json!({})).unwrap()["frames"], 3);
    s.execute("sprite.setTags", json!({"tags":[{"name":"Walk","from":0,"to":2,"direction":2,"repeat":3,"color":[1,2,3]}]})).unwrap();
    assert_eq!(s.execute("sprite.info", json!({})).unwrap()["tags"][0]["repeat"], 3);
    s.execute("sprite.deleteFrame", json!({})).unwrap();
    assert_eq!(s.execute("sprite.info", json!({})).unwrap()["frames"], 2);
    s.execute("sprite.detach", json!({})).unwrap();
    assert!(s.active().unwrap().doc.sprite.is_none());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.active().unwrap().doc.sprite.is_some());
}

#[test]
fn malformed_sprite_commands_never_modify_the_document() {
    let mut s = session();
    let before = s.active().unwrap().doc.clone();
    for (cmd, p) in [
        ("sprite.setDuration", json!({"durationMs":65536})),
        ("sprite.setDuration", json!({"durationMs":17,"frame":u64::MAX})),
        ("sprite.setPaletteEntry", json!({"index":u64::MAX,"rgba":[0,0,0,0]})),
        ("sprite.setPaletteEntry", json!({"index":0,"rgba":[0,-1,0,0]})),
        ("sprite.setCel", json!({"layer":0,"x":u64::MAX})),
        ("sprite.setTags", json!({"tags":[{"name":"Bad","from":3,"to":0,"direction":9,"repeat":1,"color":[1,2,3]}]})),
    ] {
        assert!(s.execute(cmd, p).is_err(), "{cmd}");
        assert_eq!(s.active().unwrap().doc.as_ref(), before.as_ref());
    }
}

#[test]
fn reference_layers_cannot_be_unlocked_into_a_discarded_display_cache_edit() {
    let mut s = session();
    s.edit("Reference", |doc, _| {
        doc.sprite.as_mut().unwrap().layers[0].flags |= 64;
        doc.layers[0].locks.all = true;
        Ok(())
    })
    .unwrap();
    let before = s.active().unwrap().doc.clone();
    assert!(
        s.edit("Unlock", |doc, _| {
            doc.layers[0].locks.all = false;
            Ok(())
        })
        .is_err()
    );
    assert_eq!(s.active().unwrap().doc, before);
}

#[test]
fn tilemap_pixels_require_explicit_rasterization_while_other_edits_preserve_tile_records() {
    let doc = photocraft_io::import("tiles.aseprite", include_bytes!("../../../../io/tests/fixtures/aseprite/tiles.aseprite")).unwrap().document;
    let id = doc.layers[0].id;
    let mut s = Session::new();
    s.add_document(doc, None);
    let before = s.active().unwrap().doc.clone();
    let change = |doc: &mut Document, _: &mut Option<LayerId>| {
        doc.layer_mut(id).unwrap().surface_mut().unwrap().write_pixel(0, 0, &[0.0, 0.0, 0.0, 1.0]);
        Ok(())
    };
    assert!(s.edit("Paint", change).is_err());
    assert_eq!(s.active().unwrap().doc, before);
    s.execute("sprite.duplicateFrame", json!({})).unwrap();
    assert!(photocraft_io::export(&s.active().unwrap().doc, "aseprite", &Default::default()).is_ok());
    s.execute("timeline.setFrame", json!({"frame":0})).unwrap();
    s.execute("sprite.deleteFrame", json!({})).unwrap();
    assert!(photocraft_io::export(&s.active().unwrap().doc, "aseprite", &Default::default()).is_ok());
    s.execute("sprite.rasterizeTilemap", json!({"layer":id.0})).unwrap();
    s.edit("Paint", change).unwrap();
    assert!(s.active().unwrap().doc.sprite.as_ref().unwrap().tilemap_descriptor(id).is_none());
    let out = photocraft_io::export(&s.active().unwrap().doc, "aseprite", &Default::default()).unwrap();
    assert!(photocraft_io::import("pixels.aseprite", &out.bytes).unwrap().document.sprite.as_ref().unwrap().tilemap_descriptor(id).is_none());
}
