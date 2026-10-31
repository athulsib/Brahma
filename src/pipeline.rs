//! The generation sweep. Each dimension is its own sweep, and each sweep is
//! cut into column strips of [`STRIP_WIDTH`] so memory holds a few rows of
//! one strip rather than the full width.
//!
//! Inside a strip the area is still swept north to south:
//!
//! 1. rows up to `z + 1` are generated (or restored, when a neighbour was
//!    saved by an earlier strip or a previous run), with two chunks of margin.
//!    `--quality fast` fills those chunks from approximate grids instead of
//!    `generate_proto`; decoration is the same either way;
//! 2. row `z` is decorated in three passes by `x mod 3`;
//! 3. row `z - 1` is written. Columns whose eastern neighbour in the strip is
//!    already decorated are final and go to the region file, and an existing
//!    chunk is left untouched. The easternmost column and the two-chunk halo
//!    are not final yet: the next strip restores them, so decoration spill
//!    across the boundary matches one uninterrupted run. They are stored
//!    under `.brahma/wip`, not in the world's region folder. A run left by
//!    horizon-worldgen is restored from `.horizon-worldgen/wip`.
use crate::{
    Error, GenerateArgs,
    area::{Area, strips},
    cache, chunk_nbt, fast_terrain, level,
    resources::Resources,
    restore,
};
use horizon_core::position::ChunkPos;
use horizon_world::{
    compression::Compression,
    nbt::{Limits, Tag},
    region::{ChunkRecord, RegionFile},
    worldgen::generator::{DecorateStats, GeneratorWorker, OverworldGenerator, ProtoChunk},
};
use std::{
    collections::HashMap,
    fs,
    io::{IsTerminal, Write},
    path::PathBuf,
    sync::Mutex,
    time::Instant,
};

/// Run `job` on every item of `queue` across `workers`, one thread per worker.
fn parallel<J: Send, R: Send>(
    workers: &mut [GeneratorWorker],
    queue: Vec<J>,
    job: impl Fn(&mut GeneratorWorker, J) -> Result<R, Error> + Sync,
) -> Result<Vec<R>, Error> {
    let queue = Mutex::new(queue);
    let results = Mutex::new(Vec::new());
    let job = &job;
    std::thread::scope(|scope| -> Result<(), Error> {
        let handles: Vec<_> = workers
            .iter_mut()
            .map(|worker| {
                let (queue, results) = (&queue, &results);
                scope.spawn(move || -> Result<(), Error> {
                    loop {
                        let Some(item) = queue.lock().unwrap().pop() else {
                            return Ok(());
                        };
                        let result = job(worker, item)?;
                        results.lock().unwrap().push(result);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().map_err(|_| "a worker thread panicked")??;
        }
        Ok(())
    })?;
    Ok(results.into_inner().unwrap())
}

fn unix_seconds() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32)
}

fn clip(span: Option<(i32, i32)>, lo: i32, hi: i32) -> Option<(i32, i32)> {
    let (start, end) = span?;
    let start = start.max(lo);
    let end = end.min(hi);
    (start <= end).then_some((start, end))
}

/// The decoration order of row `z` inside `x_lo..=x_hi`: chunks within one
/// chunk of the area, by `x mod 3` then west to east.
fn decoration_passes(area: &Area, z: i32, x_lo: i32, x_hi: i32) -> [Vec<i32>; 3] {
    let mut passes: [Vec<i32>; 3] = Default::default();
    if let Some((lo, hi)) = clip(area.span_within(z, 1), x_lo, x_hi) {
        for x in lo..=hi {
            passes[x.rem_euclid(3) as usize].push(x);
        }
    }
    passes
}

#[derive(Clone, Copy)]
struct Strip {
    lo: i32,
    hi: i32,
    last: bool,
}

/// Open region files. `replace` is false for the world's regions (an existing
/// chunk is left as it was) and true for the strip-edge working set.
struct Regions {
    dir: PathBuf,
    open: HashMap<(i32, i32), RegionFile>,
}
impl Regions {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            open: HashMap::new(),
        }
    }
    fn file(&mut self, pos: ChunkPos) -> Result<&mut RegionFile, Error> {
        let region = pos.region();
        let key = (region.x, region.z);
        if !self.open.contains_key(&key) {
            fs::create_dir_all(&self.dir)?;
            let path = self.dir.join(format!("r.{}.{}.mca", region.x, region.z));
            self.open
                .insert(key, RegionFile::open_writable(&path, true)?);
        }
        Ok(self.open.get_mut(&key).unwrap())
    }
    fn read_tag(&mut self, pos: ChunkPos) -> Result<Option<Tag>, Error> {
        let Some(record) = self.file(pos)?.read(pos)? else {
            return Ok(None);
        };
        Ok(Some(record.nbt(Limits::saved())?.tag))
    }
    /// Read a chunk only when its region file is already on disk. Does not
    /// create the directory, so a missing legacy working set stays missing.
    fn read_existing(&mut self, pos: ChunkPos) -> Result<Option<Tag>, Error> {
        let region = pos.region();
        let path = self.dir.join(format!("r.{}.{}.mca", region.x, region.z));
        if !self.open.contains_key(&(region.x, region.z)) && !path.is_file() {
            return Ok(None);
        }
        self.read_tag(pos)
    }
    /// Writes `record` unless `replace` is false and the chunk is already there.
    /// Returns whether the file changed.
    fn write(&mut self, pos: ChunkPos, record: &ChunkRecord, replace: bool) -> Result<bool, Error> {
        let file = self.file(pos)?;
        if !replace && file.read(pos)?.is_some() {
            return Ok(false);
        }
        file.write(pos, record)
            .map_err(|e| format!("chunk {} {}: {e}", pos.x, pos.z))?;
        Ok(true)
    }
    fn sync_all(&mut self) -> Result<(), Error> {
        for file in self.open.values() {
            file.sync()?;
        }
        Ok(())
    }
}

