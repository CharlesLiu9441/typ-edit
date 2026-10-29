//! Runtime capability check for the GPU canvas render pass.
//!
//! The canvas pipelines are built for the host render pass described by
//! [`GpuCanvasConfig`]: a color target with `msaa_samples` samples and a
//! `Stencil8` depth-stencil attachment. An adapter that cannot host that pass
//! would trigger a wgpu validation panic while creating the pipelines, so the
//! host asks [`is_supported`] first and falls back to its raster path when the
//! answer is `false`. The check reads only the adapter's advertised format
//! features and creates no device objects.

use egui_wgpu::RenderState;
use egui_wgpu::wgpu;

use super::GpuCanvasConfig;
use super::resources::STENCIL_FORMAT;

/// Whether a color format advertising `flags` supports an `samples`-sample
/// render target (1 sample always does).
fn msaa_supported(flags: wgpu::TextureFormatFeatureFlags, samples: u32) -> bool {
    match samples {
        1 => true,
        2 => flags.contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X2),
        4 => flags.contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4),
        8 => flags.contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X8),
        _ => false,
    }
}

/// Whether a format whose allowed usages are `usages` can carry the stencil
/// attachment every canvas pipeline's depth-stencil state requires.
fn stencil_supported(usages: wgpu::TextureUsages) -> bool {
    usages.contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
}

/// Whether `render_state`'s adapter can host the render pass `config` describes:
/// `config.target_format` must support `config.msaa_samples` and the canvas
/// stencil format must be usable as a render attachment.
///
/// A `false` result is the host's signal to fall back to raster; it never
/// creates a pipeline, so an unsupported device cannot panic here.
pub fn is_supported(render_state: &RenderState, config: &GpuCanvasConfig) -> bool {
    let color = render_state
        .adapter
        .get_texture_format_features(config.target_format);
    if !msaa_supported(color.flags, config.msaa_samples.max(1)) {
        return false;
    }
    let stencil = render_state
        .adapter
        .get_texture_format_features(STENCIL_FORMAT);
    stencil_supported(stencil.allowed_usages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::TextureFormatFeatureFlags as Flags;

    #[test]
    fn one_sample_is_always_supported() {
        assert!(msaa_supported(Flags::empty(), 1));
    }

    #[test]
    fn each_multisample_flag_gates_only_its_sample_count() {
        assert!(!msaa_supported(Flags::empty(), 2));
        assert!(msaa_supported(Flags::MULTISAMPLE_X2, 2));
        assert!(!msaa_supported(Flags::MULTISAMPLE_X4, 2));
        assert!(!msaa_supported(Flags::MULTISAMPLE_X2, 4));
        assert!(msaa_supported(Flags::MULTISAMPLE_X4, 4));
        assert!(!msaa_supported(Flags::MULTISAMPLE_X4, 8));
        assert!(msaa_supported(Flags::MULTISAMPLE_X8, 8));
    }

    #[test]
    fn unsupported_sample_counts_are_rejected() {
        let all = Flags::MULTISAMPLE_X2 | Flags::MULTISAMPLE_X4 | Flags::MULTISAMPLE_X8;
        assert!(!msaa_supported(all, 0));
        assert!(!msaa_supported(all, 3));
        assert!(!msaa_supported(all, 16));
    }

    #[test]
    fn stencil_requires_render_attachment() {
        assert!(stencil_supported(wgpu::TextureUsages::RENDER_ATTACHMENT));
        assert!(stencil_supported(
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING
        ));
        assert!(!stencil_supported(wgpu::TextureUsages::empty()));
        assert!(!stencil_supported(wgpu::TextureUsages::COPY_DST));
    }
}
