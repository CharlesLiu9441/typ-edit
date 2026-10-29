use egui_wgpu::wgpu;

pub(crate) struct PremultiplyPipeline {
    pub(crate) layout: wgpu::BindGroupLayout,
    pub(crate) pipeline: wgpu::ComputePipeline,
}

pub(crate) fn create_pipeline(device: &wgpu::Device) -> PremultiplyPipeline {
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("typst-gpu-canvas premultiply layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("typst-gpu-canvas premultiply shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/premultiply.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("typst-gpu-canvas premultiply pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("typst-gpu-canvas premultiply pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    PremultiplyPipeline { layout, pipeline }
}

pub(crate) fn encode(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    buffer: &wgpu::Buffer,
    pixel_count: u32,
    premultiply: &PremultiplyPipeline,
) {
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("typst-gpu-canvas premultiply bind group"),
        layout: &premultiply.layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("typst-gpu-canvas premultiply pass"),
        timestamp_writes: None,
    });
    pass.set_pipeline(&premultiply.pipeline);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.dispatch_workgroups(pixel_count.div_ceil(64), 1, 1);
}
