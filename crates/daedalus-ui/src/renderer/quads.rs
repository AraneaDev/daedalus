use std::ops::Range;

use bytemuck::{Pod, Zeroable};

use crate::layout::Rect;
use crate::theme::Rgba;

const INITIAL_INSTANCE_CAPACITY: usize = 256;

const QUAD_SHADER_WGSL: &str = r#"
struct ViewportUniform {
    // x = screen_w, y = screen_h, z = is_srgb (1.0 or 0.0), w = unused
    params: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> viewport: ViewportUniform;

struct QuadInput {
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    inst: QuadInput,
) -> VertexOutput {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );

    let corner = corners[vid];
    let px = inst.rect.xy + corner * inst.rect.zw;
    let screen_w = max(viewport.params.x, 1.0);
    let screen_h = max(viewport.params.y, 1.0);
    let ndc_x = (px.x / screen_w) * 2.0 - 1.0;
    let ndc_y = 1.0 - (px.y / screen_h) * 2.0;

    var out: VertexOutput;
    out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.color = inst.color;
    return out;
}

fn srgb_channel_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if viewport.params.z > 0.5 {
        let r = srgb_channel_to_linear(in.color.r);
        let g = srgb_channel_to_linear(in.color.g);
        let b = srgb_channel_to_linear(in.color.b);
        return vec4<f32>(r, g, b, in.color.a);
    }
    return in.color;
}
"#;

/// Converts a single sRGB channel in `[0.0, 1.0]` to linear space.
pub fn srgb_to_linear(c: f32) -> f32 {
    let clamped = c.clamp(0.0, 1.0);
    if clamped <= 0.04045 {
        clamped / 12.92
    } else {
        ((clamped + 0.055) / 1.055).powf(2.4)
    }
}

/// Converts a theme `Rgba` color into a `wgpu::Color` suitable for `LoadOp::Clear`.
pub fn rgba_to_wgpu_clear_color(rgba: Rgba, is_srgb: bool) -> wgpu::Color {
    let [r, g, b, a] = rgba.to_array();
    if is_srgb {
        wgpu::Color {
            r: f64::from(srgb_to_linear(r)),
            g: f64::from(srgb_to_linear(g)),
            b: f64::from(srgb_to_linear(b)),
            a: f64::from(a),
        }
    } else {
        wgpu::Color {
            r: f64::from(r),
            g: f64::from(g),
            b: f64::from(b),
            a: f64::from(a),
        }
    }
}

/// Per-instance GPU payload for a single axis-aligned colored quad in physical pixel coordinates.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Pod, Zeroable)]
pub struct QuadInstance {
    /// `[x, y, width, height]` in physical pixels.
    pub rect: [f32; 4],
    /// Normalized sRGB `[r, g, b, a]` in `[0.0, 1.0]`.
    pub color: [f32; 4],
}

impl QuadInstance {
    pub const fn new(rect: Rect, color: Rgba) -> Self {
        Self {
            rect: [rect.x, rect.y, rect.width, rect.height],
            color: color.to_array(),
        }
    }

    pub const fn from_xywh(x: f32, y: f32, width: f32, height: f32, color: Rgba) -> Self {
        Self {
            rect: [x, y, width, height],
            color: color.to_array(),
        }
    }

    fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![
            0 => Float32x4,
            1 => Float32x4,
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }
}

/// Instanced colored-quad rendering pipeline (Pass 1 of the two-pass renderer).
pub struct QuadPipeline {
    pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instance_count: u32,
    is_srgb: bool,
}

