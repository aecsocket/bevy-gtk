use {
    ash::vk,
    bevy_ecs::error::BevyError,
    bevy_utils::default,
    derive_more::{Debug, Deref},
    drm_fourcc::{DrmFormat, DrmFourcc, DrmModifier},
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
}

impl DmabufTexture {
    /// Creates a dmabuf-backed texture on a Vulkan [`wgpu::Device`].
    pub fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        label: Option<&'static str>,
    ) -> Result<Self, BevyError> {
        // SAFETY: `hal_device` is not manually destroyed by us
        let hal_device = unsafe { device.as_hal::<wgpu_hal::vulkan::Api>() }
            .expect("render device is not a Vulkan device");
        create_dmabuf_texture(device, &hal_device, label, width, height)
    }

    #[must_use]
    pub fn wgpu_texture(&self) -> &wgpu::Texture {
        &self.wgpu_texture
    }
}

const MEMORY_HANDLE_TYPE: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;

fn create_dmabuf_texture(
    wgpu_device: &wgpu::Device,
    hal_device: &wgpu_hal::vulkan::Device,
    label: Option<&'static str>,
    width: u32,
    height: u32,
) -> Result<DmabufTexture, BevyError> {
    // Renderdoc doesn't support capturing processes which export memory.
    // As of renderdoc v1.39, [`ash::ext::image_drm_format_modifier::NAME`] is
    // unsupported and causes Vulkan init to fail. You can sort of get around
    // this extension if you use a `vk::ImageTiling::LINEAR` image instead of
    // `vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT`, but I think this is less
    // correct.

    const DRM_MODIFIER_PLANE_COUNT: u32 = 1;
    const VK_DIM: vk::ImageType = vk::ImageType::TYPE_2D;
    const WGPU_DIM: wgpu::TextureDimension = wgpu::TextureDimension::D2;
    const VK_TILING: vk::ImageTiling = vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT;
    const MIP_LEVELS: u32 = 1;
    const VK_SAMPLES: vk::SampleCountFlags = vk::SampleCountFlags::TYPE_1;
    const WGPU_SAMPLES: u32 = 1;
    let vk_usage: vk::ImageUsageFlags =
        vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::COLOR_ATTACHMENT;
    let hal_usage: wgpu::TextureUses =
        wgpu::TextureUses::COPY_SRC | wgpu::TextureUses::COLOR_TARGET;
    let wgpu_usage: wgpu::TextureUsages =
        wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::RENDER_ATTACHMENT;

    let vk_instance = hal_device.shared_instance().raw_instance();
    let vk_physical_device = hal_device.raw_physical_device();
    let vk_device = hal_device.raw_device();

    let wgpu_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let vk_format = vk::Format::R8G8B8A8_SRGB;
    let drm_format = DrmFourcc::Abgr8888;
    let drm_modifier = DrmModifier::Linear;

    // check if we can use this modifier with this format
    let Some(drm_modifier_info) =
        get_drm_modifiers_for_format(vk_instance, vk_physical_device, vk_format)
            .find(|info| info.modifier == drm_modifier)
    else {
        return Err(format!("modifier {drm_modifier:?} is not available for {vk_format:?}").into());
    };
    if drm_modifier_info.plane_count != DRM_MODIFIER_PLANE_COUNT {
        return Err(format!(
            "cannot use DRM modifier with more than 1 memory plane (has {} planes)",
            drm_modifier_info.plane_count
        )
        .into());
    }

    // if we make an image with this format and modifier, what properties does it
    // have? make sure we fit within the limits
    {
        let mut with_drm_format_props = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT {
            drm_format_modifier: drm_modifier.into(),
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            queue_family_index_count: 0,
            ..default()
        };

        let params = vk::PhysicalDeviceImageFormatInfo2 {
            format: vk_format,
            ty: VK_DIM,
            tiling: VK_TILING,
            usage: vk_usage,
            ..default()
        }
        .push_next(&mut with_drm_format_props);
        let mut format_props = vk::ImageFormatProperties2::default();
        unsafe {
            vk_instance.get_physical_device_image_format_properties2(
                vk_physical_device,
                &params,
                &mut format_props,
            )?;
        }

        let max_extent = format_props.image_format_properties.max_extent;
        if width > max_extent.width {
            return Err(format!("width too large: {width} / {}", max_extent.width).into());
        }
        if height > max_extent.height {
            return Err(format!("height too large: {height} / {}", max_extent.height).into());
        }
    }

    // create the vulkan image
    let vk_image = {
        // our image can be backed by external memory
        let mut with_external_memory = vk::ExternalMemoryImageCreateInfo {
            handle_types: MEMORY_HANDLE_TYPE,
            ..default()
        };
        // image tiling is defined by a DRM format modifier
        // right now, we only support modifiers with 1 memory plane
        let plane_layouts = [vk::SubresourceLayout {
            offset: 0,
            size: 0,
            row_pitch: u64::from(width) * u64::from(u32::BITS / 8),
            array_pitch: 0,
            depth_pitch: 0,
        }];
        let mut with_drm_format_modifier = vk::ImageDrmFormatModifierExplicitCreateInfoEXT {
            drm_format_modifier: drm_modifier.into(),
            drm_format_modifier_plane_count: DRM_MODIFIER_PLANE_COUNT,
            p_plane_layouts: (&raw const plane_layouts).cast(),
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
            tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
            usage: vk_usage,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            initial_layout: vk::ImageLayout::UNDEFINED,
            ..default()
        }
        .push_next(&mut with_drm_format_modifier)
        .push_next(&mut with_external_memory);
        unsafe { vk_device.create_image(&params, None) }?
    };

    // to allocate memory for the image, we get what requirements the memory has for
    // this image
    let memory_requirements = {
        // memory requirements are based on the image we just made
        let params = vk::ImageMemoryRequirementsInfo2 {
            image: vk_image,
            ..default()
        };

        let mut out = vk::MemoryRequirements2::default();
        unsafe {
            vk_device.get_image_memory_requirements2(&params, &mut out);
        }
        out
    };

    let vk_memory = {
        // TODO
        let memory_type =
            find_memory_type(memory_requirements.memory_requirements.memory_type_bits);

        // this memory will be bound to exactly one image
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
            allocation_size: memory_requirements.memory_requirements.size,
            memory_type_index: memory_type,
            ..default()
        }
        .push_next(&mut with_export)
        .push_next(&mut with_dedicated);
        unsafe { vk_device.allocate_memory(&params, None) }?
    };

    unsafe { vk_device.bind_image_memory(vk_image, vk_memory, 0) }?;

    // make wgpu resources out of vulkan resources

    let hal_texture = {
        let descriptor_hal = wgpu_hal::TextureDescriptor {
            label,
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: MIP_LEVELS,
            sample_count: WGPU_SAMPLES,
            dimension: WGPU_DIM,
            format: wgpu_format,
            usage: hal_usage,
            memory_flags: wgpu_hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        };
        let drop_callback = {
            let vk_device = vk_device.clone();
            Box::new(move || unsafe {
                vk_device.destroy_image(vk_image, None);
                vk_device.free_memory(vk_memory, None);
            })
        };
        unsafe { hal_device.texture_from_raw(vk_image, &descriptor_hal, Some(drop_callback)) }
    };

    // SAFETY:
    // - `hal_texture` was just created from the device's internal handle
    // - `hal_texture`'s descriptor is the same as the descriptor we're making now,
    //   enforced via the `VK_` and `WGPU_` variable parity
    // - `hal_texture` has just been initialized
    let wgpu_texture = unsafe {
        wgpu_device.create_texture_from_hal::<wgpu_hal::vulkan::Api>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label,
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: MIP_LEVELS,
                sample_count: WGPU_SAMPLES,
                dimension: WGPU_DIM,
                format: wgpu_format,
                usage: wgpu_usage,
                view_formats: &[],
            },
        )
    };

    Ok(DmabufTexture {
        vk_instance: vk_instance.clone(),
        vk_device: vk_device.clone(),
        wgpu_texture,
        vk_memory,
        drm_format: DrmFormat {
            code: drm_format,
            modifier: drm_modifier,
        },
    })
}

