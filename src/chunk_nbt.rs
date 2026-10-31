//! A generated `ProtoChunk` as the chunk NBT vanilla's `SerializableChunkData`
//! writes for a proto chunk at status `minecraft:features`: sections (block
//! states and biomes), heightmaps, structure starts and references, pending
//! block entities, entities, scheduled ticks and post-processing marks.
//! The game lights and finalizes such a chunk on first load.
use crate::resources::Resources;
use horizon_core::{
    paletted::{PalettedContainer, Strategy},
    ticks::SavedTick,
};
use horizon_world::{
    DATA_VERSION,
    chunk_status::ChunkStatus,
    compression::Compression,
    fluid::FluidKind,
    nbt::{Compound, Limits, NamedTag, Tag},
    region::ChunkRecord,
    registry::BlockStateId,
    worldgen::generator::ProtoChunk,
};

fn string(s: &str) -> Tag {
    Tag::String(s.into())
}

fn ceil_log2(n: usize) -> u8 {
    if n <= 1 {
        0
    } else {
        (usize::BITS - (n - 1).leading_zeros()) as u8
    }
}

/// `PalettedContainer.pack` for the 4,096 blocks of a section: the palette
/// in first-occurrence order, default states as the bare block name
/// (`BlockState.CODEC`), the indices at vanilla's serialization width.
fn block_states(res: &Resources, states: &[u32; 4096]) -> Tag {
    let packed =
        PalettedContainer::from_values(Strategy::blocks(ceil_log2(res.registry.len())), states)
            .pack();
    let mut map = Compound::new();
    map.insert(
        "palette".into(),
        Tag::List(
            packed
                .palette
                .iter()
                .map(|&id| match res.registry.state(BlockStateId(id)) {
                    Some(s) if res.default_state.get(id as usize) == Some(&true) => string(&s.name),
                    Some(s) => s.to_nbt(),
                    None => string("minecraft:air"),
                })
                .collect(),
        ),
    );
    if let Some(data) = packed.data {
        map.insert(
            "data".into(),
            Tag::LongArray(data.into_iter().map(|w| w as i64).collect()),
        );
    }
    Tag::Compound(map)
}

/// The 64 quart biomes of a section, in vanilla's `(y << 4) | (z << 2) | x`
/// order (the order `BiomeChunk` stores them in).
fn biomes(chunk: &ProtoChunk, biome_count: usize, section: usize) -> Tag {
    let data = chunk.data();
    let values: Vec<u32> = match chunk.biomes().section(section) {
        Some(ids) => ids.iter().map(|b| b.0).collect(),
        None => {
            // Above what the proto holds: the top layer's biomes.
            let (_, top) = chunk.biomes().quart_range();
            let pos = chunk.position();
            (0..64)
                .map(|i| {
                    let (x, z) = (i & 3, (i >> 2) & 3);
                    chunk
                        .biomes()
                        .noise_biome([pos.x * 4 + x, top, pos.z * 4 + z])
                        .0
                })
                .collect()
        }
    };
    let packed =
        PalettedContainer::from_values(Strategy::biomes(ceil_log2(biome_count)), &values).pack();
    let mut map = Compound::new();
    map.insert(
        "palette".into(),
        Tag::List(
            packed
                .palette
                .iter()
                .map(|&id| string(data.biome_name(horizon_world::worldgen::biome::BiomeId(id))))
                .collect(),
        ),
    );
    if let Some(data) = packed.data {
        map.insert(
            "data".into(),
            Tag::LongArray(data.into_iter().map(|w| w as i64).collect()),
        );
    }
    Tag::Compound(map)
}

/// `SavedTick.saveTick`: `{i, x, y, z, t, p}`.
fn tick_list(ticks: &[SavedTick], name: impl Fn(u32) -> Option<String>) -> Tag {
    Tag::List(
        ticks
            .iter()
            .filter_map(|t| {
                Some(Tag::compound([
                    ("i", string(&name(t.kind.0)?)),
                    ("x", Tag::Int(t.pos.x)),
                    ("y", Tag::Int(t.pos.y)),
                    ("z", Tag::Int(t.pos.z)),
                    ("t", Tag::Int(t.delay)),
                    ("p", Tag::Int(t.priority as i32)),
                ]))
            })
            .collect(),
    )
}

