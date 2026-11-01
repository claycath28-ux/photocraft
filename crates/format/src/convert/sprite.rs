//! Native cel animation persistence. Pixels/index planes use the ordinary content-addressed
//! tile store; opaque metadata/profile payloads use bounded binary blobs, never JSON arrays.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use photocraft_doc::{
    LayerId,
    sprite::{MAX_CELS, MAX_FRAMES, MAX_METADATA, Sprite, SpriteCel, SpriteChunk, SpriteFrame},
};

use super::{FormatError, Loader, Result, Sink, surface_m};
use crate::manifest::{SpriteCelM, SpriteChunkM, SpriteFrameM, SpriteM};

pub(super) fn encode(sprite: &Sprite, sink: &mut dyn Sink) -> SpriteM {
    let mut palettes = Vec::new();
    let mut palette_ids = HashMap::new();
    let frames = sprite
        .frames
        .iter()
        .map(|frame| {
            let palette = *palette_ids.entry(frame.palette.clone()).or_insert_with(|| {
                let id = palettes.len();
                palettes.push(frame.palette.as_ref().clone());
                id
            });
            SpriteFrameM {
                duration_ms: frame.duration_ms,
                palette,
                cels: frame
                    .cels
                    .iter()
                    .map(|(layer, cel)| SpriteCelM {
                        layer: layer.0,
                        rect: cel.rect,
                        pixels: surface_m(&cel.pixels, sink),
                        indices: cel.indices.as_ref().map(|s| surface_m(s, sink)),
                        opacity: cel.opacity,
                        z_index: cel.z_index,
                        linked_frame: cel.linked_frame,
                        precise: cel.precise,
                    })
                    .collect(),
                metadata: frame.metadata.iter().map(|c| SpriteChunkM { kind: c.kind, target: c.target.clone(), blob: sink.blob(&c.bytes) }).collect(),
            }
        })
        .collect();
    SpriteM {
        current: sprite.current,
        layers: sprite.layers.clone(),
        indexed: sprite.indexed,
        transparent_index: sprite.transparent_index,
        pixel_aspect: sprite.pixel_aspect,
        grid: sprite.grid,
        tags: sprite.tags.clone(),
        profile: sprite.profile.as_ref().map(|p| sink.blob(p)),
        palettes,
        frames,
    }
}

impl Loader<'_> {
    pub(super) fn sprite(&mut self, m: &SpriteM) -> Result<Sprite> {
        let bad = |message: &str| FormatError::corrupt(message);
        if m.frames.is_empty()
            || m.frames.len() > MAX_FRAMES
            || m.current >= m.frames.len()
            || m.layers.len() > 4096
            || m.palettes.len() > MAX_FRAMES
            || m.palettes.iter().any(|p| p.len() > 256)
        {
            return Err(bad("invalid sprite frame/palette/layer count"));
        }
        let palettes: Vec<_> = m.palettes.iter().map(|p| Arc::new(p.clone())).collect();
        let mut count = 0usize;
        let mut metadata_bytes = 0usize;
        let mut frames = Vec::with_capacity(m.frames.len());
        for frame in &m.frames {
            count = count.checked_add(frame.cels.len()).ok_or_else(|| bad("sprite cel count overflow"))?;
            if count > MAX_CELS || frame.metadata.len() > MAX_CELS {
                return Err(bad("sprite cel/metadata count limit exceeded"));
            }
            let palette = palettes.get(frame.palette).ok_or_else(|| bad("sprite palette reference is missing"))?.clone();
            let mut cels = BTreeMap::new();
            for cel in &frame.cels {
                let id = LayerId(cel.layer);
                if cels.contains_key(&id) {
                    return Err(bad("duplicate sprite cel layer"));
                }
                cels.insert(
                    id,
                    Arc::new(SpriteCel {
                        rect: cel.rect,
                        pixels: self.surface(&cel.pixels)?,
                        indices: cel.indices.as_ref().map(|s| self.surface(s)).transpose()?,
                        opacity: cel.opacity,
                        z_index: cel.z_index,
                        linked_frame: cel.linked_frame,
                        precise: cel.precise,
                    }),
                );
            }
            let mut metadata = Vec::with_capacity(frame.metadata.len());
            for c in &frame.metadata {
                let remaining = MAX_METADATA.checked_sub(metadata_bytes).ok_or_else(|| bad("sprite metadata exceeds 32 MiB"))?;
                let bytes = self.fetch.blob_bounded(&c.blob, remaining)?;
                metadata_bytes = metadata_bytes.checked_add(bytes.len()).ok_or_else(|| bad("sprite metadata size overflow"))?;
                metadata.push(SpriteChunk { kind: c.kind, target: c.target.clone(), bytes });
            }
            frames.push(Arc::new(SpriteFrame { duration_ms: frame.duration_ms, palette, cels, metadata }));
        }
        let mut sprite = Sprite {
            frames,
            current: m.current,
            layers: m.layers.clone(),
            indexed: m.indexed,
            transparent_index: m.transparent_index,
            pixel_aspect: m.pixel_aspect,
            grid: m.grid,
            tags: m.tags.clone(),
            profile: m.profile.as_ref().map(|p| self.fetch.blob_bounded(p, (16 << 20) + 20)).transpose()?,
        };
        sprite.remap_layers(|id| self.id_map.get(&id.0).copied()).map_err(FormatError::corrupt)?;
        Ok(sprite)
    }
}