struct DrmModifierInfo {
    modifier: DrmModifier,
    plane_count: u32,
}

fn get_drm_modifiers_for_format(
    vk_instance: &ash::Instance,
    vk_physical_device: vk::PhysicalDevice,
    vk_format: vk::Format,
) -> impl Iterator<Item = DrmModifierInfo> {
    let modifier_count = {
        let mut drm_modifier_props = vk::DrmFormatModifierPropertiesList2EXT::default();
        let mut format_props = vk::FormatProperties2::default().push_next(&mut drm_modifier_props);
        unsafe {
            vk_instance.get_physical_device_format_properties2(
                vk_physical_device,
                vk_format,
                &mut format_props,
            );
        }
        drm_modifier_props.drm_format_modifier_count
    };

    let mut drm_modifiers = {
        let mut buf = Vec::new();
        buf.resize_with(modifier_count as usize, Default::default);
        buf.into_boxed_slice()
    };
    let mut drm_modifier_props = vk::DrmFormatModifierPropertiesList2EXT {
        drm_format_modifier_count: modifier_count,
        p_drm_format_modifier_properties: drm_modifiers.as_mut_ptr(),
        ..default()
    };
    let mut format_props = vk::FormatProperties2::default().push_next(&mut drm_modifier_props);
    unsafe {
        vk_instance.get_physical_device_format_properties2(
            vk_physical_device,
            vk_format,
            &mut format_props,
        );
    }

    (0..modifier_count).map(move |i| {
        let format_props = unsafe {
            *drm_modifier_props
                .p_drm_format_modifier_properties
                .add(i as usize)
        };

        DrmModifierInfo {
            modifier: DrmModifier::from(format_props.drm_format_modifier),
            plane_count: format_props.drm_format_modifier_plane_count,
        }
    })
}

fn find_memory_type(type_bits: u32) -> u32 {
    for index in 0..32 {
        if type_bits & (1 << index) == 0 {
            continue;
        }

        // TODO

        return index;
    }

    // TODO
    panic!("uh oh");
}

impl DmabufTexture {
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

    pub fn build_gdk_texture(&self) -> Result<gdk::Texture, BevyError> {
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
            // <https://docs.kernel.org/userspace-api/dma-buf-alloc-exchange.html#term-stride>
            .set_stride(0, width * u32::BITS / 8);

        // SAFETY: I have no clue what the invariants are.
        let gdk_texture = unsafe { builder.build_with_release_func(move || drop(fd))? };
        Ok(gdk_texture)
    }
}