/// The chunk's NBT at status `minecraft:features`.
pub fn encode(res: &Resources, chunk: &ProtoChunk) -> NamedTag {
    let terrain = &chunk.terrain;
    let pos = chunk.position();
    let (min_y, height) = (terrain.min_y(), terrain.height());
    let section_count = (height / 16) as usize;
    let biome_count = chunk.data().biome_names().len();
    let air = res
        .registry
        .default_state("minecraft:air")
        .map_or(0, |s| s.0);

    // Sections, bottom to top. An all-air section is written like vanilla's
    // (a single-entry palette), so the saved column is complete.
    let mut states = [0u32; 4096];
    let mut sections = Vec::with_capacity(section_count);
    for si in 0..section_count {
        let y = (min_y >> 4) + si as i32;
        match terrain.section(si) {
            Some(section) => {
                for (i, slot) in states.iter_mut().enumerate() {
                    *slot = section.get(i).map_or(air, |s| s.0);
                }
            }
            None => states.fill(air),
        }
        sections.push(Tag::compound([
            ("Y", Tag::Byte(y as i8)),
            ("block_states", block_states(res, &states)),
            ("biomes", biomes(chunk, biome_count, si)),
        ]));
    }

    // A decorated chunk is `minecraft:features`. A strip-edge chunk that has
    // only received a neighbour's spill is still `minecraft:terrain`, so a
    // later strip decorates it once.
    let status = if chunk.is_decorated() {
        ChunkStatus::Features
    } else {
        ChunkStatus::Terrain
    };
    // Heightmaps: those of the status the chunk has (`heightmapsAfter`).
    let heightmaps: Compound = status
        .heightmaps()
        .iter()
        .filter_map(|&kind| {
            terrain.heightmap(kind).map(|map| {
                (
                    kind.name().into(),
                    Tag::LongArray(map.raw_data().iter().map(|&w| w as i64).collect()),
                )
            })
        })
        .collect();

    // Structures: the starts this chunk owns and the starts reaching it.
    let mut starts = Compound::new();
    for start in chunk.own_starts() {
        match start.save() {
            Some(tag) => {
                starts.insert(start.name.as_str().into(), tag);
            }
            None => eprintln!(
                "brahma: chunk {} {}: structure start {} not saved: a piece of it cannot be",
                pos.x, pos.z, start.name
            ),
        }
    }
    let mut references: std::collections::BTreeMap<&str, Vec<i64>> = Default::default();
    for (name, start, _, _) in chunk.structures() {
        references.entry(name).or_default().push(start.pack());
    }
    let references: Compound = references
        .into_iter()
        .map(|(name, v)| (name.into(), Tag::LongArray(v)))
        .collect();

    // Pending block entities: the placed data with its type and position,
    // matching Horizon's proto save (`saved_proto`). Blocks that have a
    // block entity but no placed data get vanilla's defaults when the game
    // finishes the chunk; the generation oracle treats a missing entry as
    // those defaults.
    let mut block_entities: Vec<Tag> = Vec::new();
    for (at, nbt) in &chunk.block_entities {
        let mut nbt = nbt.clone();
        if let Some(id) = res
            .block_entity
            .get(chunk.block(at[0], at[1], at[2]).0 as usize)
            .copied()
            .flatten()
        {
            nbt.insert("id".into(), string(id));
        }
        for (key, v) in ["x", "y", "z"].into_iter().zip(at) {
            nbt.insert(key.into(), Tag::Int(*v));
        }
        block_entities.push(Tag::Compound(nbt));
    }

    // Scheduled ticks: block ticks are keyed by the block's default state id,
    // fluid ticks by `FluidKind`.
    let block_ticks = tick_list(terrain.block_ticks().pack(0), |kind| {
        res.registry
            .state(BlockStateId(kind))
            .map(|s| s.name.clone())
    });
    let fluid_ticks = tick_list(terrain.fluid_ticks().pack(0), |kind| {
        let fluid = *FluidKind::ALL.get(kind as usize)?;
        (fluid != FluidKind::Empty).then(|| fluid.name().to_owned())
    });

    // Post-processing marks: one list of packed `x | y << 4 | z << 8` per section.
    let mut post: Vec<Tag> = terrain
        .postprocessing()
        .iter()
        .map(|marks| Tag::List(marks.iter().map(|&p| Tag::Short(p as i16)).collect()))
        .collect();
    post.resize(section_count, Tag::List(Vec::new()));

    let root = Tag::compound([
        ("DataVersion", Tag::Int(DATA_VERSION)),
        ("xPos", Tag::Int(pos.x)),
        ("zPos", Tag::Int(pos.z)),
        ("yPos", Tag::Int(min_y >> 4)),
        ("Status", string(status.name())),
        ("LastUpdate", Tag::Long(0)),
        ("InhabitedTime", Tag::Long(0)),
        ("isLightOn", Tag::Byte(0)),
        ("sections", Tag::List(sections)),
        ("block_entities", Tag::List(block_entities)),
        (
            "entities",
            Tag::List(chunk.entities.iter().cloned().map(Tag::Compound).collect()),
        ),
        ("Heightmaps", Tag::Compound(heightmaps)),
        (
            "structures",
            Tag::compound([
                ("starts", Tag::Compound(starts)),
                ("References", Tag::Compound(references)),
            ]),
        ),
        ("block_ticks", block_ticks),
        ("fluid_ticks", fluid_ticks),
        ("PostProcessing", Tag::List(post)),
    ]);
    NamedTag {
        name: "".into(),
        tag: root,
    }
}

/// The chunk as a region record stamped `timestamp` (seconds since the epoch).
/// `compression` is the on-disk encoding; the chunk NBT does not change.
pub fn record(
    res: &Resources,
    chunk: &ProtoChunk,
    timestamp: u32,
    compression: Compression,
) -> std::io::Result<ChunkRecord> {
    ChunkRecord::from_nbt(&encode(res, chunk), compression, timestamp, Limits::saved())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceil_log2_matches_mth() {
        assert_eq!(ceil_log2(0), 0);
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(4), 2);
        assert_eq!(ceil_log2(5), 3);
        assert_eq!(ceil_log2(64), 6);
        assert_eq!(ceil_log2(65), 7);
    }
}
