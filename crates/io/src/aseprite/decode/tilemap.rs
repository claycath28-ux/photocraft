//! Bounded embedded tilesets and 32-bit tile maps. Original records are retained separately
//! from the raster display: editing another layer must never replace a tile map with pixels.

use super::{ColorMode, Decoder, IoError, PixelFormat, Reader, Rect, Result, SampleType, Surface, invalid};
use photocraft_doc::sprite::{self, MetadataTarget, SpriteChunk};
use std::{io::Read, sync::Arc};

pub(in crate::aseprite) struct Tileset {
    pub id: u32,
    pub width: u16,
    pub height: u16,
    pub count: u32,
    pub flags: u32,
    pub pixels: Option<Vec<u8>>,
}

pub(in crate::aseprite) fn inflate(data: &[u8], expected: usize) -> Result<Vec<u8>> {
    if expected > sprite::MAX_BYTES {
        return Err(invalid("tile data exceeds 256 MiB"));
    }
    let mut z = flate2::bufread::ZlibDecoder::new(data);
    let mut pixels = Vec::new();
    (&mut z).take(expected as u64 + 1).read_to_end(&mut pixels).map_err(|e| invalid(format!("invalid tile data: {e}")))?;
    if pixels.len() != expected || z.total_in() != data.len() as u64 {
        return Err(invalid("tile data length/checksum/trailing bytes are invalid"));
    }
    Ok(pixels)
}

pub(in crate::aseprite) fn tileset(data: &[u8], bpp: usize) -> Result<Tileset> {
    let mut r = Reader::new(data);
    let id = r.u32()?;
    let flags = r.u32()?;
    let count = r.u32()?;
    let (width, height) = (r.u16()?, r.u16()?);
    r.take(16)?;
    r.string()?;
    if flags & !63 != 0 || count == 0 || width == 0 || height == 0 {
        return Err(invalid("invalid tileset flags/count/dimensions"));
    }
    if flags & 1 != 0 {
        r.take(8)?;
    }
    let expected = usize::from(width)
        .checked_mul(usize::from(height))
        .and_then(|n| n.checked_mul(count as usize))
        .and_then(|n| n.checked_mul(bpp))
        .ok_or_else(|| invalid("tileset size overflow"))?;
    if expected > sprite::MAX_BYTES {
        return Err(invalid("tileset image exceeds 256 MiB"));
    }
    let pixels = if flags & 2 != 0 {
        let len = r.u32()? as usize;
        Some(inflate(r.take(len)?, expected)?)
    } else {
        None
    };
    r.end()?;
    Ok(Tileset { id, width, height, count, flags, pixels })
}

/// The stable tileset index named by the layer and the tileset record, not its order of
/// appearance. External-file ids are a different namespace.
pub(in crate::aseprite) fn layer_tileset(data: &[u8]) -> Result<usize> {
    let mut r = Reader::new(data);
    r.u16()?;
    if r.u16()? != 2 {
        return Err(invalid("retained tilemap layer has the wrong type"));
    }
    r.take(12)?;
    r.string()?;
    let index = r.u32()? as usize;
    if !matches!(r.remaining(), 0 | 16) {
        return Err(invalid("invalid tilemap layer record"));
    }
    Ok(index)
}

