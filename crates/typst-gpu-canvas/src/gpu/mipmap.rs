use egui_wgpu::wgpu;

const IMAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub(crate) fn create_pipeline(
    device: &wgpu::Device,
    image_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("typst-gpu-canvas mipmap shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/mipmap.wgsl").into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("typst-gpu-canvas mipmap layout"),
        bind_group_layouts: &[Some(image_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("typst-gpu-canvas mipmap pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: IMAGE_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn generate(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    mip_levels: u32,
    pipeline: &wgpu::RenderPipeline,
    sampler: &wgpu::Sampler,
    image_layout: &wgpu::BindGroupLayout,
) {
    if mip_levels <= 1 {
        return;
    }

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("typst-gpu-canvas mipmap encoder"),
    });
    for level in 1..mip_levels {
        let source_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("typst-gpu-canvas mip source"),
            base_mip_level: level - 1,
            mip_level_count: Some(1),
            ..Default::default()
        });
        let target_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("typst-gpu-canvas mip target"),
            base_mip_level: level,
            mip_level_count: Some(1),
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("typst-gpu-canvas mip bind group"),
            layout: image_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("typst-gpu-canvas mip pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    queue.submit(Some(encoder.finish()));
}
