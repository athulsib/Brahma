//! The prepared resources the generator runs on: the block registry from
//! `facts.json` and the `worldgen/` bundle, both built from the player's own
//! Minecraft install by `horizon-prepare` (never shipped with this tool).
use crate::Error;
use horizon_world::{
    DATA_VERSION,
    nbt::Tag,
    registry::{BlockRegistry, BlockStateId},
    worldgen::{
        generator::GeneratorData,
        overrides::{PackFile, WorldgenOverrides},
    },
};
use serde_json::Value;
use std::{fs, path::Path, sync::Arc};

/// Registry metadata JSON -> NBT, as the prepared `facts.json` stores states.
fn tag(v: &Value) -> Result<Tag, Error> {
    Ok(match v {
        Value::String(s) => Tag::String(s.as_str().into()),
        Value::Number(n) => Tag::Int(n.as_i64().ok_or("registry metadata number")? as i32),
        Value::Array(a) => Tag::List(a.iter().map(tag).collect::<Result<_, _>>()?),
        Value::Object(o) => Tag::Compound(
            o.iter()
                .map(|(k, v)| Ok((k.as_str().into(), tag(v)?)))
                .collect::<Result<_, Error>>()?,
        ),
        _ => return Err("invalid registry metadata".into()),
    })
}

/// The block registry of `facts.json`, leaked: the generator binds it for
/// the life of the process.
pub fn registry(resources: &Path) -> Result<&'static BlockRegistry, Error> {
    let path = resources.join("facts.json");
    let facts: Value = serde_json::from_str(
        &fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?,
    )?;
    let states = facts["states"].as_array().ok_or("facts.json: states")?;
    let registry = BlockRegistry::from_nbt(&Tag::compound([
        ("DataVersion", Tag::Int(DATA_VERSION)),
        (
            "states",
            Tag::List(
                states
                    .iter()
                    .map(|v| tag(&v[4]))
                    .collect::<Result<_, _>>()?,
            ),
        ),
        ("defaults", tag(&facts["defaults"])?),
    ]))?;
    Ok(Box::leak(Box::new(registry)))
}

/// Everything seed independent, loaded once.
pub struct Resources {
    pub registry: &'static BlockRegistry,
    pub data: Arc<GeneratorData>,
    /// Per state: whether it is its block's default state (vanilla saves
    /// those as the bare block name).
    pub default_state: Vec<bool>,
    /// Per state: the vanilla block entity type of its block, if it has one.
    pub block_entity: Vec<Option<&'static str>>,
}

/// Data packs under `dir` (each child with `pack.mcmeta`), lowest name first,
/// as `WorldgenOverrides`. Pack ids are `file/<folder>`.
pub fn datapacks(dir: &Path) -> Result<(WorldgenOverrides, Vec<String>), Error> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut packs: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("pack.mcmeta").is_file())
        .collect();
    packs.sort();
    let mut overrides = WorldgenOverrides::default();
    let mut ids = Vec::new();
    for pack in packs {
        let id = format!(
            "file/{}",
            pack.file_name().unwrap_or_default().to_string_lossy()
        );
        ids.push(id.clone());
        let mut files = Vec::new();
        walk(&pack, &pack.join("data"), &mut files);
        for file in files {
            let read = || fs::read_to_string(pack.join(&file)).ok();
            if let PackFile::Unreadable = overrides.add_file(&id, &file, read) {
                return Err(format!("{id}: {file}: unreadable").into());
            }
        }
    }
    for warning in overrides.finish(|_| true) {
        eprintln!("brahma: data pack worldgen: {warning}");
    }
    Ok((overrides, ids))
}

impl Resources {
    pub fn load(resources: &Path) -> Result<Self, Error> {
        Ok(Self::load_with(resources, None)?.0)
    }

    /// `packs` is a folder of data packs (`pack.mcmeta` in each child).
    pub fn load_with(resources: &Path, packs: Option<&Path>) -> Result<(Self, Vec<String>), Error> {
        let registry = registry(resources)?;
        let (overrides, ids) = match packs {
            Some(dir) => datapacks(dir)?,
            None => (WorldgenOverrides::default(), Vec::new()),
        };
        let data = GeneratorData::load_with(&resources.join("worldgen"), registry, &overrides)?;
        for warning in data.warnings() {
            eprintln!("brahma: worldgen bundle: {warning}");
        }
        let mut default_state = Vec::with_capacity(registry.len());
        let mut block_entity = Vec::with_capacity(registry.len());
        for id in 0..registry.len() {
            let state = registry
                .state(BlockStateId(id as u32))
                .ok_or("registry state")?;
            default_state
                .push(registry.default_state(&state.name) == Some(BlockStateId(id as u32)));
            block_entity.push(block_entity_type(&state.name));
        }
        Ok((
            Self {
                registry,
                data,
                default_state,
                block_entity,
            },
            ids,
        ))
    }
}

