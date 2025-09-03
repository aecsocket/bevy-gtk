use {
    ash::vk,
    bevy_ecs::error::BevyError,
    bevy_utils::default,
    derive_more::{Debug, Deref},
    drm_fourcc::{DrmFormat, DrmFourcc, DrmModifier},
    log::trace,
    std::os::fd::{AsRawFd as _, FromRawFd, OwnedFd},
};

/// [`wgpu::Texture`] which is backed by Linux dmabuf memory.
#[derive(Debug, Clone, Deref)]
pub struct DmabufTexture {
    #[debug(skip)]
    vk_instance: ash::Instance,
    #[debug(skip)]
    vk_device: ash::Device,
    #[deref]
    wgpu_texture: wgpu::Texture,
    #[debug(skip)]
    vk_memory: vk::DeviceMemory,
    drm_format: DrmFormat,
    // <https://docs.kernel.org/userspace-api/dma-buf-alloc-exchange.html#term-stride>
    stride: u32,
}

impl DmabufTexture {
    /// Creates a dmabuf-backed texture on a Vulkan [`wgpu::Device`].
    pub fn new(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Result<Self, BevyError> {
        create_dmabuf_texture(adapter, device, width, height, format)
    }

    #[must_use]
    pub fn wgpu_texture(&self) -> &wgpu::Texture {
        &self.wgpu_texture
    }

    /// Opens a new file descriptor to the underlying dmabuf memory.
    ///
    /// The file descriptor (and therefore reference to the underlying dmabuf/
    /// device memory) is owned by the caller. If the texture is dropped before
    /// the file descriptor, the memory will stay allocated.
    ///
    /// # Errors
    ///
    /// See <https://registry.khronos.org/vulkan/specs/latest/man/html/vkGetMemoryFdKHR.html>.
    pub fn open_fd(&self) -> Result<OwnedFd, BevyError> {
        let get_fd_info = vk::MemoryGetFdInfoKHR {
            memory: self.vk_memory,
            handle_type: MEMORY_HANDLE_TYPE,
            ..default()
        };
        let raw_fd = unsafe {
            ash::khr::external_memory_fd::Device::new(&self.vk_instance, &self.vk_device)
                .get_memory_fd(&get_fd_info)
        }?;
        // SAFETY: Vulkan just created a new open fd for us.
        // <https://registry.khronos.org/vulkan/specs/latest/man/html/vkGetMemoryFdKHR.html>
        //
        //     Each call to vkGetMemoryFdKHR must create a new file descriptor...
        //
        Ok(unsafe { OwnedFd::from_raw_fd(raw_fd) })
    }

    /// Builds a [`gdk::Texture`] backed by a file descriptor to this DMA
    /// buffer.
    ///
    /// # Errors
    ///
    /// Errors if [`DmabufTexture::open_fd`] or building the
    /// [`gdk::DmabufTexture`] fail.
    pub fn build_gdk_texture(&self) -> Result<gdk::Texture, BevyError> {
        // The Gdk docs are completely useless for the parameters here.
        // See the Linux userspace docs instead:
        // <https://www.kernel.org/doc/html//latest/userspace-api/dma-buf-alloc-exchange.html>

        let fd = self.open_fd()?;
        let (width, height) = (self.width(), self.height());
        let builder = gdk::DmabufTextureBuilder::new()
            .set_width(width)
            .set_height(height)
            .set_fourcc(self.drm_format.code as u32)
            .set_modifier(self.drm_format.modifier.into())
            .set_n_planes(1);

        // SAFETY: we use `build_with_release_func` to:
        // - move `fd` under the ownership of `gdk_texture`
        // - close `fd` when `gdk_texture` is destroyed
        let builder = unsafe { builder.set_fd(0, fd.as_raw_fd()) }
            .set_offset(0, 0)
            .set_stride(0, self.stride);

        // SAFETY: I have no clue what the safety invariants are.
        let gdk_texture = unsafe { builder.build_with_release_func(move || drop(fd))? };
        Ok(gdk_texture)
    }
}

const LABEL: &str = "bevy_gtk dmabuf texture";
const VK_DIM: vk::ImageType = vk::ImageType::TYPE_2D;
const WGPU_DIM: wgpu::TextureDimension = wgpu::TextureDimension::D2;
const VK_TILING: vk::ImageTiling = vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT;
const MIP_LEVELS: u32 = 1;
const VK_SAMPLES: vk::SampleCountFlags = vk::SampleCountFlags::TYPE_1;
const WGPU_SAMPLES: u32 = 1;
const MEMORY_HANDLE_TYPE: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;

fn vk_usage() -> vk::ImageUsageFlags {
    vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::COLOR_ATTACHMENT
}

fn hal_usage() -> wgpu::TextureUses {
    wgpu::TextureUses::COPY_SRC | wgpu::TextureUses::COLOR_TARGET
}

fn wgpu_usage() -> wgpu::TextureUsages {
    wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::RENDER_ATTACHMENT
}

fn create_dmabuf_texture(
    wgpu_adapter: &wgpu::Adapter,
    wgpu_device: &wgpu::Device,
    width: u32,
    height: u32,
    wgpu_format: wgpu::TextureFormat,
) -> Result<DmabufTexture, BevyError> {
    // Renderdoc doesn't support capturing processes which export memory.
    // As of renderdoc v1.39, [`ash::ext::image_drm_format_modifier::NAME`] is
    // unsupported and causes Vulkan init to fail. You can sort of get around
    // this extension if you use a `vk::ImageTiling::LINEAR` image instead of
    // `vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT`, but I think this is less
    // correct.

    // SAFETY: `hal_adapter` is not manually destroyed by us
    let hal_adapter = unsafe { wgpu_adapter.as_hal::<wgpu_hal::vulkan::Api>() }
        .expect("render adapter is not a Vulkan adapter");
    // SAFETY: `hal_device` is not manually destroyed by us
    let hal_device = unsafe { wgpu_device.as_hal::<wgpu_hal::vulkan::Api>() }
        .expect("render device is not a Vulkan device");

    let dev = Devices {
        vk_instance: hal_device.shared_instance().raw_instance(),
        hal_adapter: &hal_adapter,
        vk_physical_device: hal_device.raw_physical_device(),
        vk_device: hal_device.raw_device(),
        hal_device: &hal_device,
        wgpu_device,
    };

    let drm_format = format_to_fourcc(wgpu_format)
        .ok_or_else(|| format!("texture format {wgpu_format:?} cannot be mapped to a fourcc"))?;

    let vk_image = unsafe { create_image(&dev, width, height, wgpu_format) }?;
    let vk_memory = unsafe { allocate_memory(&dev, vk_image) }?;
    unsafe { dev.vk_device.bind_image_memory(vk_image, vk_memory, 0) }?;

    // when we create the image, we give the GPU a list of what DRM modifiers it
    // *could* use, but which one it chooses is implementation-specific.
    // after creating the image, we query which modifier it actually chose.
    let drm_modifier = unsafe { get_image_drm_modifier(&dev, vk_image) }?;

    trace!(
        "Using DRM format {drm_format}:0x{:016x} ({drm_modifier:?} vendor {:?})",
        u64::from(drm_modifier),
        drm_modifier.vendor(),
    );

    let wgpu_texture = vk_texture_to_wgpu(&dev, vk_image, vk_memory, width, height, wgpu_format);
    Ok(DmabufTexture {
        vk_instance: dev.vk_instance.clone(),
        vk_device: dev.vk_device.clone(),
        wgpu_texture,
        vk_memory,
        drm_format: DrmFormat {
            code: drm_format,
            modifier: drm_modifier,
        },
        stride: width * 4, // TODO
    })
}

struct Devices<'a> {
    vk_instance: &'a ash::Instance,
    hal_adapter: &'a wgpu_hal::vulkan::Adapter,
    vk_physical_device: ash::vk::PhysicalDevice,
    vk_device: &'a ash::Device,
    hal_device: &'a wgpu_hal::vulkan::Device,
    wgpu_device: &'a wgpu::Device,
}

