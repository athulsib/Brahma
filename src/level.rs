//! `level.dat` and the saved data of a fresh 26.3 world generated with the
//! `minecraft:normal` world preset, as the game writes them for a new world:
//! `data/minecraft/world_gen_settings.dat` (the seed and the dimensions),
//! `world_clocks.dat`, `weather.dat` and `game_rules.dat` (defaults).
use crate::Error;
use horizon_core::version::{DATA_VERSION, VANILLA_VERSION};
use horizon_world::{
    compression::Compression,
    nbt::{self, Limits, NamedTag, Tag},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn string(s: &str) -> Tag {
    Tag::String(s.into())
}

/// A gzip NBT file with an empty-named compound root (`NbtIo.writeCompressed`).
pub fn write_dat(path: &Path, tag: Tag) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let bytes = nbt::write_named(
        &NamedTag {
            name: "".into(),
            tag,
        },
        Limits::saved(),
    )
    .map_err(|e| e.to_string())?;
    fs::write(path, Compression::Gzip.encode(&bytes)?)?;
    Ok(())
}

/// `SavedDataStorage`: `{data: ..., DataVersion}` under `data/minecraft/`.
fn write_saved_data(world: &Path, name: &str, data: Tag) -> Result<(), Error> {
    write_dat(
        &world
            .join("data")
            .join("minecraft")
            .join(format!("{name}.dat")),
        Tag::compound([("data", data), ("DataVersion", Tag::Int(DATA_VERSION))]),
    )
}

