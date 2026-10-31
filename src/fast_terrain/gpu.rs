//! Vulkan dispatch of the fast-terrain WGSL shader.
//!
//! naga compiles the shader to SPIR-V. The wgpu crate is not used: its Windows
//! hal's raw-dylib import libraries do not link on the GNU toolchain here.
use super::ColumnGrids;
use ash::vk;

#[derive(Debug)]
pub(super) enum GpuError {
    NoAdapter(String),
    Failed(String),
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAdapter(reason) | Self::Failed(reason) => f.write_str(reason),
        }
    }
}

pub(super) struct Gpu {
    entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    physical: vk::PhysicalDevice,
    queue: vk::Queue,
    shader: vk::ShaderModule,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    descriptors: vk::DescriptorPool,
    commands: vk::CommandPool,
    command: vk::CommandBuffer,
}

impl Gpu {
    pub(super) fn new() -> Result<Self, GpuError> {
        let entry = unsafe { ash::Entry::load() }
            .map_err(|e| GpuError::NoAdapter(format!("vulkan: {e}")))?;
        let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_0);
        let instance = unsafe {
            entry
                .create_instance(
                    &vk::InstanceCreateInfo::default().application_info(&app),
                    None,
                )
                .map_err(|e| GpuError::NoAdapter(format!("vulkan instance: {e}")))?
        };
        let physicals = unsafe { instance.enumerate_physical_devices() }
            .map_err(|e| GpuError::NoAdapter(format!("vulkan devices: {e}")))?;
        let physical = physicals
            .into_iter()
            .find(|device| compute_family(&instance, *device).is_some())
            .ok_or_else(|| GpuError::NoAdapter("no Vulkan compute queue".into()))?;
        let family = compute_family(&instance, physical).expect("checked");
        let priority = [1.0f32];
        let queue_info = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(family)
            .queue_priorities(&priority);
        let device_info =
            vk::DeviceCreateInfo::default().queue_create_infos(std::slice::from_ref(&queue_info));
        let device = unsafe { instance.create_device(physical, &device_info, None) }
            .map_err(|e| GpuError::NoAdapter(format!("vulkan device: {e}")))?;
        let queue = unsafe { device.get_device_queue(family, 0) };
        let words = spirv()?;
        let shader = unsafe {
            device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
                .map_err(|e| GpuError::Failed(format!("shader module: {e}")))?
        };
        let bindings = [
            binding(0, vk::DescriptorType::UNIFORM_BUFFER),
            binding(1, vk::DescriptorType::STORAGE_BUFFER),
            binding(2, vk::DescriptorType::STORAGE_BUFFER),
            binding(3, vk::DescriptorType::STORAGE_BUFFER),
        ];
        let set_layout = unsafe {
            device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                    None,
                )
                .map_err(|e| GpuError::Failed(format!("descriptor layout: {e}")))?
        };
        let layout = unsafe {
            device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default()
                        .set_layouts(std::slice::from_ref(&set_layout)),
                    None,
                )
                .map_err(|e| GpuError::Failed(format!("pipeline layout: {e}")))?
        };
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(shader)
            .name(c"main");
        let create = vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(layout);
        let pipeline = match unsafe {
            device.create_compute_pipelines(vk::PipelineCache::null(), &[create], None)
        } {
            Ok(mut pipes) => pipes.pop().expect("one pipeline"),
            Err((_, err)) => return Err(GpuError::Failed(format!("pipeline: {err}"))),
        };
        let pool_sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: 1,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_BUFFER,
                descriptor_count: 3,
            },
        ];
        let descriptors = unsafe {
            device
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .max_sets(1)
                        .pool_sizes(&pool_sizes),
                    None,
                )
                .map_err(|e| GpuError::Failed(format!("descriptor pool: {e}")))?
        };
        let commands = unsafe {
            device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default().queue_family_index(family),
                    None,
                )
                .map_err(|e| GpuError::Failed(format!("command pool: {e}")))?
        };
        let command = unsafe {
            device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(commands)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(|e| GpuError::Failed(format!("command buffer: {e}")))?
        }[0];
        Ok(Self {
            entry,
            instance,
            device,
            physical,
            queue,
            shader,
            set_layout,
            layout,
            pipeline,
            descriptors,
            commands,
            command,
        })
    }

    pub(super) fn columns(
        &self,
        seed: u32,
        chunks: &[(i32, i32)],
        min_y: i32,
        height: i32,
        sea: i32,
    ) -> Result<Vec<ColumnGrids>, GpuError> {
        let count = chunks.len() as u32;
        let words = (height as u32).div_ceil(32);
        let params = super::params_bytes(seed, min_y, height, sea, count);
        let mut coords = Vec::with_capacity(chunks.len() * 2);
        for &(x, z) in chunks {
            coords.push(x);
            coords.push(z);
        }
        let params_buf = host_buffer(
            &self.instance,
            &self.device,
            self.physical,
            &params,
            vk::BufferUsageFlags::UNIFORM_BUFFER,
        )?;
        let coord_bytes = unsafe {
            std::slice::from_raw_parts(
                coords.as_ptr().cast::<u8>(),
                std::mem::size_of_val(coords.as_slice()),
            )
        };
        let chunks_buf = host_buffer(
            &self.instance,
            &self.device,
            self.physical,
            coord_bytes,
            vk::BufferUsageFlags::STORAGE_BUFFER,
        )?;
        let height_bytes = vec![0u8; count as usize * 256 * 4];
        let cave_bytes = vec![0u8; count as usize * 256 * words as usize * 4];
        let heights = host_buffer(
            &self.instance,
            &self.device,
            self.physical,
            &height_bytes,
            vk::BufferUsageFlags::STORAGE_BUFFER,
        )?;
        let caves = host_buffer(
            &self.instance,
            &self.device,
            self.physical,
            &cave_bytes,
            vk::BufferUsageFlags::STORAGE_BUFFER,
        )?;
        unsafe {
            self.device
                .reset_descriptor_pool(self.descriptors, vk::DescriptorPoolResetFlags::empty())
                .map_err(|e| GpuError::Failed(format!("reset descriptors: {e}")))?;
            self.device
                .reset_command_pool(self.commands, vk::CommandPoolResetFlags::empty())
                .map_err(|e| GpuError::Failed(format!("reset commands: {e}")))?;
        }
        let set = unsafe {
            self.device
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(self.descriptors)
                        .set_layouts(std::slice::from_ref(&self.set_layout)),
                )
                .map_err(|e| GpuError::Failed(format!("descriptor set: {e}")))?
        }[0];
        let infos = [
            buffer_info(&params_buf),
            buffer_info(&chunks_buf),
            buffer_info(&heights),
            buffer_info(&caves),
        ];
        let writes = [
            write_set(set, 0, vk::DescriptorType::UNIFORM_BUFFER, &infos[0..1]),
            write_set(set, 1, vk::DescriptorType::STORAGE_BUFFER, &infos[1..2]),
            write_set(set, 2, vk::DescriptorType::STORAGE_BUFFER, &infos[2..3]),
            write_set(set, 3, vk::DescriptorType::STORAGE_BUFFER, &infos[3..4]),
        ];
        unsafe { self.device.update_descriptor_sets(&writes, &[]) };
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe {
            self.device
                .begin_command_buffer(self.command, &begin)
                .map_err(|e| GpuError::Failed(format!("begin: {e}")))?;
            self.device.cmd_bind_pipeline(
                self.command,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline,
            );
            self.device.cmd_bind_descriptor_sets(
                self.command,
                vk::PipelineBindPoint::COMPUTE,
                self.layout,
                0,
                &[set],
                &[],
            );
            let groups = count * 256 / 64 + u32::from(count * 256 % 64 != 0);
            self.device.cmd_dispatch(self.command, groups, 1, 1);
            self.device
                .end_command_buffer(self.command)
                .map_err(|e| GpuError::Failed(format!("end: {e}")))?;
            let buffers = [self.command];
            self.device
                .queue_submit(
                    self.queue,
                    &[vk::SubmitInfo::default().command_buffers(&buffers)],
                    vk::Fence::null(),
                )
                .map_err(|e| GpuError::Failed(format!("submit: {e}")))?;
            self.device
                .queue_wait_idle(self.queue)
                .map_err(|e| GpuError::Failed(format!("wait: {e}")))?;
        }
        let height_read = heights.bytes();
        let cave_read = caves.bytes();
        let height_words: Vec<i32> = height_read
            .chunks_exact(4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let cave_words: Vec<u32> = cave_read
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let mut out = Vec::with_capacity(chunks.len());
        for chunk_i in 0..chunks.len() {
            let mut surface = [0; 256];
            surface.copy_from_slice(&height_words[chunk_i * 256..(chunk_i + 1) * 256]);
            let mut cave_mask = vec![0u8; height as usize * 256];
            for column in 0..256 {
                let base = (chunk_i * 256 + column) * words as usize;
                for y in 0..height as usize {
                    let word = cave_words[base + y / 32];
                    if word & (1 << (y % 32)) != 0 {
                        cave_mask[y * 256 + column] = 1;
                    }
                }
            }
            out.push(ColumnGrids {
                surface,
                caves: cave_mask,
            });
        }
        Ok(out)
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_command_pool(self.commands, None);
            self.device.destroy_descriptor_pool(self.descriptors, None);
            self.device.destroy_pipeline(self.pipeline, None);
            self.device.destroy_pipeline_layout(self.layout, None);
            self.device
                .destroy_descriptor_set_layout(self.set_layout, None);
            self.device.destroy_shader_module(self.shader, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
        let _ = &self.entry;
    }
}