pub(in crate::aseprite) struct Map {
    pub width: u16,
    pub height: u16,
    pub masks: [u32; 4],
    pub tiles: Vec<u8>,
}
pub(in crate::aseprite) fn map(data: &[u8]) -> Result<Map> {
    let mut r = Reader::new(data);
    r.take(7)?;
    if r.u16()? != 3 {
        return Err(invalid("retained tilemap cel has the wrong type"));
    }
    r.take(7)?;
    let (width, height, bits) = (r.u16()?, r.u16()?, r.u16()?);
    if bits != 32 {
        return Err(invalid("only documented 32-bit tile ids are supported"));
    }
    let masks = [r.u32()?, r.u32()?, r.u32()?, r.u32()?];
    r.take(10)?;
    let id = *masks.first().ok_or_else(|| invalid("tile id mask is missing"))?;
    if id == 0 || !((id >> id.trailing_zeros()) as u64 + 1).is_power_of_two() {
        return Err(invalid("invalid tile id mask"));
    }
    let mut used = id;
    for m in masks.iter().skip(1) {
        if *m != 0 && (!m.is_power_of_two() || used & m != 0) {
            return Err(invalid("overlapping or invalid tile transform masks"));
        }
        used |= m;
    }
    let expected = usize::from(width).checked_mul(usize::from(height)).and_then(|n| n.checked_mul(4)).ok_or_else(|| invalid("tile map size overflow"))?;
    let tiles = inflate(r.take(r.remaining())?, expected)?;
    Ok(Map { width, height, masks, tiles })
}

pub(in crate::aseprite) fn render(map: &Map, tileset: &Tileset, at: (i32, i32), fmt: PixelFormat, transparent: Option<u8>) -> Result<(Rect, Surface)> {
    let data =
        tileset.pixels.as_ref().ok_or_else(|| invalid("tilemap needs an external-only tileset; embed it in Aseprite first (no external files were read)"))?;
    let (tw, th) = (u32::from(tileset.width), u32::from(tileset.height));
    let w = u32::from(map.width).checked_mul(tw).ok_or_else(|| invalid("tilemap width overflow"))?;
    let h = u32::from(map.height).checked_mul(th).ok_or_else(|| invalid("tilemap height overflow"))?;
    let rect = Rect::from_xywh(at.0, at.1, w, h);
    sprite::pixels::checked_rect(rect, if fmt.mode == ColorMode::Grayscale && !fmt.alpha { 5 } else { fmt.bytes_per_pixel() }).map_err(invalid)?;
    let mut out = if let Some(index) = transparent { Surface::with_default(fmt, &[f32::from(index) / 255.0]) } else { Surface::new(fmt) };
    let bpp = fmt.bytes_per_pixel();
    if rect.is_empty() {
        return Ok((rect, out));
    }
    let [id_mask, x_mask, y_mask, d_mask] = map.masks;
    for (i, word) in map.tiles.chunks_exact(4).enumerate() {
        let value = u32::from_le_bytes(word.try_into().map_err(|_| invalid("truncated tile id"))?);
        let id = (value & id_mask) >> id_mask.trailing_zeros();
        if tileset.flags & 4 != 0 && id == 0 || tileset.flags & 4 == 0 && value == u32::MAX {
            continue;
        }
        if id >= tileset.count {
            return Err(invalid("tile id exceeds the tileset image"));
        }
        let (flip_x, flip_y, diagonal) = (value & x_mask != 0, value & y_mask != 0, value & d_mask != 0);
        if diagonal && tw != th {
            return Err(invalid("diagonal transforms require square tiles"));
        }
        let (cx, cy) = (i % usize::from(map.width), i / usize::from(map.width));
        for dy in 0..th {
            let mut row = Vec::with_capacity(tw as usize * bpp);
            for dx in 0..tw {
                let (mut sx, mut sy) = (dx, dy);
                if flip_x {
                    sx = tw - 1 - sx
                }
                if flip_y {
                    sy = th - 1 - sy
                }
                if diagonal {
                    std::mem::swap(&mut sx, &mut sy)
                }
                let offset = (((id as usize * th as usize + sy as usize) * tw as usize) + sx as usize)
                    .checked_mul(bpp)
                    .ok_or_else(|| invalid("tile pixel offset overflow"))?;
                row.extend_from_slice(data.get(offset..offset + bpp).ok_or_else(|| invalid("tileset pixel is missing"))?);
            }
            let x = at.0 + cx as i32 * tw as i32;
            let y = at.1 + cy as i32 * th as i32 + dy as i32;
            out.try_write_interleaved(Rect::from_xywh(x, y, tw, 1), &row).map_err(|e| invalid(e.to_string()))?;
        }
    }
    Ok((rect, out))
}

