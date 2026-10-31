//! `brahma`: a standalone Minecraft: Java Edition 26.3 world
//! generator and chunk pregenerator built on Horizon's vanilla-exact
//! generator (`horizon-world`). It writes a world folder the game opens:
//! `level.dat`, the saved data under `data/minecraft/` and `region/*.mca`
//! with every chunk at status `minecraft:features` (terrain, carvers,
//! structures and decoration done; the game lights and finalizes each chunk
//! on first load).
mod area;
mod cache;
mod chunk_nbt;
mod dump;
mod fast_terrain;
mod level;
mod menu;
mod pipeline;
mod resources;
mod restore;

use horizon_world::compression::Compression;
use std::{path::PathBuf, process::ExitCode};

type Error = Box<dyn std::error::Error + Send + Sync>;

const HELP: &str = "\
brahma: Minecraft: Java Edition 26.3 world generator and pregenerator

USAGE
  brahma
        Menu. With no terminal, this help is printed.
  brahma generate --out <world dir> --seed <i64|text>
        [--resources <dir>] [--minecraft-dir <dir>] [--server-jar <jar>]
        [--java-home <dir>] [--name <level name>] [--radius <blocks>]
        [--shape square|circle] [--center <x> <z>] [--threads <n>]
        [--strip-width <chunks>] [--compression zlib|lz4|gzip|none]
        [--quality exact|fast] [--gpu auto|on|off]
        [--dimension <id>]... [--all-dimensions] [--preset <id>]
        [--no-structures] [--datapacks <folder>] [--order-file <path>] [--verbose]
        [--debug]
  brahma dump [--resources <dir>] [--dimension <id>] <world dir> <x0,z0,x1,z1>...
  brahma --help

Chunks are written at status minecraft:features under
dimensions/<namespace>/<path>/region. The game lights them on first load.