fn compute_family(instance: &ash::Instance, device: vk::PhysicalDevice) -> Option<u32> {
    let families = unsafe { instance.get_physical_device_queue_family_properties(device) };
    families
        .iter()
        .position(|family| {
            family.queue_flags.contains(vk::QueueFlags::COMPUTE) && family.queue_count > 0
        })
        .map(|index| index as u32)
}

fn binding(binding: u32, ty: vk::DescriptorType) -> vk::DescriptorSetLayoutBinding<'static> {
    vk::DescriptorSetLayoutBinding::default()
        .binding(binding)
        .descriptor_type(ty)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::COMPUTE)
}

fn spirv() -> Result<Vec<u32>, GpuError> {
    let module = naga::front::wgsl::parse_str(SHADER)
        .map_err(|e| GpuError::Failed(format!("shader: {e:?}")))?;
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    );
    let info = validator
        .validate(&module)
        .map_err(|e| GpuError::Failed(format!("shader: {e:?}")))?;
    naga::back::spv::write_vec(
        &module,
        &info,
        &naga::back::spv::Options {
            lang_version: (1, 0),
            ..Default::default()
        },
        None,
    )
    .map_err(|e| GpuError::Failed(format!("spirv: {e:?}")))
}

struct HostBuffer {
    device: ash::Device,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    ptr: *mut u8,
    size: u64,
}