fn dimensions_of(args: &GenerateArgs) -> Result<Vec<String>, Error> {
    let mut ids = Vec::new();
    if args.all_dimensions {
        for name in ["overworld", "the_nether", "the_end"] {
            ids.push(level::parse_dimension(name)?);
        }
    }
    for name in &args.dimensions {
        let id = level::parse_dimension(name)?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        ids.push("minecraft:overworld".into());
    }
    Ok(ids)
}

pub fn generate(args: &GenerateArgs) -> Result<(), Error> {
    let started = Instant::now();
    let resources = cache::resolve(args)?;
    let (res, packs) = Resources::load_with(&resources, args.datapacks.as_deref())?;
    eprintln!(
        "loaded resources in {:.1?} ({} block states, {} biomes)",
        started.elapsed(),
        res.registry.len(),
        res.data.biome_names().len()
    );
    let preset = level::parse_preset(&args.preset)?;
    let dims = dimensions_of(args)?;
    let saved = level::read_run(&args.out)?;
    if let Some(saved) = &saved {
        if saved.seed != args.seed
            || saved.preset != preset.id
            || saved.structures != args.structures
            || saved.quality != args.quality
        {
            return Err(format!(
                "{} was generated with seed {} preset {} structures {} quality {}; this run is seed {} preset {} structures {} quality {}",
                args.out.display(),
                saved.seed,
                saved.preset,
                saved.structures,
                saved.quality.name(),
                args.seed,
                preset.id,
                args.structures,
                args.quality.name()
            )
            .into());
        }
        eprintln!("resuming {}", args.out.display());
    }

    let place_structures = args.structures && args.quality == level::Quality::Exact;
    let fast = match args.quality {
        level::Quality::Exact => None,
        level::Quality::Fast => Some(fast_terrain::FastTerrain::open(args.seed, args.gpu)?),
    };
    if saved.is_none() {
        let overworld = OverworldGenerator::for_world(
            res.data.clone(),
            args.seed,
            preset.id,
            "minecraft:overworld",
            place_structures,
        )?;
        let spawn_xz = overworld.spawn_position()?;
        let mut worker = overworld.worker();
        let spawn_chunk = ChunkPos::new(spawn_xz[0] >> 4, spawn_xz[1] >> 4);
        let spawn_y = if let Some(fast) = &fast {
            let (min_y, height) = overworld.level();
            let grids = fast.columns_for(
                &[(spawn_chunk.x, spawn_chunk.z)],
                min_y,
                height,
                overworld.sea_level(),
            )?;
            worker
                .generate_fast(spawn_chunk, &grids[0].surface, &grids[0].caves)?
                .surface_height(spawn_xz[0], spawn_xz[1])
        } else {
            worker
                .generate_proto(spawn_chunk)?
                .surface_height(spawn_xz[0], spawn_xz[1])
        };
        let name = args.name.clone().unwrap_or_else(|| {
            args.out
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "World".into())
        });
        level::write(
            &args.out,
            &level::LevelInfo {
                name: &name,
                seed: args.seed,
                spawn: [spawn_xz[0], spawn_y, spawn_xz[1]],
                preset,
                structures: args.structures,
                packs: &packs,
            },
        )?;
        level::write_run(
            &args.out,
            &level::SavedRun {
                seed: args.seed,
                preset: preset.id.to_string(),
                structures: args.structures,
                quality: args.quality,
            },
        )?;
        eprintln!(
            "spawn {} {} {} (overworld surface)",
            spawn_xz[0], spawn_y, spawn_xz[1]
        );
    }

    let mut order_file = args
        .order_file
        .as_deref()
        .map(|path| -> Result<_, Error> {
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                fs::create_dir_all(dir)?;
            }
            Ok(std::io::BufWriter::new(fs::File::create(path)?))
        })
        .transpose()?;

    for id in &dims {
        let generator = OverworldGenerator::for_world(
            res.data.clone(),
            args.seed,
            preset.id,
            id,
            place_structures,
        )?;
        let fast_fill = fast.as_ref().map(|terrain| FastFill {
            terrain,
            min_y: generator.level().0,
            height: generator.level().1,
            sea: generator.sea_level(),
        });
        eprintln!(
            "dimension {id} ({})",
            match args.quality {
                level::Quality::Exact => "exact".to_string(),
                level::Quality::Fast => format!("fast, {}", fast.as_ref().unwrap().backend()),
            }
        );
        let mut workers: Vec<GeneratorWorker> =
            (0..args.threads).map(|_| generator.worker()).collect();
        let spawn_xz = generator.spawn_position()?;
        let center_block = args.center.unwrap_or((spawn_xz[0], spawn_xz[1]));
        let center = (center_block.0 >> 4, center_block.1 >> 4);
        let area = Area::new(center, args.radius / 16, args.shape);
        let total = area.total();
        eprintln!(
            "seed {}: {:?} of radius {} chunks around chunk {} {} ({} chunks)",
            args.seed, args.shape, area.radius, center.0, center.1, total
        );
        let dim_dir = level::dimension_dir(&args.out, id);
        let mut final_regions = Regions::new(dim_dir.join("region"));
        let dim_key = id.replace(':', "/");
        let wip_dir = args.out.join(".brahma").join("wip").join(&dim_key);
        let legacy_wip_dir = args
            .out
            .join(".horizon-worldgen")
            .join("wip")
            .join(&dim_key);
        let mut wip = Regions::new(wip_dir.clone());
        let mut legacy_wip = Regions::new(legacy_wip_dir.clone());
        let (x0, x1) = area.columns();
        let bands = strips(x0, x1, args.strip_width);
        eprintln!(
            "{} threads, strip width {} chunks, {} compression",
            args.threads,
            args.strip_width,
            compression_name(args.compression)
        );
        let mut stats = DecorateStats::default();
        let mut failed_chunks = 0u64;
        let mut saved = 0u64;
        let label = id.rsplit(':').next().unwrap_or(id);
        let mut progress = Progress::new(label, total, bands.len(), args.debug);
        for (i, &(lo, hi)) in bands.iter().enumerate() {
            let strip = Strip {
                lo,
                hi,
                last: i + 1 == bands.len(),
            };
            eprintln!("strip {} of {}, columns {lo} to {hi}", i + 1, bands.len());
            if args.debug {
                eprintln!(
                    "strip {lo}..={hi} ({}/{}, width {})",
                    i + 1,
                    bands.len(),
                    args.strip_width
                );
            }
            progress.begin_strip(i + 1);
            let (one_saved, one_failed) = sweep_strip(
                &res,
                &mut workers,
                &area,
                strip,
                &mut final_regions,
                &mut wip,
                &mut legacy_wip,
                order_file.as_mut(),
                &mut stats,
                total,
                args.compression,
                fast_fill.as_ref(),
                &mut progress,
            )?;
            saved += one_saved;
            failed_chunks += one_failed;
            progress.commit();
        }
        let speeds = progress.speeds_line();
        final_regions.sync_all()?;
        wip.sync_all()?;
        legacy_wip.sync_all()?;
        drop(wip);
        drop(legacy_wip);
        let _ = fs::remove_dir_all(&wip_dir);
        let _ = fs::remove_dir_all(&legacy_wip_dir);
        eprintln!(
            "{id}: {saved} chunks written in {:.1?}; feature roots placed {}, unsupported {}, failed {}",
            started.elapsed(),
            stats.placed,
            stats.unsupported,
            stats.failed
        );
        if let Some(speeds) = speeds {
            eprintln!("{speeds}");
        }
        if !stats.skipped.is_empty() {
            let mut skipped: Vec<_> = stats.skipped.iter().cloned().collect();
            skipped.sort();
            eprintln!("skipped feature roots: {}", skipped.join(" "));
        }
        if args.verbose {
            for (feature, error) in generator.unsupported_features() {
                eprintln!("unsupported {feature}: {error}");
            }
        }
        if failed_chunks > 0 {
            eprintln!("{failed_chunks} chunks had failed or unsupported features");
        }
    }
    if let Some(mut file) = order_file {
        file.flush()?;
    }
    eprintln!("world written to {}", args.out.display());
    Ok(())
}

