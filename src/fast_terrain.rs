//! Approximate terrain grids for `--quality fast`.
//!
//! A few octaves of value noise, the same function on the CPU and in a WGSL
//! compute shader run on Vulkan. This is not the vanilla density router and
//! not Java `Math`: hills and caves do not match the seed. The block fill
//! (`ProtoChunk::from_approximate`) is shared; only these grids change with `--gpu`.
use crate::Error;

/// One chunk: a surface height per column, and one cave byte per block.
#[derive(Clone)]
pub struct ColumnGrids {
    /// `surface[z * 16 + x]` is the y of the top solid block.
    pub surface: [i32; 256],
    /// `((y - min_y) as usize) * 256 + z * 16 + x` is non-zero in a cave.
    pub caves: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuChoice {
    Auto,
    On,
    Off,
}

pub struct FastTerrain {
    seed: u32,
    gpu: Option<Gpu>,
}

impl FastTerrain {
    pub fn open(seed: i64, gpu: GpuChoice) -> Result<Self, Error> {
        let seed = mix_seed(seed);
        let gpu = match gpu {
            GpuChoice::Off => None,
            GpuChoice::On => Some(Gpu::new().map_err(|e| format!("--gpu on: {e}"))?),
            GpuChoice::Auto => match Gpu::new() {
                Ok(gpu) => Some(gpu),
                Err(GpuError::NoAdapter(reason)) => {
                    eprintln!("fast terrain: no GPU ({reason}); using the CPU");
                    None
                }
                Err(GpuError::Failed(reason)) => {
                    return Err(format!("fast terrain: {reason}").into());
                }
            },
        };
        Ok(Self { seed, gpu })
    }

    pub fn backend(&self) -> &'static str {
        if self.gpu.is_some() { "gpu" } else { "cpu" }
    }

    pub fn columns_for(
        &self,
        chunks: &[(i32, i32)],
        min_y: i32,
        height: i32,
        sea: i32,
    ) -> Result<Vec<ColumnGrids>, Error> {
        if chunks.is_empty() {
            return Ok(Vec::new());
        }
        if height <= 0 {
            return Err("fast terrain: build height must be positive".into());
        }
        if let Some(gpu) = &self.gpu {
            return gpu
                .columns(self.seed, chunks, min_y, height, sea)
                .map_err(|e| format!("fast terrain: {e}").into());
        }
        Ok(cpu_columns(self.seed, chunks, min_y, height, sea))
    }
}

fn mix_seed(seed: i64) -> u32 {
    (seed as u32) ^ ((seed >> 32) as u32)
}

fn hash01(x: i32, y: i32, z: i32, seed: u32) -> f32 {
    let n0 = (x as u32).wrapping_mul(374761393);
    let n1 = (y as u32).wrapping_mul(668265263);
    let n2 = (z as u32).wrapping_mul(2146121005);
    let mut n = n0 ^ n1 ^ n2 ^ seed;
    n = n.wrapping_mul(n.wrapping_mul(1274126177).wrapping_add(1));
    ((n >> 9) & 0x7fffff) as f32 / 8388607.0
}

fn floor_i(v: f32) -> i32 {
    let t = v as i32;
    if v < t as f32 { t - 1 } else { t }
}

fn round_i(v: f32) -> i32 {
    floor_i(v + 0.5)
}

