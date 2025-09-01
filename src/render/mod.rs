use {bevy_app::prelude::*, bevy_render::renderer::raw_vulkan_init::RawVulkanInitSettings};

mod dmabuf;
pub use dmabuf::*;

pub(crate) fn build_app(app: &mut App) {
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