/// Digits with thousands separators (`1565001` → `1,565,001`).
fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

fn human_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else if secs < 86_400 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {:02}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

fn bar(frac: f64, width: usize) -> String {
    let filled = ((frac.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    format!("[{}{}]", "=".repeat(filled), "-".repeat(width - filled))
}

/// Nearest-rank percentile of a sorted slice. `p` is 0..=100.
fn percentile(sorted: &[f64], p: u8) -> f64 {
    let rank = ((f64::from(p) / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[(rank.max(1) - 1).min(sorted.len() - 1)]
}

const SPEED_PERCENTILES: [u8; 6] = [1, 25, 50, 75, 90, 99];

/// The rate distribution, printed once when a dimension finishes.
fn speed_summary(samples: &[f64]) -> Option<String> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let body = SPEED_PERCENTILES
        .iter()
        .map(|p| format!("p{p} {:.0}", percentile(&sorted, *p)))
        .collect::<Vec<_>>()
        .join("  ");
    Some(format!("speeds chunks/s: {body}"))
}

fn compression_name(compression: Compression) -> &'static str {
    match compression {
        Compression::Zlib => "zlib",
        Compression::Lz4 => "lz4",
        Compression::Gzip => "gzip",
        Compression::None => "none",
    }
}

/// One line for the area actually finished: chunks written into the world
/// (or already there, on resume), not the per-strip row counter.
/// `rate` is chunks finished divided by time so far. Zero means no sample yet.
fn meter_line(
    label: &str,
    finished: u64,
    total: u64,
    stage: &str,
    rate: f64,
    strip_i: usize,
    strip_n: usize,
) -> String {
    let frac = if total == 0 {
        1.0
    } else {
        finished as f64 / total as f64
    };
    let pct = ((frac * 100.0).floor() as u64).min(100);
    let eta = if finished >= total && total > 0 {
        "done".to_string()
    } else if rate > 0.0 {
        human_duration(((total - finished) as f64 / rate).round() as u64)
    } else {
        "--".to_string()
    };
    let speed = if rate > 0.0 {
        format!("{rate:.0} chunks/s")
    } else {
        "-- chunks/s".to_string()
    };
    format!(
        "{label} {pct:>3}% {}  {} / {} chunks  {stage}  {speed}  ETA {eta}  strip {strip_i}/{strip_n}",
        bar(frac, 20),
        grouped(finished),
        grouped(total),
    )
}

/// The line a person watches. On a terminal it overwrites itself; on a pipe
/// it prints a fresh line every couple of seconds.
struct Progress {
    label: String,
    total: u64,
    finished: u64,
    strip_i: usize,
    strip_n: usize,
    stage: &'static str,
    started: Instant,
    last_draw: Instant,
    /// Chunks finished at `last_sample`, and the recent interval rates.
    last_sample_finished: u64,
    last_sample: Instant,
    rates: std::collections::VecDeque<f64>,
    /// Width of the last overwritten line, so a shorter update erases the tail.
    drawn: usize,
    tty: bool,
    debug: bool,
}

impl Progress {
    fn new(label: &str, total: u64, strip_n: usize, debug: bool) -> Self {
        let now = Instant::now();
        Self {
            label: label.to_string(),
            total,
            finished: 0,
            strip_i: 1,
            strip_n: strip_n.max(1),
            stage: "generating",
            started: now,
            last_draw: now,
            last_sample_finished: 0,
            last_sample: now,
            rates: std::collections::VecDeque::new(),
            drawn: 0,
            tty: std::io::stderr().is_terminal(),
            debug,
        }
    }

    fn begin_strip(&mut self, index: usize) {
        self.strip_i = index;
        self.stage = "generating";
        if self.tty {
            self.paint(true);
        }
    }

    fn stage(&mut self, stage: &'static str) {
        if self.stage == stage {
            return;
        }
        self.stage = stage;
        if self.tty {
            self.paint(true);
        }
    }

    fn add_finished(&mut self, n: u64) {
        if n == 0 {
            return;
        }
        self.finished += n;
        self.note_rate();
        self.paint(self.tty);
    }

    /// One sample is chunks finished since the previous sample, over that
    /// interval. Short gaps are kept for the next sample so a tiny pause
    /// does not invent a huge rate.
    fn note_rate(&mut self) {
        let dt = self.last_sample.elapsed().as_secs_f64();
        let n = self.finished - self.last_sample_finished;
        if n == 0 || dt < 0.05 {
            return;
        }
        self.rates.push_back(n as f64 / dt);
        if self.rates.len() > 60 {
            self.rates.pop_front();
        }
        self.last_sample = Instant::now();
        self.last_sample_finished = self.finished;
    }

    fn paint(&mut self, force: bool) {
        let elapsed = self.last_draw.elapsed().as_secs_f64();
        if !force && elapsed < if self.tty { 0.25 } else { 2.0 } {
            return;
        }
        self.last_draw = Instant::now();
        let elapsed = self.started.elapsed().as_secs_f64();
        let rate = if elapsed > 0.0 && self.finished > 0 {
            self.finished as f64 / elapsed
        } else {
            0.0
        };
        let line = meter_line(
            &self.label,
            self.finished,
            self.total,
            self.stage,
            rate,
            self.strip_i,
            self.strip_n,
        );
        if self.tty {
            let width = self.drawn.max(line.len());
            eprint!("\r{line:<width$}");
            self.drawn = width;
            let _ = std::io::stderr().flush();
        } else {
            eprintln!("{line}");
            self.drawn = 0;
        }
    }

    fn speeds_line(&mut self) -> Option<String> {
        speed_summary(self.rates.make_contiguous())
    }

    /// Leave the current meter in the scrollback and start the next line below it.
    fn commit(&mut self) {
        self.paint(true);
        if self.drawn > 0 {
            eprintln!();
            self.drawn = 0;
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if self.drawn > 0 {
            eprintln!();
            self.drawn = 0;
        }
    }
}

struct FastFill<'a> {
    terrain: &'a fast_terrain::FastTerrain,
    min_y: i32,
    height: i32,
    sea: i32,
}

#[allow(clippy::too_many_arguments)]
fn sweep_strip(
    res: &Resources,
    workers: &mut [GeneratorWorker],
    area: &Area,
    strip: Strip,
    final_regions: &mut Regions,
    wip: &mut Regions,
    legacy_wip: &mut Regions,
    mut order_file: Option<&mut std::io::BufWriter<fs::File>>,
    stats: &mut DecorateStats,
    total: u64,
    compression: Compression,
    fast: Option<&FastFill<'_>>,
    progress: &mut Progress,
) -> Result<(u64, u64), Error> {
    let (z0, z1) = area.rows();
    let mut chunks: HashMap<(i32, i32), ProtoChunk> = HashMap::new();
    let mut generated_to = z0 - 3;
    let mut failed_chunks = 0u64;
    let mut done = 0u64;
    let mut saved = 0u64;
    let mut last_report = Instant::now();
    let gen_lo = strip.lo - 2;
    let gen_hi = strip.hi + 2;
    // The western margin (one chunk outside the area) is decorated with the
    // first strip. Later strips start on their own column; the previous
    // strip's eastern column is restored and not decorated again. The last
    // strip also decorates the eastern margin. That eastern column of a
    // non-last strip stays in the working set until the next strip spills
    // into it.
    let (area_lo, _) = area.columns();
    let decorate_lo = if strip.lo == area_lo {
        strip.lo - 1
    } else {
        strip.lo
    };
    let decorate_hi = if strip.last { strip.hi + 1 } else { strip.hi };

    progress.stage("generating");
    for z in z0 - 1..=z1 + 1 {
        if generated_to < z + 1 {
            progress.stage("generating");
        }
        while generated_to < z + 1 {
            let row = generated_to + 1;
            if let Some((lo, hi)) = clip(area.span_within(row, 2), gen_lo, gen_hi) {
                let mut pending = Vec::new();
                for x in lo..=hi {
                    let pos = ChunkPos::new(x, row);
                    if chunks.contains_key(&(x, row)) {
                        continue;
                    }
                    let saved_tag = if let Some(tag) = final_regions.read_tag(pos)? {
                        Some(tag)
                    } else if let Some(tag) = wip.read_tag(pos)? {
                        Some(tag)
                    } else {
                        legacy_wip.read_existing(pos)?
                    };
                    pending.push((pos, saved_tag));
                }
                let jobs: Vec<_> = if let Some(fast) = fast {
                    let coords: Vec<(i32, i32)> =
                        pending.iter().map(|(pos, _)| (pos.x, pos.z)).collect();
                    let grids =
                        fast.terrain
                            .columns_for(&coords, fast.min_y, fast.height, fast.sea)?;
                    pending
                        .into_iter()
                        .zip(grids)
                        .map(|((pos, tag), grid)| (pos, tag, Some(grid)))
                        .collect()
                } else {
                    pending
                        .into_iter()
                        .map(|(pos, tag)| (pos, tag, None))
                        .collect()
                };
                let made = parallel(workers, jobs, |worker, (pos, saved_tag, grid)| {
                    let mut proto = if let Some(grid) = grid {
                        worker.generate_fast(pos, &grid.surface, &grid.caves)?
                    } else {
                        worker.generate_proto(pos)?
                    };
                    if let Some(tag) = saved_tag {
                        restore::apply(res, &mut proto, &tag)?;
                    }
                    Ok((pos, proto))
                })?;
                for (pos, chunk) in made {
                    chunks.insert((pos.x, pos.z), chunk);
                }
            }
            generated_to = row;
        }

        let mut decorating = false;
        for pass in decoration_passes(area, z, decorate_lo, decorate_hi) {
            let pass: Vec<i32> = pass
                .into_iter()
                .filter(|&x| chunks.get(&(x, z)).is_some_and(|c| !c.is_decorated()))
                .collect();
            if pass.is_empty() {
                continue;
            }
            if !decorating {
                progress.stage("decorating");
                decorating = true;
            }
            if let Some(file) = order_file.as_mut() {
                for &x in &pass {
                    writeln!(file, "{x} {z}")?;
                }
            }
            let mut jobs = Vec::with_capacity(pass.len());
            for x in pass {
                let mut region = Vec::with_capacity(9);
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        region.push(
                            chunks
                                .remove(&(x + dx, z + dz))
                                .ok_or_else(|| format!("chunk {} {} missing", x + dx, z + dz))?,
                        );
                    }
                }
                let region: [ProtoChunk; 9] = region.try_into().map_err(|_| "region size")?;
                jobs.push((x, region));
            }
            let results = parallel(workers, jobs, |worker, (x, mut region)| {
                let one = worker.decorate(&mut region)?;
                Ok((x, region, one))
            })?;
            for (x, region, one) in results {
                if one.failed > 0 || one.unsupported > 0 {
                    failed_chunks += 1;
                }
                stats.placed += one.placed;
                stats.unsupported += one.unsupported;
                stats.failed += one.failed;
                stats.skipped.extend(one.skipped);
                for (i, chunk) in region.into_iter().enumerate() {
                    chunks.insert((x + i as i32 % 3 - 1, z + i as i32 / 3 - 1), chunk);
                }
            }
        }

        progress.stage("saving");
        let row = z - 1;
        let (wrote, finished) = save_row(
            res,
            workers,
            &mut chunks,
            final_regions,
            wip,
            strip,
            area,
            compression,
            row,
        )?;
        saved += wrote;
        progress.add_finished(finished);
        done += area
            .span(z)
            .map_or(0, |(lo, hi)| u64::from((hi - lo + 1) as u32));
        if progress.debug && (last_report.elapsed().as_secs_f64() >= 2.0 || z == z1 + 1) {
            last_report = Instant::now();
            if progress.drawn > 0 {
                eprintln!();
                progress.drawn = 0;
            }
            eprintln!(
                "row {z} of {z0}..={z1}: {done}/{total} chunks of the area visited, {} in memory",
                chunks.len()
            );
            progress.paint(true);
        }
    }
    progress.stage("saving");
    let (wrote, finished) = save_row(
        res,
        workers,
        &mut chunks,
        final_regions,
        wip,
        strip,
        area,
        compression,
        z1 + 1,
    )?;
    saved += wrote;
    progress.add_finished(finished);
    chunks.clear();
    Ok((saved, failed_chunks))
}

