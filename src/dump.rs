//! Print saved chunks in the text format of Horizon's
//! `VanillaChunkGenerationOracle` / `chunk_dump` probe, so
//! `tools/verify_parity.py` can compare a generated world with vanilla.
use crate::Error;
use horizon_core::position::ChunkPos;
use horizon_world::{
    chunk::{BlockState, PalettedContainer},
    nbt::{Limits, NamedTag, Tag},
    region::RegionFile,
    registry::BlockRegistry,
};
use std::{
    collections::HashMap,
    fmt::Write as _,
    io::{self, Write as _},
    path::Path,
};

fn tag_string(tag: Option<&Tag>) -> Option<String> {
    match tag {
        Some(Tag::String(s)) => Some(s.to_string_lossy()),
        _ => None,
    }
}

/// Type-prefixed scalars with sorted compound keys, as the oracle prints them.
fn canonical(tag: &Tag, out: &mut String) {
    match tag {
        Tag::End => {}
        Tag::Byte(v) => write!(out, "b{v}").unwrap(),
        Tag::Short(v) => write!(out, "s{v}").unwrap(),
        Tag::Int(v) => write!(out, "i{v}").unwrap(),
        Tag::Long(v) => write!(out, "l{v}").unwrap(),
        Tag::Float(v) => write!(out, "f{}", *v as i32).unwrap(),
        Tag::Double(v) => write!(out, "d{}", *v as i64).unwrap(),
        Tag::String(s) => {
            let text = s
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"");
            write!(out, "\"{text}\"").unwrap();
        }
        Tag::ByteArray(v) => {
            out.push_str("B[");
            for b in v {
                write!(out, "{},", *b as i8).unwrap();
            }
            out.push(']');
        }
        Tag::IntArray(v) => {
            out.push_str("I[");
            for b in v {
                write!(out, "{b},").unwrap();
            }
            out.push(']');
        }
        Tag::LongArray(v) => {
            out.push_str("L[");
            for b in v {
                write!(out, "{b},").unwrap();
            }
            out.push(']');
        }
        Tag::List(v) => {
            out.push('[');
            for item in v {
                canonical(item, out);
                out.push(',');
            }
            out.push(']');
        }
        Tag::Compound(c) => {
            out.push('{');
            for (k, v) in c {
                write!(out, "{}=", k.to_string_lossy()).unwrap();
                canonical(v, out);
                out.push(',');
            }
            out.push('}');
        }
    }
}

fn byte(tag: Option<&Tag>) -> Option<i8> {
    match tag {
        Some(Tag::Byte(v)) => Some(*v),
        _ => None,
    }
}

fn int(tag: Option<&Tag>) -> Option<i32> {
    match tag {
        Some(Tag::Int(v)) => Some(*v),
        Some(Tag::Byte(v)) => Some(i32::from(*v)),
        Some(Tag::Short(v)) => Some(i32::from(*v)),
        Some(Tag::Long(v)) => Some(*v as i32),
        _ => None,
    }
}

struct OpenRegions {
    dir: std::path::PathBuf,
    files: HashMap<(i32, i32), RegionFile>,
}

impl OpenRegions {
    fn chunk(&mut self, pos: ChunkPos) -> Result<Option<NamedTag>, Error> {
        let region = pos.region();
        let key = (region.x, region.z);
        if !self.files.contains_key(&key) {
            let path = self.dir.join(format!("r.{}.{}.mca", region.x, region.z));
            if !path.exists() {
                return Ok(None);
            }
            self.files.insert(key, RegionFile::open(path)?);
        }
        let file = self.files.get_mut(&key).unwrap();
        match file.read(pos)? {
            Some(record) => Ok(Some(record.nbt(Limits::saved())?)),
            None => Ok(None),
        }
    }
}

