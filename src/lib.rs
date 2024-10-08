mod vk_custom;

use std::sync::Arc;

use ash::vk::{self, ExternalMemoryHandleTypeFlags};
use bevy::{
    prelude::*,
    render::{
        camera::{ManualTextureView, ManualTextureViewHandle, ManualTextureViews},
        render_resource::{
            Extent3d, Texture, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
            TextureViewDescriptor,
        },
        renderer::{
            RenderAdapter, RenderAdapterInfo, RenderDevice, RenderInstance, RenderQueue,
            WgpuWrapper,
        },
        settings::{RenderCreation, WgpuSettings},
        texture::TextureFormatPixelInfo,
        RenderPlugin,
    },
    window::ExitCondition,
};
use wgpu::DeviceDescriptor;
use wgpu_hal::{vulkan, Instance, OpenDevice};

#[derive(Debug)]
pub struct AdwaitaPlugin {
    pub application_id: String,
}

impl Plugin for AdwaitaPlugin {
    fn build(&self, app: &mut App) {
        // app.add_systems(PreStartup, setup);
    }
}

impl AdwaitaPlugin {
    #[must_use]
    pub const fn window_plugin() -> WindowPlugin {
        WindowPlugin {
            primary_window: None,
            exit_condition: ExitCondition::DontExit,
            close_when_requested: false,
        }
    }

    pub fn render_plugin() -> RenderPlugin {
        let render_creation = create_renderer();
        RenderPlugin {
            render_creation,
            synchronous_pipeline_compilation: false,
        }
    }
}

fn create_renderer() -> RenderCreation {
    let settings = WgpuSettings::default();

    let do_async = async move {
        let instance = unsafe {
            vulkan::Instance::init(&wgpu_hal::InstanceDescriptor {
                name: "bevy_mod_adwaita", // app name
                flags: settings.instance_flags,
                dx12_shader_compiler: settings.dx12_shader_compiler.clone(),
                gles_minor_version: settings.gles3_minor_version,
            })
        }
        .expect("failed to create vulkan instance");

        // validation works
        let instance = unsafe { wgpu::Instance::from_hal::<vulkan::Api>(instance) };
        let (device, queue, adapter_info, adapter) = bevy::render::renderer::initialize_renderer(
            &instance,
            &settings,
            &wgpu::RequestAdapterOptions {
                power_preference: settings.power_preference,
                compatible_surface: None,
                ..default()
            },
        )
        .await;

        // validation fails
        // let adapter = unsafe { instance.enumerate_adapters() }
        //     .into_iter()
        //     .next()
        //     .expect("no adapters");
        // let device = unsafe {
        //     vk_custom::open_adapter(
        //         &adapter.adapter,
        //         settings.features.clone(),
        //         [],
        //         // [ash::extensions::khr::ExternalMemoryFd::name()],
        //     )
        //     .expect("failed to open device")
        // };
        // let instance = unsafe { wgpu::Instance::from_hal::<vulkan::Api>(instance) };
        // let adapter = unsafe { instance.create_adapter_from_hal(adapter) };
        // let adapter_info = adapter.get_info();
        // let device_descriptor =
        //     vk_custom::make_device_descriptor(&settings, &adapter, &adapter_info);
        // let (device, queue) =
        //     unsafe { adapter.create_device_from_hal(device, &device_descriptor, None) }
        //         .expect("failed to create device");
        // let device = RenderDevice::from(device),
        // let queue = RenderQueue(Arc::new(WgpuWrapper::new(queue))),
        // let adapter_info = RenderAdapterInfo(WgpuWrapper::new(adapter_info)),
        // let adapter = RenderAdapter(Arc::new(WgpuWrapper::new(adapter))),

        RenderCreation::Manual(
            device,
            queue,
            adapter_info,
            adapter,
            RenderInstance(Arc::new(WgpuWrapper::new(instance))),
        )
    };

    futures_lite::future::block_on(do_async)
}

pub const MANUAL_TEXTURE_VIEW_HANDLE: ManualTextureViewHandle = ManualTextureViewHandle(3861396404);

const DEFAULT_SIZE: UVec2 = UVec2::new(1280, 720);

const TEXTURE_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;

fn setup(mut manual_texture_views: ResMut<ManualTextureViews>, render_device: Res<RenderDevice>) {
    unsafe {
        render_device
            .wgpu_device()
            .as_hal::<vulkan::Api, _, _>(|wgpu_device| {
                let wgpu_device = wgpu_device.expect("`RenderDevice` is not a vulkan device");
                let vk_device = wgpu_device.raw_device();
                let phys_device = wgpu_device.raw_physical_device();
                let instance = wgpu_device.shared_instance().raw_instance();

                let export_info = vk::ExportMemoryAllocateInfo {
                    handle_types: vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                    ..default()
                };

                let alloc_info = vk::MemoryAllocateInfo {
                    p_next: &export_info as *const _ as *const std::ffi::c_void,
                    allocation_size: 1024,
                    ..default()
                };

                let memory = unsafe { vk_device.allocate_memory(&alloc_info, None) }
                    .expect("failed to allocate memory");

                let get_memory_info = vk::MemoryGetFdInfoKHR {
                    memory,
                    handle_type: vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                    ..default()
                };
                let fd = unsafe {
                    ash::extensions::khr::ExternalMemoryFd::new(instance, vk_device)
                        .get_memory_fd(&get_memory_info)
                }
                .expect("failed to get fd for allocated memory");

                info!("fd = {fd}");

                //                 let image = vk_device.create_image(&vk::ImageCreateInfo {
                //                     image_type: vk::ImageType::TYPE_2D,
                // format: vk::Format::R8G8B8A8_SRGB,
                // extent: vk::Extent3D {
                //     width: DEFAULT_SIZE.x,
                //     height: DEFAULT_SIZE.y,
                //     depth: 1,
                // },
                // mip_levels: 1,
                // array_layers: 1,
                // samples: vk::SampleCountFlags::TYPE_1,
                // tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT
                //                 }, None);
            });
    }
}

