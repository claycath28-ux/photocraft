//! Import-only, single-frame Aseprite documents (#3811). Clean-room implementation of the
//! public binary specification, not Aseprite application code:
//! <https://github.com/aseprite/aseprite/blob/main/docs/ase-file-specs.md>.
//!
//! RGBA/grayscale cels become editable layers. Unsupported rendering features are refused;
//! non-rendering metadata is warned about. No external references are followed.

mod reader;
#[cfg(test)]
mod tests;

use photocraft_color::{BlendMode, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Layer, LayerContent};
use photocraft_geom::{Rect, Size, TILE_SIZE};
use photocraft_raster::{Interrupt, Surface};

use crate::{ImportResult, IoError};
use reader::Reader;

type Result<T> = std::result::Result<T, IoError>;
const MAX_FILE: usize = 512 << 20;
const MAX_LAYERS: usize = 4096;
const MAX_CHUNKS: usize = 65_536;
/// Counts allocated tiles, including partially covered boundary tiles, across all layers.
const MAX_PIXELS: usize = 256 << 20;

fn invalid(message: impl Into<String>) -> IoError {
    IoError::Aseprite(message.into())
}

/// Recognize the document signature, independently of its extension. Adobe Swatch Exchange
/// starts with `ASEF`, not Aseprite's little-endian magic at offset four. Full validation follows.
pub fn is_aseprite(bytes: &[u8]) -> bool {
    let word = |at| bytes.get(at..at + 2).and_then(|b| <[u8; 2]>::try_from(b).ok()).map(u16::from_le_bytes);
    let size = bytes.get(..4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_le_bytes);
    bytes.len() >= 144
        && size.is_some_and(|n| n as u64 == bytes.len() as u64)
        && word(4) == Some(0xa5e0)
        && word(6).is_some_and(|n| n > 0)
        && word(8).is_some_and(|n| n > 0)
        && word(10).is_some_and(|n| n > 0)
        && matches!(word(12), Some(8 | 16 | 32))
        && word(132) == Some(0xf1fa)
}

struct Node {
    layer: Layer,
    parent: Option<usize>,
    cel: bool,
}

struct Sprite<'a> {
    nodes: Vec<Node>,
    parents: Vec<usize>,
    fmt: PixelFormat,
    flags: u32,
    budget: usize,
    profile: Option<std::sync::Arc<Vec<u8>>>,
    profile_seen: bool,
    warnings: crate::ora::Warnings,
    ctl: &'a Interrupt<'a>,
}

/// Read a supported still sprite, preserving pixels outside the canvas as layer data.
pub(crate) fn import(name: &str, bytes: &[u8], ctl: &Interrupt<'_>) -> Result<ImportResult> {
    if bytes.len() > MAX_FILE {
        return Err(invalid("file exceeds the 512 MiB limit"));
    }
    let mut r = Reader::new(bytes);
    let declared = r.u32()? as usize;
    if declared != bytes.len() || r.u16()? != 0xa5e0 {
        return Err(invalid("invalid file size or signature"));
    }
    if r.u16()? != 1 {
        return Err(invalid("only single-frame sprites are supported; export a still sprite from Aseprite"));
    }
    let (w, h, depth) = (r.u16()?, r.u16()?, r.u16()?);
    let mode = match depth {
        32 => ColorMode::Rgb,
        16 => ColorMode::Grayscale,
        8 => return Err(invalid("indexed sprites are not supported yet; convert a copy to RGB in Aseprite")),
        _ => return Err(invalid("unsupported pixel depth")),
    };
    if w == 0 || h == 0 {
        return Err(invalid("canvas dimensions must be positive"));
    }
    let flags = r.u32()?;
    if flags & !7 != 0 {
        return Err(invalid("unsupported header flags"));
    }
    r.take(16)?;
    let (pw, ph) = (r.u8()?, r.u8()?);
    if pw != 0 && ph != 0 && pw != ph {
        return Err(invalid("non-square sprite pixels are not supported yet"));
    }
    let grid = r.take(8)?.iter().any(|b| *b != 0);
    r.take(84)?;
    let frame_bytes = r.u32()? as usize;
    let frame_len = frame_bytes.checked_sub(16).ok_or_else(|| invalid("frame is shorter than its header"))?;
    if r.u16()? != 0xf1fa {
        return Err(invalid("invalid frame signature"));
    }
    let old_chunks = r.u16()?;
    r.take(4)?; // duration and reserved: one still frame has no playback clock.
    let new_chunks = r.u32()? as usize;
    let count = if new_chunks == 0 { usize::from(old_chunks) } else { new_chunks };
    let mut frame = Reader::new(r.take(frame_len)?);
    r.end()?;
    if count > MAX_CHUNKS || count > frame.remaining() / 6 {
        return Err(invalid("invalid or excessive chunk count"));
    }
    let mut sprite = Sprite {
        nodes: Vec::new(),
        parents: Vec::new(),
        fmt: PixelFormat::new(mode, SampleType::U8, true),
        flags,
        budget: MAX_PIXELS,
        profile: None,
        profile_seen: false,
        warnings: Default::default(),
        ctl,
    };
    if grid {
        sprite.warnings.note("Aseprite canvas grid settings are not retained");
    }
    for i in 0..count {
        ctl.check().map_err(|_| IoError::Cancelled)?;
        let size = (frame.u32()? as usize).checked_sub(6).ok_or_else(|| invalid("chunk is shorter than its header"))?;
        let kind = frame.u16()?;
        let mut chunk = Reader::new(frame.take(size)?);
        sprite.chunk(kind, &mut chunk)?;
        ctl.progress((i + 1) as f32 / count.max(1) as f32 * 0.9);
    }
    frame.end()?;
    if sprite.nodes.is_empty() {
        return Err(invalid("sprite has no layers"));
    }
    let mut doc = Document::new(name, Size::new(u32::from(w), u32::from(h)), mode, SampleType::U8);
    doc.icc_profile = sprite.profile;
    // Assemble bottom-to-top without recursion. Parents always precede children in the file.
    // Reversing each completed list restores the source's bottom-to-top order.
    while let Some(mut node) = sprite.nodes.pop() {
        if let LayerContent::Group(g) = &mut node.layer.content {
            g.children.reverse();
        }
        match node.parent {
            Some(i) => {
                let parent = sprite.nodes.get_mut(i).and_then(|p| p.layer.children_mut()).ok_or_else(|| invalid("invalid parent layer"))?;
                parent.push(node.layer);
            }
            None => doc.layers.push(node.layer),
        }
    }
    doc.layers.reverse();
    Ok(ImportResult { document: doc, warnings: sprite.warnings.finish(), source_read_only: true, preview_only: false })
}