fn dump_chunk(registry: &BlockRegistry, root: &Tag, out: &mut impl io::Write) -> Result<(), Error> {
    let x = int(root.get("xPos")).ok_or("chunk missing xPos")?;
    let z = int(root.get("zPos")).ok_or("chunk missing zPos")?;
    let y_pos = int(root.get("yPos")).unwrap_or(-4);
    writeln!(out, "chunk {x} {z}")?;

    let Tag::List(sections) = root.get("sections").ok_or("chunk missing sections")? else {
        return Err("sections is not a list".into());
    };
    let mut by_y: HashMap<i8, &Tag> = HashMap::new();
    let mut min_section = i8::MAX;
    let mut max_section = i8::MIN;
    for section in sections {
        let y = byte(section.get("Y")).ok_or("section missing Y")?;
        by_y.insert(y, section);
        min_section = min_section.min(y);
        max_section = max_section.max(y);
    }
    if by_y.is_empty() {
        min_section = y_pos as i8;
        max_section = min_section;
    }

    let min_y = i32::from(min_section) * 16;
    let max_y = (i32::from(max_section) + 1) * 16;

    for qy in (min_y >> 2)..=((max_y - 1) >> 2) {
        let section_y = (qy >> 2) as i8;
        let mut line = format!("q {qy}");
        match by_y.get(&section_y).and_then(|s| s.get("biomes")) {
            Some(biomes) => {
                let palette = PalettedContainer::from_nbt(biomes, 64, 1)?;
                for qz in 0..4 {
                    for qx in 0..4 {
                        let index = (((qy & 3) << 4) | (qz << 2) | qx) as usize;
                        let name =
                            tag_string(Some(palette.get(index)?)).unwrap_or_else(|| "?".into());
                        write!(line, " {name}")?;
                    }
                }
            }
            None => {
                for _ in 0..16 {
                    write!(line, " minecraft:plains")?;
                }
            }
        }
        writeln!(out, "{line}")?;
    }

    for y in min_y..max_y {
        let mut line = format!("y {y}");
        let section = by_y.get(&(y.div_euclid(16) as i8));
        let palette = match section.and_then(|s| s.get("block_states")) {
            Some(states) => Some(PalettedContainer::from_nbt(states, 4096, 4)?),
            None => None,
        };
        for zz in 0..16 {
            for xx in 0..16 {
                let id = if let Some(palette) = &palette {
                    let index = (((y & 15) << 8) | (zz << 4) | xx) as usize;
                    let state = BlockState::from_nbt(palette.get(index)?)?;
                    registry.resolve(&state).map(|s| s.0).unwrap_or(0)
                } else {
                    0
                };
                write!(line, " {id}")?;
            }
        }
        writeln!(out, "{line}")?;
    }

    let mut entities: Vec<(i32, i32, i32, &Tag)> = Vec::new();
    if let Some(Tag::List(list)) = root.get("block_entities") {
        for tag in list {
            let bx = int(tag.get("x")).unwrap_or(0);
            let by = int(tag.get("y")).unwrap_or(0);
            let bz = int(tag.get("z")).unwrap_or(0);
            entities.push((bx, by, bz, tag));
        }
    }
    entities.sort_by_key(|&(bx, by, bz, _)| (bx, by, bz));
    for (bx, by, bz, tag) in entities {
        let mut text = String::new();
        canonical(tag, &mut text);
        writeln!(out, "be {bx} {by} {bz}\t{text}")?;
    }

    let dump_ticks = |key: &str, kind: &str, out: &mut dyn io::Write| -> Result<(), Error> {
        let Some(Tag::List(list)) = root.get(key) else {
            return Ok(());
        };
        for t in list {
            let name = tag_string(t.get("i")).unwrap_or_else(|| "?".into());
            let px = int(t.get("x")).unwrap_or(0);
            let py = int(t.get("y")).unwrap_or(0);
            let pz = int(t.get("z")).unwrap_or(0);
            let p = int(t.get("p")).unwrap_or(0);
            writeln!(out, "t {kind} {name} {px} {py} {pz} {p}")?;
        }
        Ok(())
    };
    dump_ticks("block_ticks", "block", out)?;
    dump_ticks("fluid_ticks", "fluid", out)?;

    if let Some(Tag::List(sections)) = root.get("PostProcessing") {
        for (si, marks) in sections.iter().enumerate() {
            let Tag::List(marks) = marks else {
                continue;
            };
            for mark in marks {
                let p = match mark {
                    Tag::Short(v) => i32::from(*v),
                    Tag::Int(v) => *v,
                    _ => continue,
                };
                writeln!(
                    out,
                    "p {} {} {}",
                    x * 16 + (p & 15),
                    (y_pos + si as i32) * 16 + ((p >> 4) & 15),
                    z * 16 + ((p >> 8) & 15)
                )?;
            }
        }
    }
    Ok(())
}

pub fn dump(
    resources: &Path,
    world: &Path,
    dimension: &str,
    rects: &[[i32; 4]],
) -> Result<(), Error> {
    let registry = crate::resources::registry(resources)?;
    let mut regions = OpenRegions {
        dir: crate::level::dimension_dir(world, dimension).join("region"),
        files: HashMap::new(),
    };
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    for &[x0, z0, x1, z1] in rects {
        for z in z0..=z1 {
            for x in x0..=x1 {
                let Some(root) = regions.chunk(ChunkPos::new(x, z))? else {
                    return Err(format!("chunk {x} {z} is missing").into());
                };
                dump_chunk(registry, &root.tag, &mut out)?;
            }
        }
    }
    out.flush()?;
    Ok(())
}