// fn setup(mut manual_texture_views: ResMut<ManualTextureViews>, render_device: Res<RenderDevice>) {
//     // create the bevy/wgpu texture that we'll be rendering into
//     let texture = render_device.create_texture(&TextureDescriptor {
//         label: Some("adwaita_render_target"),
//         size: Extent3d {
//             width: DEFAULT_SIZE.x,
//             height: DEFAULT_SIZE.y,
//             depth_or_array_layers: 1,
//         },
//         mip_level_count: 1,
//         sample_count: 1,
//         dimension: TextureDimension::D2,
//         format: TEXTURE_FORMAT,
//         usage: TextureUsages::COPY_SRC
//             | TextureUsages::TEXTURE_BINDING
//             | TextureUsages::RENDER_ATTACHMENT,
//         view_formats: &[],
//     });
//     let texture_view = texture.create_view(&TextureViewDescriptor {
//         label: Some("adwaita_render_target_view"),
//         ..default()
//     });
//     let manual_texture_view = ManualTextureView {
//         texture_view,
//         size: DEFAULT_SIZE,
//         format: TEXTURE_FORMAT,
//     };

//     // cameras will render into `RenderTarget::Manual(MANUAL_TEXTURE_VIEW_HANDLE)`
//     // to draw into the Adwaita buffer
//     let replaced = manual_texture_views.insert(MANUAL_TEXTURE_VIEW_HANDLE, manual_texture_view);
//     assert!(replaced.is_none());

//     // export this texture as a dmabuf
//     let texture_fd = unsafe {
//         render_device
//             .wgpu_device()
//             .as_hal::<vulkan::Api, _, _>(|device| {
//                 let device = device.expect("`RenderDevice` should be a vulkan device");
//                 export_texture_memory(device, &texture)
//             })
//     }
//     .unwrap();

//     info!("fd = {texture_fd}");
// }

// fn export_texture_memory(device: &vulkan::Device, texture: &Texture) -> i32 {
//     // get the raw Vulkan Image for this texture
//     let image = unsafe {
//         texture
//             .as_hal::<vulkan::Api, _, _>(|texture| texture.map(|texture| texture.raw_handle()))
//             .expect("`texture` should be a vulkan `Image`")
//     };

//     // allocate the actual memory object that we'll be exporting as dmabuf
//     // compute some properties for the allocation first
//     let alloc_size = texture.size().width as usize
//         * texture.size().height as usize
//         * texture.format().pixel_size();
//     let memory_properties = unsafe {
//         device
//             .shared_instance()
//             .raw_instance()
//             .get_physical_device_memory_properties(device.raw_physical_device())
//     };

//     let memory_type_index = memory_properties
//         .memory_types
//         .iter()
//         .take(memory_properties.memory_type_count as usize)
//         .position(|memory_type| {
//             // TODO external memory?
//             memory_type
//                 .property_flags
//                 .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
//         })
//         .expect("failed to find memory type index for exporting memory");

//     let export_info = vk::ExportMemoryAllocateInfo {
//         handle_types: vk::ExternalMemoryHandleTypeFlagsKHR::OPAQUE_FD,
//         ..default()
//     };

//     let memory_alloc_info = vk::MemoryAllocateInfo {
//         p_next: &export_info as *const _ as *const std::ffi::c_void,
//         allocation_size: alloc_size as u64,
//         memory_type_index: memory_type_index as u32,
//         ..default()
//     };

//     let memory = unsafe {
//         device
//             .raw_device()
//             .allocate_memory(&memory_alloc_info, None)
//     }
//     .expect("failed to allocate memory");

//     // bind the render image to this allocated memory
//     unsafe { device.raw_device().bind_image_memory(image, memory, 0) }
//         .expect("failed to bind memory to image");

//     // read the fd of this memory object
//     let external_memory_fd_ext = ash::extensions::khr::ExternalMemoryFd::new(
//         device.shared_instance().raw_instance(),
//         device.raw_device(),
//     );
//     let external_memory_fd = unsafe {
//         external_memory_fd_ext.get_memory_fd(&vk::MemoryGetFdInfoKHR {
//             memory,
//             ..default()
//         })
//     }
//     .expect("failed to get fd for memory");

//     external_memory_fd
// }
