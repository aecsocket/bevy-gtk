use std::{
    fs::File,
    os::{fd::FromRawFd, raw::c_void},
    sync::Arc,
};

use ash::vk;
use bevy::{
    prelude::*,
    render::{
        camera::ManualTextureView,
        renderer::{
            RenderAdapter, RenderAdapterInfo, RenderDevice, RenderInstance, RenderQueue,
            WgpuWrapper,
        },
        settings::{RenderCreation, WgpuSettings},
        RenderPlugin,
    },
};
use gtk::gdk;
use wgpu::TextureFormat;
use wgpu_hal::{vulkan, Instance};

use crate::{hal_custom, AdwaitaPlugin, DmabufInfo};

impl AdwaitaPlugin {
    #[must_use]
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
        // let instance = unsafe { wgpu::Instance::from_hal::<vulkan::Api>(instance) };
        // let (device, queue, adapter_info, adapter) = bevy::render::renderer::initialize_renderer(
        //     &instance,
        //     &settings,
        //     &wgpu::RequestAdapterOptions {
        //         power_preference: settings.power_preference,
        //         compatible_surface: None,
        //         ..default()
        //     },
        // )
        // .await;

        // validation fails
        let adapter = unsafe { instance.enumerate_adapters() }
            .into_iter()
            .next()
            .expect("no adapters");
        let device = unsafe {
            hal_custom::open_adapter(
                &adapter.adapter,
                settings.features.clone(),
                [
                    ash::extensions::khr::GetMemoryRequirements2::name(),
                    ash::extensions::khr::ExternalMemoryFd::name(),
                ],
            )
            .expect("failed to open device")
        };
        let instance = unsafe { wgpu::Instance::from_hal::<vulkan::Api>(instance) };
        let adapter = unsafe { instance.create_adapter_from_hal(adapter) };
        let adapter_info = adapter.get_info();
        let device_descriptor =
            hal_custom::make_device_descriptor(&settings, &adapter, &adapter_info);
        let (device, queue) =
            unsafe { adapter.create_device_from_hal(device, &device_descriptor, None) }
                .expect("failed to create device");
        let device = RenderDevice::from(device);
        let queue = RenderQueue(Arc::new(WgpuWrapper::new(queue)));
        let adapter_info = RenderAdapterInfo(WgpuWrapper::new(adapter_info));
        let adapter = RenderAdapter(Arc::new(WgpuWrapper::new(adapter)));

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

// https://github.com/dzfranklin/drm-fourcc-rs/blob/main/src/consts.rs
// const DMABUF_MODIFIER: u64 = 0xff_ffff_ffff_ffff; // invalid
const DMABUF_MODIFIER: u64 = 0; // DRM_FORMAT_MOD_LINEAR

// https://github.com/torvalds/linux/blob/master/include/uapi/drm/drm_fourcc.h
// Why isn't this RGBA8? I don't know! But this works!
const DMABUF_FORMAT: u32 = u32::from_le_bytes(*b"AB24"); // ABGR8888
const VK_FORMAT: vk::Format = vk::Format::R8G8B8A8_SRGB;
const TEXTURE_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;

pub fn setup_render_target(size: UVec2, render_device: &RenderDevice) -> (ManualTextureView, i32) {
    let wgpu_device = render_device.wgpu_device();
    let (texture, dmabuf_fd) = unsafe {
        let r = wgpu_device.as_hal::<vulkan::Api, _, _>(|hal_device| {
            let hal_device = hal_device.expect("`RenderDevice` is not a vulkan device");
            create_target_from_hal(wgpu_device, hal_device, size.x, size.y)
        });
        r.unwrap()
    };

    let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let manual_texture_view = ManualTextureView {
        texture_view: texture_view.into(),
        size,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
    };

    (manual_texture_view, dmabuf_fd)
}

fn create_target_from_hal(
    wgpu_device: &wgpu::Device,
    hal_device: &vulkan::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, i32) {
    struct DropGuard {
        device: ash::Device,
        memory: vk::DeviceMemory,
        image: vk::Image,
        dmabuf_fd: i32,
    }

    impl Drop for DropGuard {
        fn drop(&mut self) {
            unsafe {
                self.device.destroy_image(self.image, None);
                self.device.free_memory(self.memory, None);
            }

            let dmabuf = unsafe { File::from_raw_fd(self.dmabuf_fd) };
            drop(dmabuf);
        }
    }

    let vk_device = hal_device.raw_device();
    let instance = hal_device.shared_instance().raw_instance();

    let external_memory_image_create = vk::ExternalMemoryImageCreateInfo {
        handle_types: vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
        ..default()
    };
    let image_create = vk::ImageCreateInfo {
        p_next: &external_memory_image_create as *const _ as *const c_void,
        image_type: vk::ImageType::TYPE_2D,
        format: VK_FORMAT,
        extent: vk::Extent3D {
            width,
            height,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: vk::SampleCountFlags::TYPE_1,
        tiling: vk::ImageTiling::LINEAR, // or OPTIMAL?
        usage: vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::COLOR_ATTACHMENT,
        sharing_mode: vk::SharingMode::EXCLUSIVE,
        initial_layout: vk::ImageLayout::UNDEFINED,
        ..default()
    };
    let image =
        unsafe { vk_device.create_image(&image_create, None) }.expect("failed to create image");

    let mut memory_requirements = vk::MemoryRequirements2KHR::default();
    unsafe {
        ash::extensions::khr::GetMemoryRequirements2::new(instance, vk_device)
            .get_image_memory_requirements2(
                &vk::ImageMemoryRequirementsInfo2 { image, ..default() },
                &mut memory_requirements,
            );
    }

    let dedicated_alloc_info = vk::MemoryDedicatedAllocateInfo { image, ..default() };
    let export_info = vk::ExportMemoryAllocateInfo {
        p_next: &dedicated_alloc_info as *const _ as *const c_void,
        handle_types: vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
        ..default()
    };
    let alloc_info = vk::MemoryAllocateInfo {
        p_next: &export_info as *const _ as *const c_void,
        allocation_size: memory_requirements.memory_requirements.size,
        ..default()
    };
    let memory =
        unsafe { vk_device.allocate_memory(&alloc_info, None) }.expect("failed to allocate memory");

    let bind_image_memory = vk::BindImageMemoryInfo {
        image,
        memory,
        ..default()
    };
    unsafe { vk_device.bind_image_memory2(&[bind_image_memory]) }
        .expect("failed to bind memory to image");

    let get_memory_info = vk::MemoryGetFdInfoKHR {
        memory,
        handle_type: vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
        ..default()
    };
    let dmabuf_fd = unsafe {
        ash::extensions::khr::ExternalMemoryFd::new(instance, vk_device)
            .get_memory_fd(&get_memory_info)
    }
    .expect("failed to get fd for allocated memory");

    let texture_desc = wgpu_hal::TextureDescriptor {
        label: Some("adwaita_render_target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TEXTURE_FORMAT,
        usage: wgpu_hal::TextureUses::COPY_SRC | wgpu_hal::TextureUses::COLOR_TARGET,
        memory_flags: wgpu_hal::MemoryFlags::empty(),
        view_formats: Vec::new(),
    };

    let drop_guard = Box::new(DropGuard {
        device: hal_device.raw_device().clone(),
        memory,
        image,
        dmabuf_fd,
    });
    let texture =
        unsafe { vulkan::Device::texture_from_raw(image, &texture_desc, Some(drop_guard)) };

    let texture = unsafe {
        wgpu_device.create_texture_from_hal::<vulkan::Api>(
            texture,
            &wgpu::TextureDescriptor {
                label: Some("adwaita_render_target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TEXTURE_FORMAT,
                usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            },
        )
    };

    (texture, dmabuf_fd)
}

pub fn build_dmabuf_texture(info: DmabufInfo) -> gdk::Texture {
    let DmabufInfo { size, fd } = info;

    // https://docs.gtk.org/gdk4/class.DmabufTextureBuilder.html

    let builder = gdk::DmabufTextureBuilder::new();
    builder.set_width(size.x);
    builder.set_height(size.y);
    builder.set_fourcc(DMABUF_FORMAT);
    builder.set_modifier(DMABUF_MODIFIER);

    builder.set_n_planes(1);
    builder.set_fd(0, fd);
    builder.set_offset(0, 0);
    builder.set_stride(0, size.x * 4); // bytes per row

    unsafe { builder.build() }.expect("should be a valid dmabuf texture")
}
