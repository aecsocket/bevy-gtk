use ash::vk::{self, ExternalMemoryHandleTypeFlags};
use bevy::{
    prelude::*,
    render::{
        camera::{ManualTextureView, ManualTextureViewHandle, ManualTextureViews},
        render_resource::{
            Extent3d, Texture, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
            TextureViewDescriptor,
        },
        renderer::RenderDevice,
        texture::TextureFormatPixelInfo,
    },
};
use wgpu_hal::vulkan;

#[derive(Debug)]
pub struct AdwaitaPlugin {
    pub application_id: String,
}

impl AdwaitaPlugin {
    #[must_use]
    pub const fn window_plugin() -> WindowPlugin {
        WindowPlugin {
            primary_window: None,
            exit_condition: bevy::window::ExitCondition::DontExit,
            close_when_requested: false,
        }
    }
}

impl Plugin for AdwaitaPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreStartup, setup);
    }
}

pub const MANUAL_TEXTURE_VIEW_HANDLE: ManualTextureViewHandle = ManualTextureViewHandle(3861396404);

const DEFAULT_SIZE: UVec2 = UVec2::new(1280, 720);

const TEXTURE_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;

fn setup(mut manual_texture_views: ResMut<ManualTextureViews>, render_device: Res<RenderDevice>) {
    // create the bevy/wgpu texture that we'll be rendering into
    let texture = render_device.create_texture(&TextureDescriptor {
        label: Some("adwaita_render_target"),
        size: Extent3d {
            width: DEFAULT_SIZE.x,
            height: DEFAULT_SIZE.y,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TEXTURE_FORMAT,
        usage: TextureUsages::COPY_SRC
            | TextureUsages::TEXTURE_BINDING
            | TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let texture_view = texture.create_view(&TextureViewDescriptor {
        label: Some("adwaita_render_target_view"),
        ..default()
    });
    let manual_texture_view = ManualTextureView {
        texture_view,
        size: DEFAULT_SIZE,
        format: TEXTURE_FORMAT,
    };

    // cameras will render into `RenderTarget::Manual(MANUAL_TEXTURE_VIEW_HANDLE)`
    // to draw into the Adwaita buffer
    let replaced = manual_texture_views.insert(MANUAL_TEXTURE_VIEW_HANDLE, manual_texture_view);
    assert!(replaced.is_none());

    // export this texture as a dmabuf
    let texture_fd = unsafe {
        render_device
            .wgpu_device()
            .as_hal::<vulkan::Api, _, _>(|device| {
                let device = device.expect("`RenderDevice` should be a vulkan device");
                export_texture_memory(device, &texture)
            })
    }
    .unwrap();

    info!("fd = {texture_fd}");
}

fn export_texture_memory(device: &vulkan::Device, texture: &Texture) -> i32 {
    // get the raw Vulkan Image for this texture
    let image = unsafe {
        texture
            .as_hal::<vulkan::Api, _, _>(|texture| texture.map(|texture| texture.raw_handle()))
            .expect("`texture` should be a vulkan `Image`")
    };

    // allocate the actual memory object that we'll be exporting as dmabuf
    // compute some properties for the allocation first
    let alloc_size = texture.size().width as usize
        * texture.size().height as usize
        * texture.format().pixel_size();
    let memory_properties = unsafe {
        device
            .shared_instance()
            .raw_instance()
            .get_physical_device_memory_properties(device.raw_physical_device())
    };

    let memory_type_index = memory_properties
        .memory_types
        .iter()
        .take(memory_properties.memory_type_count as usize)
        .position(|memory_type| {
            // TODO external memory?
            memory_type
                .property_flags
                .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        })
        .expect("failed to find memory type index for exporting memory");

    let export_info = vk::ExportMemoryAllocateInfo {
        handle_types: vk::ExternalMemoryHandleTypeFlagsKHR::OPAQUE_FD,
        ..default()
    };

    let memory_alloc_info = vk::MemoryAllocateInfo {
        p_next: &export_info as *const _ as *const std::ffi::c_void,
        allocation_size: alloc_size as u64,
        memory_type_index: memory_type_index as u32,
        ..default()
    };

    let memory = unsafe {
        device
            .raw_device()
            .allocate_memory(&memory_alloc_info, None)
    }
    .expect("failed to allocate memory");

    // bind the render image to this allocated memory
    unsafe { device.raw_device().bind_image_memory(image, memory, 0) }
        .expect("failed to bind memory to image");

    // read the fd of this memory object
    let external_memory_fd_ext = ash::extensions::khr::ExternalMemoryFd::new(
        device.shared_instance().raw_instance(),
        device.raw_device(),
    );
    let external_memory_fd = unsafe {
        external_memory_fd_ext.get_memory_fd(&vk::MemoryGetFdInfoKHR {
            memory,
            ..default()
        })
    }
    .expect("failed to get fd for memory");

    external_memory_fd
}