unsafe fn create_image(
    dev: &Devices,
    width: u32,
    height: u32,
    wgpu_format: wgpu::TextureFormat,
) -> Result<vk::Image, BevyError> {
    let vk_format = dev.hal_adapter.texture_format_as_raw(wgpu_format);

    // for this texture format, figure out what DRM modifiers we can use
    // we start by getting the number of modifiers `drm_modifier_count`
    let drm_modifier_count = {
        let mut drm_modifier_out = vk::DrmFormatModifierPropertiesList2EXT::default();
        let mut format_out = vk::FormatProperties2::default().push_next(&mut drm_modifier_out);
        unsafe {
            dev.vk_instance.get_physical_device_format_properties2(
                dev.vk_physical_device,
                vk_format,
                &mut format_out,
            );
        }
        drm_modifier_out.drm_format_modifier_count
    };

    // then allocate a buffer for `drm_modifier_count` number of modifier props
    // and get info for those modifiers
    let drm_modifiers = {
        let mut buf = (0..drm_modifier_count)
            .map(|_| default())
            .collect::<Box<[_]>>();
        let mut drm_modifier_out = vk::DrmFormatModifierPropertiesList2EXT {
            drm_format_modifier_count: drm_modifier_count,
            p_drm_format_modifier_properties: buf.as_mut_ptr(),
            ..default()
        };
        let mut format_out = vk::FormatProperties2::default().push_next(&mut drm_modifier_out);
        unsafe {
            dev.vk_instance.get_physical_device_format_properties2(
                dev.vk_physical_device,
                vk_format,
                &mut format_out,
            );
        }
        buf.into_iter()
            .map(|props| DrmModifier::from(props.drm_format_modifier))
            .collect::<Box<[_]>>()
    };

    // TODO
    let drm_modifiers = [DrmModifier::Linear];

    trace!("Available DRM format modifiers");
    for modifier in &drm_modifiers {
        trace!(
            "- 0x{:016x} ({modifier:?} vendor {:?})",
            u64::from(*modifier),
            modifier.vendor()
        );
    }

    // we tell the device that we can make an image with any of the above modifiers,
    // we're not picky
    // let mut with_drm_modifiers = vk::ImageDrmFormatModifierListCreateInfoEXT {
    //     drm_format_modifier_count: drm_modifiers.len() as u32,
    //     p_drm_format_modifiers: drm_modifiers.as_ptr().cast(),
    //     ..default()
    // };

    let plane_layouts = [vk::SubresourceLayout {
        offset: 0,
        size: 0,
        row_pitch: u64::from(width * 4),
        array_pitch: 0,
        depth_pitch: 0,
    }];
    let mut with_drm_modifiers = vk::ImageDrmFormatModifierExplicitCreateInfoEXT {
        drm_format_modifier: 0,
        drm_format_modifier_plane_count: 1,
        p_plane_layouts: (&raw const plane_layouts).cast(),
        ..default()
    };

    // our image can be backed by external memory
    let mut with_external_memory = vk::ExternalMemoryImageCreateInfo {
        handle_types: MEMORY_HANDLE_TYPE,
        ..default()
    };
    let params = vk::ImageCreateInfo {
        image_type: VK_DIM,
        format: vk_format,
        extent: vk::Extent3D {
            width,
            height,
            depth: 1,
        },
        mip_levels: MIP_LEVELS,
        array_layers: 1,
        samples: VK_SAMPLES,
        tiling: VK_TILING,
        usage: vk_usage(),
        sharing_mode: vk::SharingMode::EXCLUSIVE,
        initial_layout: vk::ImageLayout::UNDEFINED,
        ..default()
    }
    .push_next(&mut with_drm_modifiers)
    .push_next(&mut with_external_memory);
    Ok(unsafe { dev.vk_device.create_image(&params, None) }?)
}