/// Encode and write row `row`. Final columns go to the world unless that
/// chunk is already saved. The strip's eastern edge and halo go to `wip`.
fn save_row(
    res: &Resources,
    workers: &mut [GeneratorWorker],
    chunks: &mut HashMap<(i32, i32), ProtoChunk>,
    final_regions: &mut Regions,
    wip: &mut Regions,
    strip: Strip,
    area: &Area,
    compression: Compression,
    row: i32,
) -> Result<(u64, u64), Error> {
    // Everything decorated up to one column short of the eastern edge is
    // final, including the previous strip's edge (strip.lo - 1) after this
    // strip spilled into it. The last strip has no later neighbour, so its
    // eastern margin is final too.
    let final_lo = strip.lo - 1;
    let final_hi = if strip.last {
        strip.hi + 1
    } else {
        strip.hi - 1
    };
    let (wip_lo, wip_hi) = if strip.last {
        (1, 0)
    } else {
        (strip.hi, strip.hi + 2)
    };
    let xs: Vec<i32> = chunks
        .keys()
        .filter(|&&(_, z)| z == row)
        .map(|&(x, _)| x)
        .collect();
    let timestamp = unix_seconds();
    let mut targets = Vec::new();
    for x in xs {
        let Some(chunk) = chunks.remove(&(x, row)) else {
            continue;
        };
        let slot = if (final_lo..=final_hi).contains(&x) && chunk.is_decorated() {
            Some(true)
        } else if (wip_lo..=wip_hi).contains(&x) {
            Some(false)
        } else {
            None
        };
        if let Some(is_final) = slot {
            targets.push((chunk, is_final));
        }
    }
    let records = parallel(workers, targets, |_, (chunk, is_final)| {
        let pos = chunk.position();
        Ok((
            pos,
            chunk_nbt::record(res, &chunk, timestamp, compression)?,
            is_final,
        ))
    })?;
    let mut written = 0u64;
    let mut finished = 0u64;
    for (pos, record, is_final) in records {
        let wrote = if is_final {
            final_regions.write(pos, &record, false)?
        } else {
            wip.write(pos, &record, true)?
        };
        if wrote && is_final {
            written += 1;
        }
        // Count the area chunk once it is final, including one already on disk.
        if is_final
            && area
                .span(pos.z)
                .is_some_and(|(lo, hi)| (lo..=hi).contains(&pos.x))
        {
            finished += 1;
        }
    }
    Ok((written, finished))
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_world::{
        chunk_status::ChunkStatus,
        compression::Compression,
        nbt::{NamedTag, Tag},
    };

    fn args(out: PathBuf, resources: PathBuf) -> GenerateArgs {
        GenerateArgs {
            resources: Some(resources),
            minecraft_dir: None,
            server_jar: None,
            java_home: None,
            out,
            seed: 12345,
            name: Some("Test".into()),
            radius: 32,
            shape: crate::area::Shape::Square,
            center: Some((0, 0)),
            threads: 2,
            strip_width: crate::area::STRIP_WIDTH,
            compression: Compression::Zlib,
            quality: level::Quality::Exact,
            gpu: fast_terrain::GpuChoice::Auto,
            order_file: None,
            verbose: false,
            debug: false,
            dimensions: Vec::new(),
            all_dimensions: false,
            preset: "minecraft:normal".into(),
            structures: true,
            datapacks: None,
        }
    }

    #[test]
    fn meter_tracks_finished_chunks_and_names_the_stage() {
        assert_eq!(grouped(1_565_001), "1,565,001");
        assert_eq!(human_duration(3), "3s");
        assert_eq!(human_duration(75), "1m 15s");
        assert_eq!(human_duration(3_720), "1h 02m");
        assert_eq!(
            meter_line("overworld", 255, 1000, "decorating", 255.0, 2, 5),
            "overworld  25% [=====---------------]  255 / 1,000 chunks  decorating  255 chunks/s  ETA 3s  strip 2/5"
        );
        assert_eq!(
            meter_line("overworld", 0, 1000, "generating", 0.0, 1, 5),
            "overworld   0% [--------------------]  0 / 1,000 chunks  generating  -- chunks/s  ETA --  strip 1/5"
        );
        assert_eq!(
            speed_summary(&[100.0, 200.0, 300.0, 400.0]).as_deref(),
            Some("speeds chunks/s: p1 100  p25 100  p50 200  p75 300  p90 400  p99 400")
        );
        assert!(meter_line("end", 10, 10, "saving", 5.0, 1, 1).contains("100%"));
        assert!(meter_line("end", 10, 10, "saving", 5.0, 1, 1).contains("ETA done"));
    }

    #[test]
    fn decoration_passes_cover_the_row_once() {
        let area = Area::new((4, 4), 3, crate::area::Shape::Square);
        let passes = decoration_passes(&area, 2, i32::MIN, i32::MAX);
        let mut all: Vec<i32> = passes.iter().flatten().copied().collect();
        all.sort();
        assert_eq!(all, (0..=8).collect::<Vec<_>>());
        for (i, pass) in passes.iter().enumerate() {
            assert!(pass.iter().all(|x| x.rem_euclid(3) as usize == i));
            assert!(pass.windows(2).all(|w| w[0] < w[1]));
        }
        assert!(
            decoration_passes(&area, 9, i32::MIN, i32::MAX)
                .iter()
                .all(Vec::is_empty)
        );
        let east = decoration_passes(&area, 2, 6, 8);
        let mut cols: Vec<i32> = east.iter().flatten().copied().collect();
        cols.sort();
        assert_eq!(cols, vec![6, 7, 8]);
    }

    #[test]
    fn an_existing_chunk_is_not_rewritten() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-worldgen-region-{}-{}",
            std::process::id(),
            unix_seconds()
        ));
        let _ = fs::remove_dir_all(&dir);
        let mut regions = Regions::new(dir.clone());
        let pos = ChunkPos::new(1, 2);
        let record = |marker: i32| {
            ChunkRecord::from_nbt(
                &NamedTag {
                    name: "".into(),
                    tag: Tag::compound([
                        ("marker", Tag::Int(marker)),
                        ("Status", Tag::String("minecraft:features".into())),
                    ]),
                },
                Compression::Zlib,
                1,
                Limits::saved(),
            )
            .unwrap()
        };
        assert!(regions.write(pos, &record(1), false).unwrap());
        assert!(!regions.write(pos, &record(2), false).unwrap());
        let tag = regions.read_tag(pos).unwrap().unwrap();
        assert_eq!(tag.get("marker"), Some(&Tag::Int(1)));
        assert_eq!(
            ChunkStatus::from_nbt(tag.get("Status").unwrap()).unwrap(),
            ChunkStatus::Features
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Encode a generated chunk and restore it onto a second proto.
    /// Skipped unless `HORIZON_RESOURCES` points at a prepared folder.
    #[test]
    fn restored_chunk_matches_the_encoded_one() {
        let Some(resources) = std::env::var_os("HORIZON_RESOURCES") else {
            eprintln!("HORIZON_RESOURCES not set: skipped");
            return;
        };
        let res = Resources::load(std::path::Path::new(&resources)).unwrap();
        let generator = OverworldGenerator::for_dimension(
            res.data.clone(),
            12345,
            "minecraft:normal",
            "minecraft:overworld",
        )
        .unwrap();
        let mut worker = generator.worker();
        let pos = ChunkPos::new(0, 0);
        let mut centre = worker.generate_proto(pos).unwrap();
        let mut around = [
            worker.generate_proto(ChunkPos::new(-1, -1)).unwrap(),
            worker.generate_proto(ChunkPos::new(0, -1)).unwrap(),
            worker.generate_proto(ChunkPos::new(1, -1)).unwrap(),
            worker.generate_proto(ChunkPos::new(-1, 0)).unwrap(),
            centre,
            worker.generate_proto(ChunkPos::new(1, 0)).unwrap(),
            worker.generate_proto(ChunkPos::new(-1, 1)).unwrap(),
            worker.generate_proto(ChunkPos::new(0, 1)).unwrap(),
            worker.generate_proto(ChunkPos::new(1, 1)).unwrap(),
        ];
        worker.decorate(&mut around).unwrap();
        centre = around.into_iter().nth(4).unwrap();
        let encoded = chunk_nbt::encode(&res, &centre);
        let mut again = worker.generate_proto(pos).unwrap();
        restore::apply(&res, &mut again, &encoded.tag).unwrap();
        assert!(again.is_decorated());
        let (x, y, z) = (3, 64, 5);
        assert_eq!(again.block(x, y, z), centre.block(x, y, z));
        assert_eq!(
            again.terrain.block_ticks().pack(0).len(),
            centre.terrain.block_ticks().pack(0).len()
        );
    }

    #[test]
    fn saved_chunks_read_back_as_generated() {
        let Some(resources) = std::env::var_os("HORIZON_RESOURCES") else {
            eprintln!("HORIZON_RESOURCES not set: skipped");
            return;
        };
        let out = std::env::temp_dir().join(format!(
            "horizon-worldgen-test-{}-{}",
            std::process::id(),
            unix_seconds()
        ));
        let _ = fs::remove_dir_all(&out);
        generate(&args(out.clone(), PathBuf::from(resources))).unwrap();
        let region = out
            .join("dimensions")
            .join("minecraft")
            .join("overworld")
            .join("region")
            .join("r.0.0.mca");
        let mut region = RegionFile::open(region).unwrap();
        let record = region
            .read(ChunkPos::new(0, 0))
            .unwrap()
            .expect("chunk 0 0 saved");
        let root = record.nbt(Limits::saved()).unwrap();
        assert_eq!(
            ChunkStatus::from_nbt(root.tag.get("Status").unwrap()).unwrap(),
            ChunkStatus::Features
        );
        assert_eq!(root.tag.get("isLightOn"), Some(&Tag::Byte(0)));
        let _ = fs::remove_dir_all(&out);
    }
}