impl Sprite<'_> {
    fn chunk(&mut self, kind: u16, r: &mut Reader<'_>) -> Result<()> {
        match kind {
            0x2004 => self.layer(r)?,
            0x2005 => self.cel(r)?,
            0x2007 => self.profile(r)?,
            0x2006 => return Err(invalid("precise/scaled reference cels are not supported")),
            0x2023 => return Err(invalid("tilesets and tilemap sprites are not supported yet")),
            _ => {
                // Unknown chunks can introduce rendering semantics. Only known metadata can
                // safely be omitted, and the import must say what was left out.
                let label = match kind {
                    0x0004 | 0x0011 | 0x2019 => "sprite palette",
                    0x2008 => "external file references (no external files were read)",
                    0x2016 => "saved masks",
                    0x2017 => "legacy paths",
                    0x2018 => "animation tags",
                    0x2020 => "user data",
                    0x2022 => "slices and pivots",
                    _ => return Err(invalid(format!("unsupported chunk type 0x{kind:04x}"))),
                };
                self.warnings.note(&format!("Aseprite {label} is not retained; save an edited copy as PSD or .pcraft"));
                r.take(r.remaining())?;
            }
        }
        r.end()
    }

    fn layer(&mut self, r: &mut Reader<'_>) -> Result<()> {
        if self.nodes.len() >= MAX_LAYERS {
            return Err(invalid("layer count exceeds 4096"));
        }
        let (flags, kind, level) = (r.u16()?, r.u16()?, usize::from(r.u16()?));
        if flags & !63 != 0 || kind > 1 {
            return Err(invalid("reference layers, tilemaps or unknown layer flags are not supported"));
        }
        if level > photocraft_doc::MAX_GROUP_DEPTH || level > self.parents.len() {
            return Err(invalid("invalid or excessive group nesting"));
        }
        self.parents.truncate(level);
        let parent = self.parents.last().copied();
        r.take(4)?;
        let blend = r.u16()?;
        let opacity = r.u8()?;
        r.take(3)?;
        let name = r.string()?;
        let mut layer = if kind == 1 { Layer::group(name, Vec::new()) } else { Layer::raster(name, self.fmt) };
        layer.visible = flags & 1 != 0;
        layer.locks.all = flags & 2 == 0;
        layer.locks.position = flags & (4 | 8) != 0;
        layer.locks.transparency = flags & 8 != 0;
        if kind == 0 || self.flags & 2 != 0 {
            layer.blend = blend_mode(blend)?;
            if self.flags & 1 != 0 {
                layer.opacity = f32::from(opacity) / 255.0;
            }
        }
        if let LayerContent::Group(g) = &mut layer.content {
            g.expanded = flags & 32 == 0;
            self.parents.push(self.nodes.len());
        }
        if self.flags & 4 != 0 {
            r.take(16)?;
            self.warnings.note("Aseprite layer UUIDs are not retained");
        }
        self.nodes.push(Node { layer, parent, cel: false });
        Ok(())
    }

    fn cel(&mut self, r: &mut Reader<'_>) -> Result<()> {
        let index = usize::from(r.u16()?);
        let (x, y, opacity, kind, z) = (i32::from(r.i16()?), i32::from(r.i16()?), r.u8()?, r.u16()?, r.i16()?);
        r.take(5)?;
        if !matches!(kind, 0 | 2) || z != 0 {
            return Err(invalid("linked/tilemap cels and per-cel z-order are not supported yet"));
        }
        let node = self.nodes.get_mut(index).ok_or_else(|| invalid("cel refers to a missing layer"))?;
        if node.cel || node.layer.is_group() {
            return Err(invalid("duplicate cel or cel on a group layer"));
        }
        let (w, h) = (u32::from(r.u16()?), u32::from(r.u16()?));
        if w == 0 || h == 0 {
            return Err(invalid("cel dimensions must be positive"));
        }
        let rect = Rect::from_xywh(x, y, w, h);
        let tiles = usize::try_from(rect.tile_count()).map_err(|_| invalid("cel tile count overflow"))?;
        let cost = tiles.checked_mul(TILE_SIZE as usize * TILE_SIZE as usize * self.fmt.bytes_per_pixel()).ok_or_else(|| invalid("cel size overflow"))?;
        self.budget = self.budget.checked_sub(cost).ok_or_else(|| invalid("decoded layers exceed the 256 MiB tile limit"))?;
        let mut surface = Surface::new(self.fmt);
        reader::pixels(r, kind == 2, rect, &mut surface, self.ctl)?;
        // Cel opacity is ordinary source opacity, not Photoshop Fill (which changes the
        // formula of e.g. Color Dodge). Combine the two opacities without changing samples.
        node.layer.opacity *= f32::from(opacity) / 255.0;
        node.layer.content = LayerContent::Raster(surface);
        node.cel = true;
        Ok(())
    }

    fn profile(&mut self, r: &mut Reader<'_>) -> Result<()> {
        if self.profile_seen {
            return Err(invalid("duplicate color profile"));
        }
        self.profile_seen = true;
        let (kind, flags, gamma) = (r.u16()?, r.u16()?, r.u32()?);
        r.take(8)?;
        if flags & !1 != 0 || (flags & 1 != 0 && gamma != 65536) {
            return Err(invalid("custom gamma profiles are not supported yet"));
        }
        let gray = self.fmt.mode == ColorMode::Grayscale;
        self.profile = match kind {
            0 if flags == 0 => None,
            1 => {
                use photocraft_cms::Builtin;
                let linear = flags & 1 != 0 && gamma == 65536;
                Some(match (gray, linear) {
                    (false, false) => Builtin::Srgb.profile().to_bytes(),
                    (false, true) => Builtin::LinearSrgb.profile().to_bytes(),
                    (true, false) => Builtin::SGray.profile().to_bytes(),
                    (true, true) => photocraft_cms::builtin::linear_gray().to_bytes(),
                })
            }
            2 if flags == 0 => {
                let len = r.u32()? as usize;
                if len > 16 << 20 {
                    return Err(invalid("ICC profile exceeds 16 MiB"));
                }
                let profile = photocraft_cms::Profile::parse(r.take(len)?).map_err(|e| invalid(format!("invalid ICC profile: {e}")))?;
                let expected = if gray { photocraft_cms::ColorSpace::Gray } else { photocraft_cms::ColorSpace::Rgb };
                if profile.color_space != expected {
                    return Err(invalid("ICC color space does not match the sprite"));
                }
                Some(profile.to_bytes())
            }
            _ => return Err(invalid("unsupported color profile")),
        };
        Ok(())
    }
}

fn blend_mode(mode: u16) -> Result<BlendMode> {
    use BlendMode as B;
    // IDs are specified by the format, not PhotoCraft's enum discriminants.
    const MODES: [B; 19] = [
        B::Normal,
        B::Multiply,
        B::Screen,
        B::Overlay,
        B::Darken,
        B::Lighten,
        B::ColorDodge,
        B::ColorBurn,
        B::HardLight,
        B::SoftLight,
        B::Difference,
        B::Exclusion,
        B::Hue,
        B::Saturation,
        B::Color,
        B::Luminosity,
        B::LinearDodge,
        B::Subtract,
        B::Divide,
    ];
    MODES.get(usize::from(mode)).copied().ok_or_else(|| invalid(format!("unsupported blend mode {mode}")))
}