impl QuadPipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("daedalus-quad-shader"),
            source: wgpu::ShaderSource::Wgsl(QUAD_SHADER_WGSL.into()),
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("daedalus-quad-viewport-uniform"),
            size: std::mem::size_of::<[f32; 4]>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("daedalus-quad-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("daedalus-quad-bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("daedalus-quad-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("daedalus-quad-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(QuadInstance::vertex_buffer_layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let instance_buffer = create_instance_buffer(device, INITIAL_INSTANCE_CAPACITY);

        Self {
            pipeline,
            uniform_buffer,
            bind_group,
            instance_buffer,
            instance_capacity: INITIAL_INSTANCE_CAPACITY,
            instance_count: 0,
            is_srgb: format.is_srgb(),
        }
    }

    /// Uploads viewport dimensions and quad instances, growing the GPU instance buffer if needed.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_w: f32,
        screen_h: f32,
        instances: &[QuadInstance],
    ) {
        let uniform: [f32; 4] = [
            screen_w.max(1.0),
            screen_h.max(1.0),
            if self.is_srgb { 1.0 } else { 0.0 },
            0.0,
        ];
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniform));

        if instances.len() > self.instance_capacity {
            let new_cap = instances
                .len()
                .next_power_of_two()
                .max(INITIAL_INSTANCE_CAPACITY);
            self.instance_buffer = create_instance_buffer(device, new_cap);
            self.instance_capacity = new_cap;
        }

        if !instances.is_empty() {
            queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(instances));
        }
        self.instance_count = instances.len() as u32;
    }

    /// Draws all prepared quad instances in the active render pass.
    pub fn render<'a>(&'a self, render_pass: &mut wgpu::RenderPass<'a>) {
        self.render_range(render_pass, 0..self.instance_count);
    }

    /// Draws a sub-range of prepared quad instances in the active render pass.
    pub fn render_range<'a>(&'a self, render_pass: &mut wgpu::RenderPass<'a>, range: Range<u32>) {
        let start = range.start.min(self.instance_count);
        let end = range.end.min(self.instance_count);
        if start >= end {
            return;
        }
        render_pass.set_pipeline(&self.pipeline);
        render_pass.set_bind_group(0, &self.bind_group, &[]);
        render_pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        render_pass.draw(0..6, start..end);
    }
}

fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("daedalus-quad-instances"),
        size: (capacity * std::mem::size_of::<QuadInstance>()) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::BG_WINDOW;

    #[test]
    fn quad_instance_layout_is_32_bytes_and_bytemuck_safe() {
        assert_eq!(std::mem::size_of::<QuadInstance>(), 32);
        let q = QuadInstance::new(Rect::new(10.0, 20.0, 100.0, 40.0), BG_WINDOW);
        let bytes = bytemuck::bytes_of(&q);
        assert_eq!(bytes.len(), 32);
        let decoded: &QuadInstance = bytemuck::from_bytes(bytes);
        assert_eq!(*decoded, q);
    }

    #[test]
    fn srgb_to_linear_matches_standard_transfer_curve() {
        assert_eq!(srgb_to_linear(0.0), 0.0);
        assert_eq!(srgb_to_linear(1.0), 1.0);
        assert!((srgb_to_linear(0.5) - 0.21404114).abs() < 1e-5);
        assert!((srgb_to_linear(0.04) - (0.04 / 12.92)).abs() < 1e-6);

        let clear = rgba_to_wgpu_clear_color(BG_WINDOW, true);
        assert!(clear.r < f64::from(BG_WINDOW.0[0]));
        assert_eq!(clear.a, 1.0);
    }

    #[test]
    fn pipeline_reuses_grown_instance_buffer_and_accepts_clipped_draw_ranges() {
        let (device, queue) = wgpu::Device::noop(&Default::default());
        for format in [
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::TextureFormat::Bgra8Unorm,
        ] {
            let mut pipeline = QuadPipeline::new(&device, format);
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: 16,
                    height: 16,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let instances = vec![
                QuadInstance::new(Rect::new(0., 0., 4., 4.), BG_WINDOW);
                INITIAL_INSTANCE_CAPACITY + 1
            ];
            for count in [instances.len(), 1, 0] {
                pipeline.prepare(&device, &queue, 16., 16., &instances[..count]);
                assert_eq!(pipeline.instance_count as usize, count);
                assert_eq!(pipeline.instance_capacity, INITIAL_INSTANCE_CAPACITY * 2);
                assert_eq!(
                    pipeline.instance_buffer.size() as usize,
                    pipeline.instance_capacity * std::mem::size_of::<QuadInstance>()
                );
                let mut encoder = device.create_command_encoder(&Default::default());
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: Default::default(),
                        })],
                        ..Default::default()
                    });
                    pipeline.render(&mut pass);
                    pipeline.render_range(&mut pass, 1..u32::MAX);
                    pipeline.render_range(&mut pass, u32::MAX..u32::MAX);
                }
                queue.submit([encoder.finish()]);
            }
        }
    }
}