/// `WorldOptions.parseSeed`: an integer, or the text's Java `String.hashCode`.
pub fn parse_seed(text: &str) -> Option<i64> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    Some(t.parse::<i64>().unwrap_or_else(|_| {
        i64::from(
            t.encode_utf16()
                .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c))),
        )
    }))
}

/// The vanilla block entity type of a block (`BlockEntityType.validBlocks`),
/// the same table as Horizon's `lan::chunks::block_entity_type`.
pub fn block_entity_type(name: &str) -> Option<&'static str> {
    let n = name.strip_prefix("minecraft:")?;
    Some(match n {
        "chest" => "minecraft:chest",
        "trapped_chest" => "minecraft:trapped_chest",
        "ender_chest" => "minecraft:ender_chest",
        "barrel" => "minecraft:barrel",
        "furnace" => "minecraft:furnace",
        "blast_furnace" => "minecraft:blast_furnace",
        "smoker" => "minecraft:smoker",
        "hopper" => "minecraft:hopper",
        "dispenser" => "minecraft:dispenser",
        "dropper" => "minecraft:dropper",
        "brewing_stand" => "minecraft:brewing_stand",
        "beacon" => "minecraft:beacon",
        "bell" => "minecraft:bell",
        "campfire" | "soul_campfire" => "minecraft:campfire",
        "enchanting_table" => "minecraft:enchanting_table",
        "jukebox" => "minecraft:jukebox",
        "lectern" => "minecraft:lectern",
        "spawner" => "minecraft:mob_spawner",
        "comparator" => "minecraft:comparator",
        "daylight_detector" => "minecraft:daylight_detector",
        "conduit" => "minecraft:conduit",
        "decorated_pot" => "minecraft:decorated_pot",
        "moving_piston" => "minecraft:piston",
        "structure_block" => "minecraft:structure_block",
        "jigsaw" => "minecraft:jigsaw",
        "command_block" | "chain_command_block" | "repeating_command_block" => {
            "minecraft:command_block"
        }
        "end_portal" => "minecraft:end_portal",
        "end_gateway" => "minecraft:end_gateway",
        "sculk_sensor" => "minecraft:sculk_sensor",
        "calibrated_sculk_sensor" => "minecraft:calibrated_sculk_sensor",
        "sculk_catalyst" => "minecraft:sculk_catalyst",
        "sculk_shrieker" => "minecraft:sculk_shrieker",
        "crafter" => "minecraft:crafter",
        "potent_sulfur" => "minecraft:potent_sulfur",
        "chiseled_bookshelf" => "minecraft:chiseled_bookshelf",
        "trial_spawner" => "minecraft:trial_spawner",
        "vault" => "minecraft:vault",
        "beehive" | "bee_nest" => "minecraft:beehive",
        "suspicious_sand" | "suspicious_gravel" => "minecraft:brushable_block",
        "creaking_heart" => "minecraft:creaking_heart",
        "test_block" => "minecraft:test_block",
        "test_instance_block" => "minecraft:test_instance_block",
        n if n.ends_with("shulker_box") => "minecraft:shulker_box",
        n if n.ends_with("_wall_hanging_sign") || n.ends_with("_hanging_sign") => {
            "minecraft:hanging_sign"
        }
        n if n.ends_with("_sign") => "minecraft:sign",
        n if n.ends_with("_banner") => "minecraft:banner",
        n if n.ends_with("_head") || n.ends_with("_skull") => "minecraft:skull",
        n if n.ends_with("_shelf") => "minecraft:shelf",
        n if n.ends_with("copper_golem_statue") => "minecraft:copper_golem_statue",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_parse_as_the_game_does() {
        assert_eq!(parse_seed("12345"), Some(12345));
        assert_eq!(parse_seed(" -7 "), Some(-7));
        assert_eq!(parse_seed(""), None);
        // Java: "horizon".hashCode()
        let expected = "horizon"
            .bytes()
            .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)));
        assert_eq!(parse_seed("horizon"), Some(i64::from(expected)));
        // Out of i64 range: hashed like any text.
        assert!(parse_seed("99999999999999999999").is_some());
    }

    #[test]
    fn block_entity_types() {
        assert_eq!(
            block_entity_type("minecraft:chest"),
            Some("minecraft:chest")
        );
        assert_eq!(
            block_entity_type("minecraft:oak_sign"),
            Some("minecraft:sign")
        );
        assert_eq!(
            block_entity_type("minecraft:oak_wall_hanging_sign"),
            Some("minecraft:hanging_sign")
        );
        assert_eq!(block_entity_type("minecraft:stone"), None);
        assert_eq!(block_entity_type("stone"), None);
    }
}