fn fade(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn value2(x: f32, z: f32, seed: u32) -> f32 {
    let x0 = floor_i(x);
    let z0 = floor_i(z);
    let fx = fade(x - x0 as f32);
    let fz = fade(z - z0 as f32);
    let v00 = hash01(x0, 0, z0, seed);
    let v10 = hash01(x0 + 1, 0, z0, seed);
    let v01 = hash01(x0, 0, z0 + 1, seed);
    let v11 = hash01(x0 + 1, 0, z0 + 1, seed);
    lerp(lerp(v00, v10, fx), lerp(v01, v11, fx), fz)
}

fn value3(x: f32, y: f32, z: f32, seed: u32) -> f32 {
    let x0 = floor_i(x);
    let y0 = floor_i(y);
    let z0 = floor_i(z);
    let fx = fade(x - x0 as f32);
    let fy = fade(y - y0 as f32);
    let fz = fade(z - z0 as f32);
    let sample = |dx: i32, dy: i32, dz: i32| hash01(x0 + dx, y0 + dy, z0 + dz, seed);
    let x00 = lerp(sample(0, 0, 0), sample(1, 0, 0), fx);
    let x10 = lerp(sample(0, 0, 1), sample(1, 0, 1), fx);
    let x01 = lerp(sample(0, 1, 0), sample(1, 1, 0), fx);
    let x11 = lerp(sample(0, 1, 1), sample(1, 1, 1), fx);
    lerp(lerp(x00, x10, fz), lerp(x01, x11, fz), fy)
}

fn surface_y(wx: i32, wz: i32, seed: u32, sea: i32, min_y: i32, max_y: i32) -> i32 {
    let x = wx as f32;
    let z = wz as f32;
    let continent = value2(x / 96.0, z / 96.0, seed);
    let hills = value2(x / 24.0, z / 24.0, seed ^ 0x9e37_79b9);
    let detail = value2(x / 8.0, z / 8.0, seed ^ 0x85eb_ca6b);
    let base = if continent < 0.45 {
        sea as f32 - 8.0 - (0.45 - continent) * 40.0
    } else {
        sea as f32 + (continent - 0.45) * 24.0
    };
    let h = base + (hills - 0.5) * 16.0 + (detail - 0.5) * 6.0;
    round_i(h).clamp(min_y, max_y)
}

fn is_cave(wx: i32, y: i32, wz: i32, surface: i32, min_y: i32, seed: u32) -> bool {
    if y <= min_y + 4 || y >= surface - 4 {
        return false;
    }
    value3(
        wx as f32 / 10.0,
        y as f32 / 8.0,
        wz as f32 / 10.0,
        seed ^ 0xc2b2_ae35,
    ) < 0.38
}

fn cpu_columns(
    seed: u32,
    chunks: &[(i32, i32)],
    min_y: i32,
    height: i32,
    sea: i32,
) -> Vec<ColumnGrids> {
    let max_y = min_y + height - 1;
    chunks
        .iter()
        .map(|&(cx, cz)| {
            let mut surface = [0; 256];
            let mut caves = vec![0u8; height as usize * 256];
            let origin_x = cx.wrapping_shl(4);
            let origin_z = cz.wrapping_shl(4);
            for lz in 0..16 {
                for lx in 0..16 {
                    let column = lz * 16 + lx;
                    let wx = origin_x.wrapping_add(lx as i32);
                    let wz = origin_z.wrapping_add(lz as i32);
                    let top = surface_y(wx, wz, seed, sea, min_y, max_y);
                    surface[column] = top;
                    for y in min_y..=max_y {
                        if is_cave(wx, y, wz, top, min_y, seed) {
                            let yi = (y - min_y) as usize;
                            caves[yi * 256 + column] = 1;
                        }
                    }
                }
            }
            ColumnGrids { surface, caves }
        })
        .collect()
}

fn params_bytes(seed: u32, min_y: i32, height: i32, sea: i32, count: u32) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[0..4].copy_from_slice(&seed.to_le_bytes());
    out[4..8].copy_from_slice(&min_y.to_le_bytes());
    out[8..12].copy_from_slice(&height.to_le_bytes());
    out[12..16].copy_from_slice(&sea.to_le_bytes());
    out[16..20].copy_from_slice(&count.to_le_bytes());
    out
}

mod gpu;

use gpu::{Gpu, GpuError};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_grids_are_stable_and_stay_inside_the_world() {
        let once = cpu_columns(mix_seed(12345), &[(0, 0), (-3, 4)], -64, 384, 63);
        let twice = cpu_columns(mix_seed(12345), &[(0, 0), (-3, 4)], -64, 384, 63);
        assert_eq!(once.len(), 2);
        assert_eq!(once[0].surface, twice[0].surface);
        assert_eq!(once[0].caves, twice[0].caves);
        assert!(once[0].surface.iter().all(|y| (-64..320).contains(y)));
        assert!(once[1].surface.iter().any(|y| *y != once[0].surface[0]));
        assert_eq!(once[0].caves.len(), 384 * 256);
        assert!(once[0].caves.iter().any(|b| *b == 1));
    }

    #[test]
    fn gpu_grids_match_the_cpu() {
        let gpu = match Gpu::new() {
            Ok(gpu) => gpu,
            Err(GpuError::NoAdapter(_)) => return,
            Err(GpuError::Failed(reason)) => panic!("{reason}"),
        };
        let chunks = [(0, 0), (-2, 5)];
        let cpu = cpu_columns(mix_seed(99), &chunks, -64, 96, 63);
        let grids = gpu
            .columns(mix_seed(99), &chunks, -64, 96, 63)
            .expect("gpu grids");
        assert_eq!(grids[0].surface, cpu[0].surface);
        assert_eq!(grids[0].caves, cpu[0].caves);
        assert_eq!(grids[1].surface, cpu[1].surface);
        assert_eq!(grids[1].caves, cpu[1].caves);
    }
}
