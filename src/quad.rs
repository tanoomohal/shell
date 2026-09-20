//! Instanced solid-color rectangle pipeline.
//!
//! Quads are submitted in two batches per frame: `under` draws beneath the
//! text (cell backgrounds, selection, chrome) and `over` draws on top of it
//! (the cursor), so both share one vertex buffer and one pipeline.

use bytemuck::{Pod, Zeroable};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor,
    BindGroupLayoutEntry, BindingType, BlendState, Buffer, BufferBindingType, BufferDescriptor,
    BufferUsages, ColorTargetState, ColorWrites, Device, FragmentState, MultisampleState,
    PipelineLayoutDescriptor, PrimitiveState, PrimitiveTopology, Queue, RenderPass,
    RenderPipeline, RenderPipelineDescriptor, ShaderModuleDescriptor, ShaderSource, ShaderStages,
    TextureFormat, VertexBufferLayout, VertexState, VertexStepMode, vertex_attr_array,
};

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct Quad {
    pub rect: [f32; 4],
    pub color: [f32; 4],
    /// Corner radius in physical pixels; zero draws a hard-edged rect.
    pub corner_radius: f32,
}

impl Quad {
    /// A hard-edged rect. Correct for anything that tiles, such as cell
    /// backgrounds, where antialiased edges would show seams.
    pub fn new(x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) -> Self {
        Self {
            rect: [x, y, w, h],
            color,
            corner_radius: 0.0,
        }
    }

    pub fn rounded(x: f32, y: f32, w: f32, h: f32, radius: f32, color: [f32; 4]) -> Self {
        Self {
            rect: [x, y, w, h],
            color,
            corner_radius: radius,
        }
    }

    /// A circle, as a rounded rect whose radius is half its size.
    pub fn circle(x: f32, y: f32, diameter: f32, color: [f32; 4]) -> Self {
        Self::rounded(x, y, diameter, diameter, diameter * 0.5, color)
    }
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Params {
    screen: [f32; 2],
    _pad: [f32; 2],
}

pub struct QuadPipeline {
    pipeline: RenderPipeline,
    bind_group: BindGroup,
    params_buf: Buffer,
    instances: Buffer,
    capacity: usize,
    under_count: u32,
    over_count: u32,
    modal_under_count: u32,
    modal_over_count: u32,
}

impl QuadPipeline {
    pub fn new(device: &Device, format: TextureFormat) -> Self {
        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("quad shader"),
            source: ShaderSource::Wgsl(include_str!("quad.wgsl").into()),
        });

        let params_buf = device.create_buffer(&BufferDescriptor {
            label: Some("quad params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bgl = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("quad bind group layout"),
            entries: &[BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::VERTEX,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("quad bind group"),
            layout: &bgl,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: params_buf.as_entire_binding(),
            }],
        });

        let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("quad pipeline layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("quad pipeline"),
            layout: Some(&layout),
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(VertexBufferLayout {
                    array_stride: std::mem::size_of::<Quad>() as u64,
                    step_mode: VertexStepMode::Instance,
                    attributes: &vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32],
                })],
            },
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(ColorTargetState {
                    format,
                    blend: Some(BlendState::ALPHA_BLENDING),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            primitive: PrimitiveState {
                topology: PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            bind_group,
            params_buf,
            instances: Self::alloc(device, 1024),
            capacity: 1024,
            under_count: 0,
            over_count: 0,
            modal_under_count: 0,
            modal_over_count: 0,
        }
    }

    fn alloc(device: &Device, capacity: usize) -> Buffer {
        device.create_buffer(&BufferDescriptor {
            label: Some("quad instances"),
            size: (capacity * std::mem::size_of::<Quad>()) as u64,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn prepare(
        &mut self,
        device: &Device,
        queue: &Queue,
        under: &[Quad],
        over: &[Quad],
        modal_under: &[Quad],
        modal_over: &[Quad],
        screen: [f32; 2],
    ) {
        queue.write_buffer(
            &self.params_buf,
            0,
            bytemuck::bytes_of(&Params {
                screen,
                _pad: [0.0; 2],
            }),
        );

        let needed = under.len() + over.len() + modal_under.len() + modal_over.len();
        if needed > self.capacity {
            self.capacity = needed.next_power_of_two().max(1024);
            self.instances = Self::alloc(device, self.capacity);
        }

        let mut offset_bytes = 0u64;
        let quad_size = std::mem::size_of::<Quad>() as u64;

        if !under.is_empty() {
            queue.write_buffer(&self.instances, offset_bytes, bytemuck::cast_slice(under));
        }
        offset_bytes += under.len() as u64 * quad_size;

        if !over.is_empty() {
            queue.write_buffer(&self.instances, offset_bytes, bytemuck::cast_slice(over));
        }
        offset_bytes += over.len() as u64 * quad_size;

        if !modal_under.is_empty() {
            queue.write_buffer(&self.instances, offset_bytes, bytemuck::cast_slice(modal_under));
        }
        offset_bytes += modal_under.len() as u64 * quad_size;

        if !modal_over.is_empty() {
            queue.write_buffer(&self.instances, offset_bytes, bytemuck::cast_slice(modal_over));
        }

        self.under_count = under.len() as u32;
        self.over_count = over.len() as u32;
        self.modal_under_count = modal_under.len() as u32;
        self.modal_over_count = modal_over.len() as u32;
    }

    pub fn render_under(&self, pass: &mut RenderPass<'_>) {
        self.draw(pass, 0, self.under_count);
    }

    pub fn render_over(&self, pass: &mut RenderPass<'_>) {
        self.draw(pass, self.under_count, self.over_count);
    }

    pub fn render_modal_under(&self, pass: &mut RenderPass<'_>) {
        self.draw(pass, self.under_count + self.over_count, self.modal_under_count);
    }

    pub fn render_modal_over(&self, pass: &mut RenderPass<'_>) {
        self.draw(
            pass,
            self.under_count + self.over_count + self.modal_under_count,
            self.modal_over_count,
        );
    }

    fn draw(&self, pass: &mut RenderPass<'_>, start: u32, count: u32) {
        if count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..4, start..start + count);
    }
}