unsafe fn get_image_drm_modifier(
    dev: &Devices,
    vk_image: vk::Image,
) -> Result<DrmModifier, BevyError> {
    let mut out = vk::ImageDrmFormatModifierPropertiesEXT::default();
    let device = ash::ext::image_drm_format_modifier::Device::new(dev.vk_instance, dev.vk_device);
    unsafe { device.get_image_drm_format_modifier_properties(vk_image, &mut out) }?;
    Ok(DrmModifier::from(out.drm_format_modifier))
}

unsafe fn allocate_memory(
    dev: &Devices,
    vk_image: vk::Image,
) -> Result<vk::DeviceMemory, BevyError> {
    // ask the device what memory specs (size etc.) are required for this image
    let memory_requirements = {
        let params = vk::ImageMemoryRequirementsInfo2 {
            image: vk_image,
            ..default()
        };
        let mut out = vk::MemoryRequirements2::default();
        unsafe {
            dev.vk_device
                .get_image_memory_requirements2(&params, &mut out);
        }
        out.memory_requirements
    };

    // ask the device what memory types it has
    let memory_props = {
        let mut out = vk::PhysicalDeviceMemoryProperties2::default();
        unsafe {
            dev.vk_instance
                .get_physical_device_memory_properties2(dev.vk_physical_device, &mut out);
        }
        out.memory_properties
    };

    // given what memory types the device has (`memory_props`),
    // and what kinds we can use for our image allocation (`memory_type_bits`),
    // figure out what memory type index in that bitset is best for us
    let memory_type_bits = memory_requirements.memory_type_bits;
    let memory_type_index = (0..memory_props.memory_type_count).find(|index| {
        if memory_type_bits & (1 << index) == 0 {
            return false;
        }

        let memory_type_props = memory_props.memory_types[*index as usize].property_flags;
        // we want a memory type which is visible on the CPU as well
        // since the CPU will likely need to copy it through GTK
        // TODO: maybe not?
        memory_type_props.contains(
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )
    });
    let Some(memory_type_index) = memory_type_index else {
        return Err("no compatible memory type found".into());
    };

    // this memory will be bound to exactly one image
    // it's recommended to use a dedicated memory allocation for exported resources
    let mut with_dedicated = vk::MemoryDedicatedAllocateInfo {
        image: vk_image,
        ..default()
    };
    // this memory must be exportable
    let mut with_export = vk::ExportMemoryAllocateInfo {
        handle_types: MEMORY_HANDLE_TYPE,
        ..default()
    };

    let params = vk::MemoryAllocateInfo {
        allocation_size: memory_requirements.size,
        memory_type_index,
        ..default()
    }
    .push_next(&mut with_export)
    .push_next(&mut with_dedicated);
    Ok(unsafe { dev.vk_device.allocate_memory(&params, None) }?)
}