impl HostBuffer {
    fn bytes(&self) -> Vec<u8> {
        unsafe { std::slice::from_raw_parts(self.ptr, self.size as usize).to_vec() }
    }
}

impl Drop for HostBuffer {
    fn drop(&mut self) {
        unsafe {
            if !self.ptr.is_null() {
                self.device.unmap_memory(self.memory);
                self.ptr = std::ptr::null_mut();
            }
            self.device.destroy_buffer(self.buffer, None);
            self.device.free_memory(self.memory, None);
        }
    }
}

fn host_buffer(
    instance: &ash::Instance,
    device: &ash::Device,
    physical: vk::PhysicalDevice,
    bytes: &[u8],
    usage: vk::BufferUsageFlags,
) -> Result<HostBuffer, GpuError> {
    let size = bytes.len() as u64;
    let buffer = unsafe {
        device
            .create_buffer(
                &vk::BufferCreateInfo::default().size(size).usage(usage),
                None,
            )
            .map_err(|e| GpuError::Failed(format!("buffer: {e}")))?
    };
    let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
    let properties = unsafe { instance.get_physical_device_memory_properties(physical) };
    let index = (0..properties.memory_type_count).find(|i| {
        let bit = 1u32 << i;
        requirements.memory_type_bits & bit != 0
            && properties.memory_types[*i as usize]
                .property_flags
                .contains(
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
    });
    let Some(index) = index else {
        unsafe { device.destroy_buffer(buffer, None) };
        return Err(GpuError::NoAdapter(
            "Vulkan device has no host-visible memory".into(),
        ));
    };
    let memory = match unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(index),
            None,
        )
    } {
        Ok(memory) => memory,
        Err(e) => {
            unsafe { device.destroy_buffer(buffer, None) };
            return Err(GpuError::Failed(format!("memory: {e}")));
        }
    };
    if let Err(e) = unsafe { device.bind_buffer_memory(buffer, memory, 0) } {
        unsafe {
            device.free_memory(memory, None);
            device.destroy_buffer(buffer, None);
        }
        return Err(GpuError::Failed(format!("bind: {e}")));
    }
    let ptr = match unsafe {
        device.map_memory(memory, 0, requirements.size, vk::MemoryMapFlags::empty())
    } {
        Ok(ptr) => ptr.cast::<u8>(),
        Err(e) => {
            unsafe {
                device.free_memory(memory, None);
                device.destroy_buffer(buffer, None);
            }
            return Err(GpuError::Failed(format!("map: {e}")));
        }
    };
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
    }
    Ok(HostBuffer {
        device: device.clone(),
        buffer,
        memory,
        ptr,
        size,
    })
}

fn buffer_info(buffer: &HostBuffer) -> vk::DescriptorBufferInfo {
    vk::DescriptorBufferInfo::default()
        .buffer(buffer.buffer)
        .offset(0)
        .range(buffer.size)
}

fn write_set(
    set: vk::DescriptorSet,
    binding: u32,
    ty: vk::DescriptorType,
    info: &[vk::DescriptorBufferInfo],
) -> vk::WriteDescriptorSet<'_> {
    vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(ty)
        .buffer_info(info)
}