generate
  --resources     Prepared resources (facts.json and worldgen/). When omitted,
                  the first run builds that folder in the user cache from your
                  own Minecraft jars (Java, and the official launcher directory
                  or --minecraft-dir / --server-jar). Nothing of Mojang's is
                  written into this program's sources.
  --minecraft-dir Official launcher game directory, used when --resources is omitted.
  --server-jar    Pinned server jar, used when --resources is omitted.
  --java-home     JDK/runtime to use for that first-run prepare.
  --out           World folder. An existing world this tool wrote is resumed:
                  chunks already in their region are not rewritten. The seed,
                  preset, structures flag and quality must match. A world written
                  before --quality is exact. A world written by horizon-worldgen
                  (horizon-worldgen.json) is resumed the same way.
  --seed          Integer, or text hashed as the game does (WorldOptions.parseSeed).
  --name          Level name (default: the folder name). Used for a new world.
  --radius        Radius in blocks (default 2500). The area is the chunks within
                  radius/16 of the centre chunk, in that dimension.
  --shape         square (default) or circle, as the game's /pregen.
  --center        Centre block position (default: that dimension's spawn column).
  --threads       Worker threads (default: the available parallelism).
  --strip-width   Columns of one memory band (default 256). Wider holds more
                  chunks and recomputes less of the join between bands. A width
                  that covers the area is a single band.
  --compression   How region chunks are encoded: zlib (default), lz4, gzip, or
                  none. The chunk data is the same. lz4 and none write faster
                  and use more disk; 26.3 reads all four.
  --quality       exact (default) or fast. exact is the vanilla generator.
                  fast skips the density tree: approximate hills and caves,
                  the real biome source, sea level, and the same decoration
                  (ores, trees, plants). It does not place structures, and the
                  hills do not match the seed. Chunks are still minecraft:features.
  --gpu           auto (default), on, or off. Used only with --quality fast.
                  auto uses a GPU when one is available and the CPU otherwise.
                  on is an error when no device is found. The GPU only builds
                  the height and cave grids; the CPU fills blocks. The GPU
                  path uses Vulkan; without a Vulkan device, auto uses the CPU.
  --dimension     overworld, the_nether, the_end, or a namespaced id. Repeatable.
                  Default: overworld. A data-pack dimension is generated only
                  when named here.
  --all-dimensions  overworld, the_nether and the_end.
  --preset        minecraft:normal (default), large_biomes, amplified, or
                  single_biome_surface. minecraft:flat is rejected.
  --no-structures Do not place structures.
  --datapacks     Folder of data packs (each child has pack.mcmeta). Their
                  worldgen overrides the vanilla bundles, and their ids are
                  written into level.dat DataPacks.
  --order-file    Decoration order (x z per line) for parity checks.
  --verbose       List features the generator could not place.
  --debug         Print the per-row sweep log. Without it, one updating line
                  shows percent, chunks done, the stage (generating,
                  decorating, saving), the current chunks/s, and ETA.
                  p1 p25 p50 p75 p90 p99 of those rates are printed when the
                  dimension finishes.

dump
  Prints saved chunks of the rectangles (chunk coordinates, inclusive) in the
  text format of Horizon's VanillaChunkGenerationOracle.
  --dimension   Which dimension's region folder to read (default overworld).
  --resources   Prepared resources, or HORIZON_RESOURCES, or the user cache.
";

pub(crate) struct GenerateArgs {
    pub(crate) resources: Option<PathBuf>,
    pub(crate) minecraft_dir: Option<PathBuf>,
    pub(crate) server_jar: Option<PathBuf>,
    pub(crate) java_home: Option<PathBuf>,
    pub(crate) out: PathBuf,
    pub(crate) seed: i64,
    pub(crate) name: Option<String>,
    pub(crate) radius: i32,
    pub(crate) shape: area::Shape,
    pub(crate) center: Option<(i32, i32)>,
    pub(crate) threads: usize,
    pub(crate) strip_width: i32,
    pub(crate) compression: Compression,
    pub(crate) quality: level::Quality,
    pub(crate) gpu: fast_terrain::GpuChoice,
    pub(crate) order_file: Option<PathBuf>,
    pub(crate) verbose: bool,
    pub(crate) debug: bool,
    /// Dimension names as given, before `parse_dimension`.
    pub(crate) dimensions: Vec<String>,
    pub(crate) all_dimensions: bool,
    pub(crate) preset: String,
    pub(crate) structures: bool,
    pub(crate) datapacks: Option<PathBuf>,
}

fn parse_generate(args: &[String]) -> Result<GenerateArgs, Error> {
    let mut resources = None;
    let mut minecraft_dir = None;
    let mut server_jar = None;
    let mut java_home = None;
    let mut out = None;
    let mut seed = None;
    let mut name = None;
    let mut radius = 2500;
    let mut shape = area::Shape::Square;
    let mut center = None;
    let mut threads = std::thread::available_parallelism().map_or(2, |n| n.get());
    let mut strip_width = area::STRIP_WIDTH;
    let mut compression = Compression::Zlib;
    let mut quality = level::Quality::Exact;
    let mut gpu = fast_terrain::GpuChoice::Auto;
    let mut order_file = None;
    let mut verbose = false;
    let mut debug = false;
    let mut dimensions = Vec::new();
    let mut all_dimensions = false;
    let mut preset = "minecraft:normal".to_string();
    let mut structures = true;
    let mut datapacks = None;
    let mut i = 0;
    let value = |i: &mut usize, flag: &str| -> Result<String, Error> {
        *i += 1;
        args.get(*i)
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value").into())
    };
    while i < args.len() {
        match args[i].as_str() {
            "--resources" => resources = Some(PathBuf::from(value(&mut i, "--resources")?)),
            "--minecraft-dir" => {
                minecraft_dir = Some(PathBuf::from(value(&mut i, "--minecraft-dir")?))
            }
            "--server-jar" => server_jar = Some(PathBuf::from(value(&mut i, "--server-jar")?)),
            "--java-home" => java_home = Some(PathBuf::from(value(&mut i, "--java-home")?)),
            "--out" => out = Some(PathBuf::from(value(&mut i, "--out")?)),
            "--dimension" => dimensions.push(value(&mut i, "--dimension")?),
            "--all-dimensions" => all_dimensions = true,
            "--preset" => preset = value(&mut i, "--preset")?,
            "--no-structures" => structures = false,
            "--datapacks" => datapacks = Some(PathBuf::from(value(&mut i, "--datapacks")?)),
            "--seed" => {
                let text = value(&mut i, "--seed")?;
                seed = Some(
                    resources::parse_seed(&text).ok_or_else(|| format!("empty seed {text:?}"))?,
                );
            }
            "--name" => name = Some(value(&mut i, "--name")?),
            "--radius" => {
                radius = value(&mut i, "--radius")?.parse()?;
                if radius < 0 {
                    return Err("--radius must not be negative".into());
                }
            }
            "--shape" => {
                shape = match value(&mut i, "--shape")?.as_str() {
                    "square" => area::Shape::Square,
                    "circle" => area::Shape::Circle,
                    other => return Err(format!("unknown shape {other}; square or circle").into()),
                }
            }
            "--center" => {
                let x = value(&mut i, "--center")?.parse()?;
                let z = value(&mut i, "--center")?.parse()?;
                center = Some((x, z));
            }
            "--threads" => {
                threads = value(&mut i, "--threads")?.parse()?;
                if threads == 0 {
                    return Err("--threads must be at least 1".into());
                }
            }
            "--strip-width" => {
                strip_width = value(&mut i, "--strip-width")?.parse()?;
                if strip_width < 1 {
                    return Err("--strip-width must be at least 1".into());
                }
            }
            "--compression" => {
                compression = match value(&mut i, "--compression")?.as_str() {
                    "zlib" => Compression::Zlib,
                    "lz4" => Compression::Lz4,
                    "gzip" => Compression::Gzip,
                    "none" => Compression::None,
                    other => {
                        return Err(format!(
                            "unknown compression {other}; zlib, lz4, gzip, or none"
                        )
                        .into());
                    }
                }
            }
            "--quality" => quality = level::Quality::parse(&value(&mut i, "--quality")?)?,
            "--gpu" => {
                gpu = match value(&mut i, "--gpu")?.as_str() {
                    "auto" => fast_terrain::GpuChoice::Auto,
                    "on" => fast_terrain::GpuChoice::On,
                    "off" => fast_terrain::GpuChoice::Off,
                    other => {
                        return Err(format!("unknown gpu {other}; auto, on, or off").into());
                    }
                }
            }
            "--order-file" => order_file = Some(PathBuf::from(value(&mut i, "--order-file")?)),
            "--verbose" | "-v" => verbose = true,
            "--debug" => debug = true,
            other => return Err(format!("unknown argument {other}; see --help").into()),
        }
        i += 1;
    }
    Ok(GenerateArgs {
        resources,
        minecraft_dir,
        server_jar,
        java_home,
        out: out.ok_or("--out is required")?,
        seed: seed.ok_or("--seed is required")?,
        name,
        radius,
        shape,
        center,
        threads,
        strip_width,
        compression,
        quality,
        gpu,
        order_file,
        verbose,
        debug,
        dimensions,
        all_dimensions,
        preset,
        structures,
        datapacks,
    })
}

fn run(args: &[String]) -> Result<(), Error> {
    match args.first().map(String::as_str) {
        None if menu::is_terminal() => match menu::run()? {
            None => Ok(()),
            Some(args) => {
                menu::mark_ran();
                pipeline::generate(&args)
            }
        },
        None | Some("--help" | "-h" | "help") => {
            print!("{HELP}");
            Ok(())
        }
        Some("--version" | "-V") => {
            println!(
                "brahma {} (Java Edition {}, data version {})",
                env!("CARGO_PKG_VERSION"),
                horizon_core::version::VANILLA_VERSION,
                horizon_core::version::DATA_VERSION
            );
            Ok(())
        }
        Some("generate") => {
            let args = parse_generate(&args[1..])?;
            pipeline::generate(&args)
        }
        Some("dump") => {
            let mut resources = std::env::var_os("HORIZON_RESOURCES").map(PathBuf::from);
            let mut dimension = "minecraft:overworld".to_string();
            let mut rest = Vec::new();
            let mut i = 1;
            while i < args.len() {
                if args[i] == "--resources" {
                    i += 1;
                    resources = Some(PathBuf::from(
                        args.get(i).ok_or("--resources needs a value")?,
                    ));
                } else if args[i] == "--dimension" {
                    i += 1;
                    dimension =
                        level::parse_dimension(args.get(i).ok_or("--dimension needs a value")?)?;
                } else {
                    rest.push(args[i].clone());
                }
                i += 1;
            }
            let world = rest.first().ok_or("dump needs a world folder")?;
            let rects = rest[1..]
                .iter()
                .map(|rect| {
                    let v: Vec<i32> = rect
                        .split(',')
                        .map(str::parse)
                        .collect::<Result<_, _>>()
                        .map_err(|_| format!("rectangle {rect:?}: expected x0,z0,x1,z1"))?;
                    let v: [i32; 4] = v
                        .try_into()
                        .map_err(|_| format!("rectangle {rect:?}: expected x0,z0,x1,z1"))?;
                    Ok(v)
                })
                .collect::<Result<Vec<_>, Error>>()?;
            if rects.is_empty() {
                return Err("dump needs at least one x0,z0,x1,z1 rectangle".into());
            }
            let resources = resources.or_else(cache::existing).ok_or(
                "dump needs --resources <dir> (or HORIZON_RESOURCES, or a prepared cache)",
            )?;
            dump::dump(&resources, std::path::Path::new(world), &dimension, &rects)
        }
        Some(other) => Err(format!("unknown command {other}; see --help").into()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("brahma: {error}");
            ExitCode::from(2)
        }
    };
    // After the result is on screen, including a generate error printed above.
    if menu::ran() {
        menu::pause();
    }
    code
}
