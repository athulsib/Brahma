# Brahma

Brahma generates and pregenerates Minecraft: Java Edition 26.3 worlds. The
world folder opens in the game:

- `level.dat` and `data/minecraft/*.dat` (seed, dimensions, clocks, weather, game rules)
- `dimensions/<namespace>/<path>/region/*.mca` with every generated chunk at
  status `minecraft:features`

Terrain, carvers, structures (exact quality) and decoration are done. Lighting
and the last generation statuses (`initialize_light`, `light`, `spawn`, `full`)
run when the game first loads each chunk.

## Exact and fast

`--quality exact` (the default) is the vanilla generator: the same density
math, biomes, carvers, structures and decoration as Java Edition 26.3 for this
decoration order.

`--quality fast` skips the density tree. It still uses the real biome source,
sea level and the same decoration (ores, trees, plants), and it writes
playable `minecraft:features` chunks: bedrock, stone, deepslate, water, and a
biome surface. Hills and caves are approximate and do not match the seed.
Structures are not placed. The `--gpu` flag applies only in fast mode: it
builds the height and cave grids on a Vulkan device. The CPU fills the blocks.
`auto` uses the CPU when no Vulkan device is found.

## Build

Rust 1.85 or newer. Java is required the first time Brahma prepares assets
from your Minecraft jars. A Horizon checkout must sit next to this folder, at
`../horizon`.

`Cargo.toml` path-patches `horizon-world` and `horizon-core` and path-depends
on `horizon-prepare`. `ProtoChunk::from_approximate` and `prepare_generator`
are on that checkout and are not in the pinned git revision. A clone with no
sibling Horizon tree will not build until those land upstream.

```sh
cargo build --release
```

The binary is `target/release/brahma` (`brahma.exe` on Windows).

## Generate

```text
brahma generate --out <world dir> --seed <i64|text>
      [--resources <dir>] [--dimension <id>]... [--all-dimensions]
      [--preset normal|large_biomes|amplified|single_biome_surface]
      [--no-structures] [--datapacks <folder>]
      [--name <level name>] [--radius <blocks>] [--shape square|circle]
      [--center <x> <z>] [--threads <n>] [--strip-width <chunks>]
      [--compression zlib|lz4|gzip|none] [--quality exact|fast]
      [--gpu auto|on|off] [--order-file <path>] [--verbose]
      [--debug]
```

`--resources` accepts a prepared folder (`facts.json` and `worldgen/`). When
it is omitted, the first run extracts that bundle from your own jars (the
official launcher directory, or `--minecraft-dir` / `--server-jar`). New
prepares are cached under `%LOCALAPPDATA%/brahma/26.3` on Windows and
`$XDG_DATA_HOME/brahma/26.3` elsewhere. If that folder has no `facts.json` and
an older `horizon-worldgen` cache does, Brahma uses the older cache.

- `--out` may already be a world Brahma wrote. The seed, preset, structures
  flag and quality must match; chunks already in their region are not rewritten.
- `--dimension` is repeatable (`overworld`, `the_nether`, `the_end`, or a
  namespaced id). The default is the overworld. `--all-dimensions` is the
  three vanilla dimensions. A data-pack dimension is generated only when named.
- `--preset` defaults to `minecraft:normal`. `minecraft:flat` is rejected.
- `--datapacks` is a folder of packs (`pack.mcmeta` in each child). Their
  worldgen overrides the vanilla bundles.
- `--radius` is in **blocks**, in that dimension's chunks. The sweep is strips
  of `--strip-width` columns (default 256), so a large radius does not hold
  the whole width in memory.
- `--threads` is the worker count (default: logical CPUs).
- `--strip-width` is how many columns stay in one band. Wider uses more memory
  and recomputes less of the join between bands. A width that covers the area
  is a single band.
- `--compression` is the region encoding: `zlib` (default), `lz4`, `gzip`, or
  `none`. The chunk data is the same. `lz4` and `none` write faster and use
  more disk; 26.3 reads all four.
- `--gpu` is `auto` (default), `on`, or `off`. `on` is an error when no device
  is found. Both paths use the same noise.
- `--center` is a block position; the default is that dimension's spawn column.
  `level.dat` spawn is the overworld column.
- `--debug` prints the per-row sweep log. The normal display is one updating
  line: percent of the area finished, chunks done, the stage (generating,
  decorating, or saving), chunks per second so far, and an ETA for the whole
  area. When the dimension finishes, `speeds chunks/s:` lists p1, p25, p50,
  p75, p90, and p99 of the rates seen along the way.
- `--order-file` writes the decoration order (`x z` per line) for every
  dimension in the run. A parity check compares one dimension, so generate
  that dimension on its own when you pass the file to the oracle.

Example (a small square around spawn, about 80 chunks with the decoration
margin):

```sh
brahma generate \
  --out ./worlds/demo \
  --seed 12345 \
  --radius 16 \
  --all-dimensions \
  --threads 8 \
  --order-file order.txt
```

Open the folder in Horizon or vanilla 26.3. The game lights the chunks on load.

## Resume

A new run writes `brahma.json` beside `level.dat`, and keeps unfinished strip
edges in `.brahma/wip`. Starting `generate` again on that folder continues:
finished region chunks are left as they are.

A world generated by horizon-worldgen still resumes. Brahma reads
`horizon-worldgen.json` when `brahma.json` is absent, and restores a strip
from `.horizon-worldgen/wip` when a previous run left that directory. The seed,
preset, structures flag and quality must still match. A sidecar with no
`quality` field is exact.

## Dump

Print saved chunks in the text format of Horizon's chunk-generation oracle:

```sh
brahma dump --dimension the_nether ./worlds/demo -1,-1,1,1
```

## Parity

Decoration order is deterministic and independent of the thread count: each row
west to east in three passes (`x mod 3`). Vanilla has no single canonical order
(it depends on how a player approaches), so this order is the contract.

From a Horizon checkout that has the 26.3 jars and `.local/` data:

```sh
python tools/verify_parity.py \
  --horizon /path/to/horizon \
  --resources /path/to/horizon/.local/horizon-client/26.3 \
  --seed 12345 --rect -1,-1,1,1 \
  --dimension overworld --dimension the_nether --dimension the_end
```

That generates a small world with the `brahma` binary, runs vanilla's
`VanillaChunkGenerationOracle` with the same `HORIZON_ORDER` for each
dimension, and compares blocks, biomes, ticks, post-processing and block
entities. Parity is "same seed, this decoration order", not "however a player
walked in". Fast quality is not part of that comparison.

## Not in this tool

Lighting, `minecraft:full`, and the initial mob spawn stay the game's job when
a chunk is loaded.

## Credits

- Athulsib
- World generation is [Horizon](https://github.com/horizon-rust/horizon), a clean-room Rust port. Brahma does not include Mojang's code. The first run prepares assets from the player's own Minecraft jars.