const SHADER: &str = r#"
struct Params {
    seed: u32,
    min_y: i32,
    height: i32,
    sea: i32,
    count: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> chunks: array<vec2<i32>>;
@group(0) @binding(2) var<storage, read_write> heights: array<i32>;
@group(0) @binding(3) var<storage, read_write> caves: array<u32>;

fn hash01(x: i32, y: i32, z: i32, seed: u32) -> f32 {
    let n0 = bitcast<u32>(x) * 374761393u;
    let n1 = bitcast<u32>(y) * 668265263u;
    let n2 = bitcast<u32>(z) * 2146121005u;
    var n = n0 ^ n1 ^ n2 ^ seed;
    n = n * (n * 1274126177u + 1u);
    return f32((n >> 9u) & 0x7fffffu) / 8388607.0;
}

fn floor_i(v: f32) -> i32 {
    let t = i32(v);
    if (v < f32(t)) { return t - 1; }
    return t;
}

fn round_i(v: f32) -> i32 {
    return floor_i(v + 0.5);
}

fn fade(t: f32) -> f32 {
    return t * t * (3.0 - 2.0 * t);
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    return a + (b - a) * t;
}

fn value2(x: f32, z: f32, seed: u32) -> f32 {
    let x0 = floor_i(x);
    let z0 = floor_i(z);
    let fx = fade(x - f32(x0));
    let fz = fade(z - f32(z0));
    let v00 = hash01(x0, 0, z0, seed);
    let v10 = hash01(x0 + 1, 0, z0, seed);
    let v01 = hash01(x0, 0, z0 + 1, seed);
    let v11 = hash01(x0 + 1, 0, z0 + 1, seed);
    return lerp(lerp(v00, v10, fx), lerp(v01, v11, fx), fz);
}

fn value3(x: f32, y: f32, z: f32, seed: u32) -> f32 {
    let x0 = floor_i(x);
    let y0 = floor_i(y);
    let z0 = floor_i(z);
    let fx = fade(x - f32(x0));
    let fy = fade(y - f32(y0));
    let fz = fade(z - f32(z0));
    let s000 = hash01(x0, y0, z0, seed);
    let s100 = hash01(x0 + 1, y0, z0, seed);
    let s001 = hash01(x0, y0, z0 + 1, seed);
    let s101 = hash01(x0 + 1, y0, z0 + 1, seed);
    let s010 = hash01(x0, y0 + 1, z0, seed);
    let s110 = hash01(x0 + 1, y0 + 1, z0, seed);
    let s011 = hash01(x0, y0 + 1, z0 + 1, seed);
    let s111 = hash01(x0 + 1, y0 + 1, z0 + 1, seed);
    let x00 = lerp(s000, s100, fx);
    let x10 = lerp(s001, s101, fx);
    let x01 = lerp(s010, s110, fx);
    let x11 = lerp(s011, s111, fx);
    return lerp(lerp(x00, x10, fz), lerp(x01, x11, fz), fy);
}

fn surface_y(wx: i32, wz: i32, seed: u32, sea: i32, min_y: i32, max_y: i32) -> i32 {
    let x = f32(wx);
    let z = f32(wz);
    let continent = value2(x / 96.0, z / 96.0, seed);
    let hills = value2(x / 24.0, z / 24.0, seed ^ 0x9e3779b9u);
    let detail = value2(x / 8.0, z / 8.0, seed ^ 0x85ebca6bu);
    var base: f32;
    if (continent < 0.45) {
        base = f32(sea) - 8.0 - (0.45 - continent) * 40.0;
    } else {
        base = f32(sea) + (continent - 0.45) * 24.0;
    }
    let h = base + (hills - 0.5) * 16.0 + (detail - 0.5) * 6.0;
    return clamp(round_i(h), min_y, max_y);
}

fn is_cave(wx: i32, y: i32, wz: i32, surface: i32, min_y: i32, seed: u32) -> bool {
    if (y <= min_y + 4 || y >= surface - 4) { return false; }
    return value3(f32(wx) / 10.0, f32(y) / 8.0, f32(wz) / 10.0, seed ^ 0xc2b2ae35u) < 0.38;
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if (i >= params.count * 256u) { return; }
    let chunk_i = i / 256u;
    let col = i % 256u;
    let lx = i32(col % 16u);
    let lz = i32(col / 16u);
    let wx = chunks[chunk_i].x * 16 + lx;
    let wz = chunks[chunk_i].y * 16 + lz;
    let top = surface_y(wx, wz, params.seed, params.sea, params.min_y, params.min_y + params.height - 1);
    heights[i] = top;
    let words = (u32(params.height) + 31u) / 32u;
    let base = i * words;
    for (var w = 0u; w < words; w++) {
        caves[base + w] = 0u;
    }
    for (var y = 0; y < params.height; y++) {
        let world_y = params.min_y + y;
        if (is_cave(wx, world_y, wz, top, params.min_y, params.seed)) {
            let yi = u32(y);
            caves[base + yi / 32u] |= 1u << (yi % 32u);
        }
    }
}
"#;