/// How the overworld is built. Flat is rejected: it is not a noise generator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
    pub id: &'static str,
    /// `noise_settings` id stored on the overworld dimension.
    settings: &'static str,
    /// Multi-noise preset id, or a fixed biome for `single_biome_surface`.
    source: PresetSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresetSource {
    MultiNoise(&'static str),
    Fixed(&'static str),
}

/// `normal`, `large_biomes`, `amplified`, `single_biome_surface` (with or
/// without the `minecraft:` prefix).
pub fn parse_preset(name: &str) -> Result<Preset, Error> {
    let id = name.trim().trim_start_matches("minecraft:");
    let preset = |id: &'static str, settings: &'static str, source: PresetSource| Preset {
        id,
        settings,
        source,
    };
    Ok(match id {
        "normal" => preset(
            "minecraft:normal",
            "minecraft:overworld",
            PresetSource::MultiNoise("minecraft:overworld"),
        ),
        "large_biomes" => preset(
            "minecraft:large_biomes",
            "minecraft:large_biomes",
            PresetSource::MultiNoise("minecraft:overworld"),
        ),
        "amplified" => preset(
            "minecraft:amplified",
            "minecraft:amplified",
            PresetSource::MultiNoise("minecraft:overworld"),
        ),
        "single_biome_surface" => preset(
            "minecraft:single_biome_surface",
            "minecraft:overworld",
            PresetSource::Fixed("minecraft:plains"),
        ),
        "flat" => {
            return Err(
                "minecraft:flat is not a noise generator; this tool does not generate flat worlds"
                    .into(),
            );
        }
        other => return Err(format!("unknown preset {other:?}").into()),
    })
}

/// `overworld`, `the_nether`, `the_end`, or an id that already contains `:`.
/// A pack dimension is accepted only as a named id; nothing is inferred.
pub fn parse_dimension(name: &str) -> Result<String, Error> {
    let name = name.trim();
    let id = if name.contains(':') {
        name.to_string()
    } else if name.is_empty() {
        return Err("empty dimension".into());
    } else {
        format!("minecraft:{name}")
    };
    let (namespace, path) = id.split_once(':').unwrap();
    let ok = |part: &str| {
        !part.is_empty()
            && !part.contains("..")
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
    };
    if !ok(namespace) || !ok(path) {
        return Err(format!("invalid dimension id {id:?}").into());
    }
    Ok(id)
}

/// `dimensions/<namespace>/<path>` (26.3). Region files live in `region/` under that.
pub fn dimension_dir(world: &Path, id: &str) -> PathBuf {
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    world.join("dimensions").join(namespace).join(path)
}

/// `worldgen/world_preset/normal.json`: each dimension's noise settings and
/// biome source, as the game stores them in `world_gen_settings`. The
/// overworld follows `preset`; the nether and the end stay the vanilla ones.
fn world_gen_settings(seed: i64, preset: Preset, structures: bool) -> Tag {
    let dimension = |kind: &str, settings: &str, source: Tag| {
        Tag::compound([
            ("type", string(kind)),
            (
                "generator",
                Tag::compound([
                    ("type", string("minecraft:noise")),
                    ("settings", string(settings)),
                    ("biome_source", source),
                ]),
            ),
        ])
    };
    let multi = |p: &str| {
        Tag::compound([
            ("type", string("minecraft:multi_noise")),
            ("preset", string(p)),
        ])
    };
    let overworld_source = match preset.source {
        PresetSource::MultiNoise(name) => multi(name),
        PresetSource::Fixed(biome) => Tag::compound([
            ("type", string("minecraft:fixed")),
            ("biome", string(biome)),
        ]),
    };
    Tag::compound([
        ("seed", Tag::Long(seed)),
        (
            "generate_structures",
            Tag::Byte(if structures { 1 } else { 0 }),
        ),
        ("bonus_chest", Tag::Byte(0)),
        (
            "dimensions",
            Tag::compound([
                (
                    "minecraft:overworld",
                    dimension("minecraft:overworld", preset.settings, overworld_source),
                ),
                (
                    "minecraft:the_nether",
                    dimension(
                        "minecraft:the_nether",
                        "minecraft:nether",
                        multi("minecraft:nether"),
                    ),
                ),
                (
                    "minecraft:the_end",
                    dimension(
                        "minecraft:the_end",
                        "minecraft:end",
                        Tag::compound([("type", string("minecraft:the_end"))]),
                    ),
                ),
            ]),
        ),
    ])
}

/// The game rules of 26.3 with their defaults (`GameRules`); integer rules
/// are listed in `INT_RULES`, the others are booleans.
const GAME_RULES: [(&str, i32); 58] = [
    ("spawn_wandering_traders", 1),
    ("block_drops", 1),
    ("reduced_debug_info", 0),
    ("show_death_messages", 1),
    ("spawner_blocks_work", 1),
    ("spawn_monsters", 1),
    ("tnt_explodes", 1),
    ("player_movement_check", 1),
    ("immediate_respawn", 0),
    ("block_explosion_drop_decay", 1),
    ("spread_vines", 1),
    ("max_entity_cramming", 24),
    ("forgive_dead_players", 1),
    ("fall_damage", 1),
    ("send_command_feedback", 1),
    ("global_sound_events", 1),
    ("elytra_movement_check", 1),
    ("fire_spread_radius_around_player", 128),
    ("freeze_damage", 1),
    ("mob_explosion_drop_decay", 1),
    ("natural_health_regeneration", 1),
    ("players_nether_portal_default_delay", 80),
    ("mob_drops", 1),
    ("log_admin_commands", 1),
    ("mob_griefing", 1),
    ("spawn_mobs", 1),
    ("pvp", 1),
    ("spectators_generate_chunks", 1),
    ("max_command_sequence_length", 65536),
    ("players_nether_portal_creative_delay", 0),
    ("players_sleeping_percentage", 100),
    ("advance_weather", 1),
    ("max_block_modifications", 32768),
    ("max_command_forks", 65536),
    ("drowning_damage", 1),
    ("command_block_output", 1),
    ("locator_bar", 1),
    ("show_advancement_messages", 1),
    ("respawn_radius", 10),
    ("raids", 1),
    ("spawn_phantoms", 1),
    ("max_snow_accumulation_height", 1),
    ("limited_crafting", 0),
    ("allow_entering_nether_using_portals", 1),
    ("lava_source_conversion", 0),
    ("tnt_explosion_drop_decay", 0),
    ("universal_anger", 0),
    ("keep_inventory", 0),
    ("spawn_patrols", 1),
    ("random_tick_speed", 3),
    ("fire_damage", 1),
    ("entity_drops", 1),
    ("advance_time", 1),
    ("command_blocks_work", 1),
    ("spawn_wardens", 1),
    ("water_source_conversion", 1),
    ("projectiles_can_break_blocks", 1),
    ("ender_pearls_vanish_on_death", 1),
];
const INT_RULES: [&str; 11] = [
    "max_snow_accumulation_height",
    "random_tick_speed",
    "max_entity_cramming",
    "fire_spread_radius_around_player",
    "players_nether_portal_default_delay",
    "max_command_sequence_length",
    "players_nether_portal_creative_delay",
    "players_sleeping_percentage",
    "max_block_modifications",
    "max_command_forks",
    "respawn_radius",
];

pub struct LevelInfo<'a> {
    pub name: &'a str,
    pub seed: i64,
    /// The world spawn: the overworld column at its surface.
    pub spawn: [i32; 3],
    pub preset: Preset,
    pub structures: bool,
    /// Enabled pack ids after `vanilla` (`file/<folder>`).
    pub packs: &'a [String],
}

/// `exact` is the vanilla generator. `fast` is approximate terrain.
/// A sidecar written before this field existed is `exact`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Exact,
    Fast,
}

