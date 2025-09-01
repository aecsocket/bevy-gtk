use {
    bevy_app::prelude::*,
    bevy_ecs::prelude::*,
    bevy_render::renderer::raw_vulkan_init::RawVulkanInitSettings,
    drm_fourcc::{DrmFormat, DrmFourcc, DrmModifier},
    gdk::prelude::*,
    log::trace,
};

mod dmabuf;
pub use dmabuf::*;

pub struct GtkRenderPlugin;

impl Plugin for GtkRenderPlugin {
    fn build(&self, app: &mut App) {
        let mut raw_vulkan_settings = app
            .world_mut()
            .get_resource_or_init::<RawVulkanInitSettings>();

        // SAFETY: we do not remove any features or functionality
        unsafe {
            raw_vulkan_settings.add_create_device_callback(|args, _, _| {
                args.extensions.extend_from_slice(&[
                    ash::khr::external_memory::NAME,
                    ash::khr::external_memory_fd::NAME,
                    ash::ext::image_drm_format_modifier::NAME,
                    ash::ext::external_memory_dma_buf::NAME,
                ]);
            });
        }
    }
}

#[derive(Debug, Resource)]
pub struct GtkRenderData {
    dmabuf_formats: Vec<DrmFormat>,
}

impl GtkRenderData {
    #[must_use]
    pub fn dmabuf_formats(&self) -> &[DrmFormat] {
        &self.dmabuf_formats
    }
}

pub(crate) fn post_activate(app: &mut App) {
    let dmabuf_formats = gdk::Display::default()
        .expect("failed to get GDK display")
        .dmabuf_formats();
    let dmabuf_formats = (0..dmabuf_formats.n_formats())
        .filter_map(|i| {
            let (code, modifier) = dmabuf_formats.format(i);
            let modifier = DrmModifier::from(modifier);
            DrmFourcc::try_from(code)
                .map(|code| DrmFormat { code, modifier })
                .inspect_err(|err| {
                    trace!(
                        "dmabuf format ({}, {modifier:?}) has unknown fourcc",
                        err.display()
                            .map_or_else(|| format!("{code}"), |s| s.to_string(),)
                    );
                })
                .ok()
        })
        .collect::<Vec<_>>();

    trace!("Supported dmabuf formats:");
    for format in &dmabuf_formats {
        trace!("- {format:?}");
    }

    app.insert_resource(GtkRenderData { dmabuf_formats });
}