impl Decoder<'_> {
    pub(super) fn retain_tile_record(&mut self, kind: u16, data: &[u8], target: MetadataTarget) -> Result<()> {
        self.metadata_bytes = self.metadata_bytes.checked_add(data.len()).ok_or_else(|| invalid("tile metadata size overflow"))?;
        if self.metadata_bytes > sprite::MAX_METADATA {
            return Err(invalid("retained tile records exceed 32 MiB"));
        }
        self.frame.metadata.push(SpriteChunk { kind, target, bytes: Arc::new(data.to_vec()) });
        Ok(())
    }
    pub(super) fn render_tilemaps(&mut self) -> Result<()> {
        let mut sets = std::collections::BTreeMap::new();
        let source_bpp = if self.indexed { 1 } else { self.fmt.bytes_per_pixel() };
        let mut decoded = 0usize;
        for frame in &self.frames {
            for c in &frame.metadata {
                if c.kind == 0x2023 {
                    let set = tileset(&c.bytes, source_bpp)?;
                    decoded = decoded.checked_add(set.pixels.as_ref().map_or(0, Vec::len)).ok_or_else(|| invalid("tileset size overflow"))?;
                    if decoded > self.budget {
                        return Err(invalid("tilesets exceed the remaining decoded memory budget"));
                    }
                    if sets.insert(set.id as usize, set).is_some() {
                        return Err(invalid("duplicate tileset index"));
                    }
                }
            }
        }
        self.budget = self.budget.checked_sub(decoded).ok_or_else(|| invalid("tileset budget exceeded"))?;
        let descriptors = self.frames.first().map(|f| f.metadata.iter().filter(|c| c.kind == 0x2004).cloned().collect::<Vec<_>>()).unwrap_or_default();
        for frame_index in 0..self.frames.len() {
            self.ctl.check().map_err(|_| IoError::Cancelled)?;
            let records = self.frames.get(frame_index).map(|f| f.metadata.iter().filter(|c| c.kind == 0x2005).cloned().collect::<Vec<_>>()).unwrap_or_default();
            for record in records {
                let MetadataTarget::Cel(id) = record.target else { return Err(invalid("tile cel attachment is invalid")) };
                let descriptor = descriptors.iter().find(|c| c.target == MetadataTarget::Layer(id)).ok_or_else(|| invalid("tile cel has no tilemap layer"))?;
                let set = sets.get(&layer_tileset(&descriptor.bytes)?).ok_or_else(|| invalid("tilemap layer refers to a missing tileset"))?;
                let parsed = map(&record.bytes)?;
                let frame = self.frames.get(frame_index).ok_or_else(|| invalid("tilemap frame is missing"))?;
                let cel = frame.cels.get(&id).ok_or_else(|| invalid("tilemap cel is missing"))?.clone();
                let palette = frame.palette.clone();
                let projected =
                    Rect::from_xywh(cel.rect.x0, cel.rect.y0, u32::from(parsed.width) * u32::from(set.width), u32::from(parsed.height) * u32::from(set.height));
                self.charge(projected, if self.indexed { 5 } else { self.fmt.bytes_per_pixel() })?;
                let fmt = if self.indexed { PixelFormat::new(ColorMode::Grayscale, SampleType::U8, false) } else { self.fmt };
                let (rect, pixels) = render(&parsed, set, (cel.rect.x0, cel.rect.y0), fmt, self.indexed.then_some(self.transparent))?;
                let display = if self.indexed {
                    sprite::pixels::render_indices(&pixels, rect, &palette, Some(self.transparent)).map_err(invalid)?
                } else {
                    pixels.clone()
                };
                let mut replacement = cel.as_ref().clone();
                replacement.rect = rect;
                replacement.pixels = display;
                replacement.indices = self.indexed.then_some(pixels);
                Arc::make_mut(self.frames.get_mut(frame_index).ok_or_else(|| invalid("tilemap frame is missing"))?).cels.insert(id, Arc::new(replacement));
            }
        }
        Ok(())
    }
}