impl Quality {
    pub fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Fast => "fast",
        }
    }

    pub fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "exact" => Ok(Self::Exact),
            "fast" => Ok(Self::Fast),
            other => Err(format!("unknown quality {other}; exact or fast").into()),
        }
    }
}

/// Seed, preset, structures flag and terrain quality of a world this tool already wrote.
#[derive(Debug, PartialEq, Eq)]
pub struct SavedRun {
    pub seed: i64,
    pub preset: String,
    pub structures: bool,
    pub quality: Quality,
}

fn run_path(world: &Path) -> PathBuf {
    world.join("brahma.json")
}

fn existing_run_path(world: &Path) -> Option<PathBuf> {
    let current = run_path(world);
    if current.is_file() {
        return Some(current);
    }
    let legacy = world.join("horizon-worldgen.json");
    legacy.is_file().then_some(legacy)
}

pub fn write_run(world: &Path, run: &SavedRun) -> Result<(), Error> {
    fs::create_dir_all(world)?;
    let text = serde_json::json!({
        "seed": run.seed,
        "preset": run.preset,
        "structures": run.structures,
        "quality": run.quality.name(),
    });
    fs::write(
        run_path(world),
        serde_json::to_string_pretty(&text).map_err(|e| e.to_string())?,
    )?;
    Ok(())
}

/// `None` when `world` is not a world yet. A `level.dat` without this tool's
/// sidecar is refused: the preset and structures flag cannot be checked.
pub fn read_run(world: &Path) -> Result<Option<SavedRun>, Error> {
    let Some(path) = existing_run_path(world) else {
        if world.join("level.dat").is_file() {
            return Err(
                "world has level.dat but no brahma.json or horizon-worldgen.json; refusing to resume"
                    .into(),
            );
        }
        return Ok(None);
    };
    let label = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "brahma.json".into());
    let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
    let seed = value["seed"]
        .as_i64()
        .ok_or_else(|| format!("{label}: seed"))?;
    let preset = value["preset"]
        .as_str()
        .ok_or_else(|| format!("{label}: preset"))?
        .to_string();
    let structures = value["structures"]
        .as_bool()
        .ok_or_else(|| format!("{label}: structures"))?;
    // Worlds written before `--quality` have no field. Those runs were exact.
    let quality = match value.get("quality") {
        None => Quality::Exact,
        Some(quality) => {
            let text = quality
                .as_str()
                .ok_or_else(|| format!("{label}: quality"))?;
            Quality::parse(text).map_err(|e| format!("{label}: {e}"))?
        }
    };
    Ok(Some(SavedRun {
        seed,
        preset,
        structures,
        quality,
    }))
}

