//! A saved `minecraft:features` (or not-yet-decorated strip edge) chunk copied
//! back onto a freshly generated proto. Biomes and structure starts come from
//! `generate_proto` again: they are a function of the seed. Blocks, block
//! entities, entities, ticks, post-processing and the decorated flag come
//! from the save, so a neighbour's decoration spill is still there.
use crate::{Error, resources::Resources};
use horizon_core::{
    position::BlockPos,
    ticks::{ProtoChunkTicks, SavedTick, TickPriority, TickTypeId},
};
use horizon_world::{
    chunk::{BlockState, PalettedContainer},
    chunk_status::ChunkStatus,
    fluid::FluidKind,
    heightmap::HeightmapType,
    nbt::Tag,
    registry::BlockStateId,
    worldgen::{generator::ProtoChunk, terrain::PreLightChunkStage},
};

pub fn apply(res: &Resources, chunk: &mut ProtoChunk, root: &Tag) -> Result<(), Error> {
    let x_pos = int(root.get("xPos")).ok_or("saved chunk missing xPos")?;
    let z_pos = int(root.get("zPos")).ok_or("saved chunk missing zPos")?;
    let y_pos = int(root.get("yPos")).unwrap_or(-4);
    let origin_x = x_pos * 16;
    let origin_z = z_pos * 16;
    let air = res
        .registry
        .default_state("minecraft:air")
        .map_or(BlockStateId(0), |s| s);

    let Tag::List(sections) = root.get("sections").ok_or("saved chunk missing sections")? else {
        return Err("sections is not a list".into());
    };
    for section in sections {
        let y = i32::from(byte(section.get("Y")).ok_or("section missing Y")?);
        let Some(states) = section.get("block_states") else {
            continue;
        };
        let palette = PalettedContainer::from_nbt(states, 4096, 4)?;
        for lz in 0..16 {
            for lx in 0..16 {
                for ly in 0..16 {
                    let index = ((ly << 8) | (lz << 4) | lx) as usize;
                    let id = match BlockState::from_nbt(palette.get(index)?) {
                        Ok(state) => res.registry.resolve(&state).unwrap_or(air),
                        Err(_) => air,
                    };
                    chunk
                        .terrain
                        .set_block_raw(origin_x + lx, y * 16 + ly, origin_z + lz, id)
                        .map_err(|e| format!("restore block: {e}"))?;
                }
            }
        }
    }

    chunk.block_entities.clear();
    if let Some(Tag::List(list)) = root.get("block_entities") {
        for tag in list {
            let Tag::Compound(nbt) = tag else {
                continue;
            };
            let at = [
                int(tag.get("x")).unwrap_or(0),
                int(tag.get("y")).unwrap_or(0),
                int(tag.get("z")).unwrap_or(0),
            ];
            chunk.block_entities.push((at, nbt.clone()));
        }
    }
    chunk.entities.clear();
    if let Some(Tag::List(list)) = root.get("entities") {
        for tag in list {
            if let Tag::Compound(nbt) = tag {
                chunk.entities.push(nbt.clone());
            }
        }
    }

    *chunk.terrain.block_ticks_mut() = saved_ticks(res, root.get("block_ticks"), true)?;
    *chunk.terrain.fluid_ticks_mut() = saved_ticks(res, root.get("fluid_ticks"), false)?;

    chunk.terrain.clear_postprocessing();
    if let Some(Tag::List(sections)) = root.get("PostProcessing") {
        for (si, section) in sections.iter().enumerate() {
            let Tag::List(marks) = section else {
                continue;
            };
            let base = (y_pos + si as i32) * 16;
            for mark in marks {
                let Tag::Short(packed) = mark else {
                    continue;
                };
                let packed = *packed as u16;
                chunk.terrain.mark_postprocessing(
                    i32::from(packed & 15),
                    base + i32::from((packed >> 4) & 15),
                    i32::from((packed >> 8) & 15),
                );
            }
        }
    }

    let decorated = matches!(
        root.get("Status").map(ChunkStatus::from_nbt),
        Some(Ok(ChunkStatus::Features))
    );
    let stage = if decorated {
        PreLightChunkStage::Features
    } else {
        PreLightChunkStage::Terrain
    };
    chunk.terrain.set_generation_stage(stage);
    let kinds: Vec<HeightmapType> = stage.heightmaps().to_vec();
    chunk
        .terrain
        .prime_heightmaps(&kinds)
        .map_err(|e| format!("restore heightmaps: {e}"))?;
    if decorated {
        chunk.set_decorated();
    }
    Ok(())
}

fn saved_ticks(res: &Resources, tag: Option<&Tag>, blocks: bool) -> Result<ProtoChunkTicks, Error> {
    let Some(Tag::List(list)) = tag else {
        return Ok(ProtoChunkTicks::default());
    };
    let mut ticks = Vec::new();
    for tick in list {
        let Some(name) = tick.get("i").and_then(|t| match t {
            Tag::String(s) => Some(s.to_string_lossy()),
            _ => None,
        }) else {
            continue;
        };
        let Some(kind) = (if blocks {
            res.registry.default_state(&name).map(|s| s.0)
        } else {
            FluidKind::ALL
                .iter()
                .position(|fluid| fluid.name() == name)
                .map(|i| i as u32)
        }) else {
            continue;
        };
        ticks.push(SavedTick {
            kind: TickTypeId(kind),
            pos: BlockPos {
                x: int(tick.get("x")).unwrap_or(0),
                y: int(tick.get("y")).unwrap_or(0),
                z: int(tick.get("z")).unwrap_or(0),
            },
            delay: int(tick.get("t")).unwrap_or(0),
            priority: TickPriority::by_value(int(tick.get("p")).unwrap_or(0)),
        });
    }
    Ok(ProtoChunkTicks::from_saved(ticks))
}

fn int(tag: Option<&Tag>) -> Option<i32> {
    match tag {
        Some(Tag::Int(v)) => Some(*v),
        Some(Tag::Byte(v)) => Some(i32::from(*v)),
        Some(Tag::Short(v)) => Some(i32::from(*v)),
        _ => None,
    }
}

fn byte(tag: Option<&Tag>) -> Option<i8> {
    match tag {
        Some(Tag::Byte(v)) => Some(*v),
        Some(Tag::Int(v)) => i8::try_from(*v).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uniform_palette_reads_back_as_that_block() {
        let tag = Tag::compound([(
            "palette",
            Tag::List(vec![Tag::String("minecraft:stone".into())]),
        )]);
        let palette = PalettedContainer::from_nbt(&tag, 4096, 4).unwrap();
        assert_eq!(
            palette.get(0).unwrap(),
            &Tag::String("minecraft:stone".into())
        );
        assert_eq!(
            palette.get(4095).unwrap(),
            &Tag::String("minecraft:stone".into())
        );
    }
}
