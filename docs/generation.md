# Generation

Running `brahma` with no arguments opens a menu. It asks for the world folder, seed, radius (Village 512, Normal 2,500, Large 10,000, or a number of blocks), shape, dimensions (overworld, or all three), exact or fast, GPU when fast, structures, and threads. `r` fills a new seed. If that folder is already a world Brahma wrote, the menu says the run will continue it, and a seed or quality that does not match is shown before anything starts. The first time, if the game files are not already prepared, it also asks for the client (the Minecraft folder, or `versions/26.3/26.3.jar`) and the server jar: download the pinned 26.3 jar from Mojang, type a path, or open [the server download page](https://www.minecraft.net/en-us/download/server). Preset, center, compression, strip width, datapacks, and the parity flags stay on the command line.

`brahma generate` writes a world folder:

- `level.dat` and `data/minecraft/*.dat` (seed, dimensions, clocks, weather, game rules)
- `dimensions/<namespace>/<path>/region/*.mca`

Every generated chunk is saved at status `minecraft:features`. Terrain, carvers, decoration, and (in exact quality) structures are done. Lighting and the last statuses (`initialize_light`, `light`, `spawn`, `full`) run when the game first loads each chunk. Brahma does not spawn the initial mobs, and it rejects the flat preset.

```text
brahma generate --out <world dir> --seed <i64|text>
      [--resources <dir>] [--minecraft-dir <dir>] [--server-jar <jar>]
      [--java-home <dir>] [--dimension <id>]... [--all-dimensions]
      [--preset normal|large_biomes|amplified|single_biome_surface]
      [--no-structures] [--datapacks <folder>]
      [--name <level name>] [--radius <blocks>] [--shape square|circle]
      [--center <x> <z>] [--threads <n>] [--strip-width <chunks>]
      [--compression zlib|lz4|gzip|none] [--quality exact|fast]
      [--gpu auto|on|off] [--debug]
```

When `--resources` is omitted, the first run builds `facts.json` and the worldgen data from your own Minecraft jars. `--minecraft-dir` is the official launcher folder. `--server-jar` is a 26.3 server jar you already have. The menu can download that jar and checks its checksum, or open the [server download page](https://www.minecraft.net/en-us/download/server). That cache is `%LOCALAPPDATA%/brahma/26.3` on Windows and `$XDG_DATA_HOME/brahma/26.3` elsewhere (`~/.local/share/brahma/26.3` when `XDG_DATA_HOME` is unset). Later runs reuse it. `--java-home` picks the JDK for that first prepare.

- `--dimension` is repeatable (`overworld`, `the_nether`, `the_end`, or a namespaced id). The default is the overworld. `--all-dimensions` is the three vanilla dimensions. A data-pack dimension is generated only when named.
- `--preset` defaults to `minecraft:normal`. `minecraft:flat` is rejected.
- `--no-structures` skips structures in exact quality. Fast quality does not place them either way.
- `--datapacks` is a folder of packs (`pack.mcmeta` in each child). Their worldgen overrides the vanilla data.
- `--name` is the level name. The default is the folder name.

```sh
brahma generate --out ./worlds/demo --seed 12345 --radius 2500 --threads 8
```

## How a run works

`--radius` is in blocks. The area is the chunks within that many blocks / 16 of the center chunk. `--shape square` is the default. `--shape circle` keeps the chunks inside that radius. The default center is that dimension's spawn column. `--center` sets a block position instead. The spawn stored in `level.dat` is always the overworld column.

A large area is cut into vertical strips, `--strip-width` columns wide (default 256). One strip is in memory at a time. Inside a strip the rows run north to south:

1. Chunks are generated, with a two-chunk margin, so trees, ores, and other decoration that cross a border land in the right chunk.
2. The row is decorated in three passes. The order does not depend on `--threads`, so the same settings write the same chunks.
3. Finished chunks are written into the region file. The edge the next strip still needs is kept in `.brahma/wip` and removed when the dimension finishes.

The console shows one line for the whole area: percent, chunks finished, the stage (generating, decorating, or saving), chunks per second so far, and an ETA for the whole area. When the dimension finishes, `speeds chunks/s:` lists p1, p25, p50, p75, p90, and p99 of the short-interval rates. The chunks/s on the progress line is the average for the area. `--debug` prints the per-row log instead.

## Exact and fast

`--quality exact` is the default. It runs the vanilla 26.3 generator for this decoration order: density, biomes, carvers, structures, and decoration.

`--quality fast` skips the density tree and writes a playable world from the real biome map: bedrock, stone, deepslate, water, a biome surface (grass, sand, gravel), simple caves, and the same decoration (ores, trees, plants). Hills and caves do not follow the seed. Structures are not placed. Chunks are still `minecraft:features`. Resuming a world onto the other quality is refused.

`--gpu` applies only to fast quality. `auto` (the default) builds the height and cave grids on a Vulkan device when one is available, and on the CPU otherwise. `on` is an error when no device is found. `off` always uses the CPU. Both paths use the same noise. The CPU fills the blocks either way. Windows finds `vulkan-1.dll` from the graphics driver. Linux needs `libvulkan.so.1` (often the `libvulkan1` or `vulkan-loader` package).

## What affects speed

Exact quality spends its time in the vanilla density math, carvers, structures, and decoration. Threads split that work. `--threads` defaults to the number of logical CPUs. More threads help until the machine is busy.

Fast quality skips the density tree, which is most of that CPU time. The GPU only builds the height and cave grids. Block filling and decoration stay on the CPU, so a GPU does not turn the rest of the run into a graphics job.

`--strip-width` trades memory for work at the joins. A wider strip holds more chunks and repeats less of the border between bands. A width that covers the area is a single band. Holding more than a few hundred megabytes of chunks does not make generation faster. The default 256 is the usual balance.

`--compression` changes how region files are encoded, not the blocks. `zlib` is the default. `lz4` and `none` spend less CPU on disk and use more space. `gzip` is the slowest writer. Java Edition 26.3 reads all four.

The time on the progress line is generation and saving. It does not include lighting. The game does that when you load the chunks.

## Resume

Run the same `generate` command on an existing world folder. Chunks already in their region are not rewritten. Unfinished strip edges are restored from `.brahma/wip`. The seed, preset, structures flag, and quality must match. That record is `brahma.json` next to `level.dat`.

A world written by an earlier build that left `horizon-worldgen.json` and `.horizon-worldgen/wip` resumes the same way when `brahma.json` is not there yet. A record with no `quality` field is exact.

## Dump

Print saved chunks as text:

```sh
brahma dump --dimension the_nether ./worlds/demo -1,-1,1,1
```