/// Write a new world's `level.dat` and saved data into `world`.
pub fn write(world: &Path, info: &LevelInfo<'_>) -> Result<(), Error> {
    fs::create_dir_all(world)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let data = Tag::compound([
        ("DataVersion", Tag::Int(DATA_VERSION)),
        (
            "Version",
            Tag::compound([
                ("Name", string(VANILLA_VERSION)),
                ("Id", Tag::Int(DATA_VERSION)),
                ("Snapshot", Tag::Byte(0)),
                ("Series", string("main")),
            ]),
        ),
        ("version_history", Tag::List(vec![Tag::Int(DATA_VERSION)])),
        ("version", Tag::Int(19133)),
        ("LevelName", string(info.name)),
        ("GameType", Tag::Int(0)),
        ("Time", Tag::Long(0)),
        ("LastPlayed", Tag::Long(now)),
        (
            "spawn",
            Tag::compound([
                ("pos", Tag::IntArray(info.spawn.to_vec())),
                ("dimension", string("minecraft:overworld")),
                ("yaw", Tag::Float(0f32.to_bits())),
                ("pitch", Tag::Float(0f32.to_bits())),
            ]),
        ),
        (
            "difficulty_settings",
            Tag::compound([
                ("difficulty", string("normal")),
                ("hardcore", Tag::Byte(0)),
                ("locked", Tag::Byte(0)),
            ]),
        ),
        // Not initialized: the game runs its own initial spawn search over
        // the generated chunks when it first opens the world, as for a world
        // it created itself.
        ("initialized", Tag::Byte(0)),
        ("allowCommands", Tag::Byte(0)),
        ("WasModded", Tag::Byte(0)),
        ("ServerBrands", Tag::List(vec![string("brahma")])),
        (
            "DataPacks",
            Tag::compound([
                (
                    "Enabled",
                    Tag::List(
                        std::iter::once(string("vanilla"))
                            .chain(info.packs.iter().map(|id| string(id)))
                            .collect(),
                    ),
                ),
                (
                    "Disabled",
                    Tag::List(
                        [
                            "minecart_improvements",
                            "redstone_experiments",
                            "trade_rebalance",
                        ]
                        .iter()
                        .map(|s| string(s))
                        .collect(),
                    ),
                ),
            ]),
        ),
    ]);
    write_saved_data(
        world,
        "world_gen_settings",
        world_gen_settings(info.seed, info.preset, info.structures),
    )?;
    write_saved_data(
        world,
        "world_clocks",
        Tag::compound([
            (
                "minecraft:overworld",
                Tag::compound([("total_ticks", Tag::Long(0))]),
            ),
            (
                "minecraft:the_end",
                Tag::compound([("total_ticks", Tag::Long(0))]),
            ),
        ]),
    )?;
    write_saved_data(
        world,
        "weather",
        Tag::compound([
            ("clear_weather_time", Tag::Int(0)),
            ("rain_time", Tag::Int(0)),
            ("thunder_time", Tag::Int(0)),
            ("raining", Tag::Byte(0)),
            ("thundering", Tag::Byte(0)),
        ]),
    )?;
    let mut rules = horizon_world::nbt::Compound::new();
    for &(name, default) in &GAME_RULES {
        let tag = if INT_RULES.contains(&name) {
            Tag::Int(default)
        } else {
            Tag::Byte(i8::from(default != 0))
        };
        let key = format!("minecraft:{name}");
        rules.insert(key.as_str().into(), tag);
    }
    write_saved_data(world, "game_rules", Tag::Compound(rules))?;
    write_dat(&world.join("level.dat"), Tag::compound([("Data", data)]))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_files_read_back() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-worldgen-level-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write(
            &dir,
            &LevelInfo {
                name: "Test",
                seed: -42,
                spawn: [8, 70, -24],
                preset: parse_preset("normal").unwrap(),
                structures: true,
                packs: &[],
            },
        )
        .unwrap();
        let read = |path: &Path| -> Tag {
            let bytes = Compression::Gzip
                .decode(&fs::read(path).unwrap(), 1 << 26)
                .unwrap();
            nbt::read_named(&bytes, Limits::default()).unwrap().0.tag
        };
        let level = read(&dir.join("level.dat"));
        let data = level.get("Data").unwrap();
        assert_eq!(data.get("LevelName"), Some(&string("Test")));
        assert_eq!(data.get("DataVersion"), Some(&Tag::Int(DATA_VERSION)));
        assert_eq!(
            data.get("spawn").unwrap().get("pos"),
            Some(&Tag::IntArray(vec![8, 70, -24]))
        );
        let settings = read(&dir.join("data/minecraft/world_gen_settings.dat"));
        assert_eq!(
            settings.get("data").unwrap().get("seed"),
            Some(&Tag::Long(-42))
        );
        assert_eq!(settings.get("DataVersion"), Some(&Tag::Int(DATA_VERSION)));
        let rules = read(&dir.join("data/minecraft/game_rules.dat"));
        assert_eq!(
            rules
                .get("data")
                .unwrap()
                .get("minecraft:random_tick_speed"),
            Some(&Tag::Int(3))
        );
        assert_eq!(
            rules.get("data").unwrap().get("minecraft:keep_inventory"),
            Some(&Tag::Byte(0))
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_sidecar_without_quality_resumes_as_exact() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-worldgen-run-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("horizon-worldgen.json"),
            r#"{"seed":1,"preset":"minecraft:normal","structures":true}"#,
        )
        .unwrap();
        let saved = read_run(&dir).unwrap().unwrap();
        assert_eq!(saved.quality, Quality::Exact);
        write_run(
            &dir,
            &SavedRun {
                seed: 1,
                preset: "minecraft:normal".into(),
                structures: true,
                quality: Quality::Fast,
            },
        )
        .unwrap();
        assert!(dir.join("brahma.json").is_file());
        assert_eq!(read_run(&dir).unwrap().unwrap().quality, Quality::Fast);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn flat_preset_is_rejected_and_dimensions_have_their_own_folders() {
        assert!(parse_preset("flat").is_err());
        assert!(parse_preset("minecraft:flat").is_err());
        assert_eq!(
            parse_preset("large_biomes").unwrap().id,
            "minecraft:large_biomes"
        );
        assert_eq!(
            dimension_dir(Path::new("world"), "minecraft:the_nether"),
            Path::new("world")
                .join("dimensions")
                .join("minecraft")
                .join("the_nether")
        );
        assert_eq!(parse_dimension("the_end").unwrap(), "minecraft:the_end");
        assert!(parse_dimension("../x").is_err());
    }
}