fn vk_texture_to_wgpu(
    dev: &Devices,
    vk_image: vk::Image,
    vk_memory: vk::DeviceMemory,
    width: u32,
    height: u32,
    wgpu_format: wgpu::TextureFormat,
) -> wgpu::Texture {
    let hal_texture = {
        let hal_descriptor = wgpu_hal::TextureDescriptor {
            label: Some(LABEL),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: MIP_LEVELS,
            sample_count: WGPU_SAMPLES,
            dimension: WGPU_DIM,
            format: wgpu_format,
            usage: hal_usage(),
            memory_flags: wgpu_hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        };
        let drop_callback = {
            let vk_device = dev.vk_device.clone();
            Box::new(move || unsafe {
                vk_device.destroy_image(vk_image, None);
                vk_device.free_memory(vk_memory, None);
            })
        };
        // SAFETY:
        // - `vk_image` was created with the same descriptor as `hal_descriptor`
        // - we move `vk_image` into the `drop_callback`, and destroy it on drop
        // - `view_formats` is empty
        unsafe {
            dev.hal_device
                .texture_from_raw(vk_image, &hal_descriptor, Some(drop_callback))
        }
    };

    // SAFETY:
    // - `hal_texture` was just created from the device's internal handle
    // - `hal_texture`'s descriptor is the same as the descriptor we're making now,
    //   enforced via the `VK_` and `WGPU_` variable parity
    // - `hal_texture` has just been initialized
    unsafe {
        dev.wgpu_device
            .create_texture_from_hal::<wgpu_hal::vulkan::Api>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some(LABEL),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: MIP_LEVELS,
                    sample_count: WGPU_SAMPLES,
                    dimension: WGPU_DIM,
                    format: wgpu_format,
                    usage: wgpu_usage(),
                    view_formats: &[],
                },
            )
    }
}

fn format_to_fourcc(format: wgpu::TextureFormat) -> Option<DrmFourcc> {
    use {DrmFourcc as Cc, wgpu::TextureFormat as Tf};
    match format {
        Tf::Rgba8Unorm | Tf::Rgba8UnormSrgb => Some(Cc::Abgr8888),
        _ => None, // TODO
    }
}
