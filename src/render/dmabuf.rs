use {
    crate::render::GtkRenderData,
    ash::vk,
    bevy_derive::Deref,
    bevy_ecs::error::BevyError,
    bevy_utils::default,
    drm_fourcc::{DrmFourcc, DrmModifier},
    std::os::fd::{AsRawFd as _, FromRawFd, OwnedFd},
};

/// [`wgpu::Texture`] which is backed by Linux dmabuf memory.
#[derive(Deref)]
pub struct DmabufTexture {
    vk_instance: ash::Instance,
    vk_device: ash::Device,
    #[deref]
    wgpu_texture: wgpu::Texture,
    vk_memory: vk::DeviceMemory,
}

impl GtkRenderData {
    /// Creates a dmabuf-backed texture on a Vulkan [`wgpu::Device`].
    pub fn create_dmabuf_texture<'l>(
        &self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        label: Option<&'static str>,
    ) -> Result<DmabufTexture, BevyError> {
        // SAFETY: `hal_device` is not manually destroyed by us
        let hal_device = unsafe { device.as_hal::<wgpu_hal::vulkan::Api>() }
            .expect("render device is not a Vulkan device");
        create_dmabuf_texture(device, &*hal_device, label, width, height)
    }
}

const MEMORY_HANDLE_TYPE: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;

const WGPU_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DRM_FORMAT: DrmFourcc = DrmFourcc::Abgr8888;

fn create_dmabuf_texture(
    wgpu_device: &wgpu::Device,
    hal_device: &wgpu_hal::vulkan::Device,
    label: Option<&'static str>,
    width: u32,
    height: u32,
) -> Result<DmabufTexture, BevyError> {
    const VK_DIM: vk::ImageType = vk::ImageType::TYPE_2D;
    const WGPU_DIM: wgpu::TextureDimension = wgpu::TextureDimension::D2;

    const VK_TILING: vk::ImageTiling = vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT;

    const MIP_LEVELS: u32 = 1;
    const DEPTH: u32 = 1;
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

    // Renderdoc doesn't support capturing processes which export memory.
    // As of renderdoc v1.39, [`ash::ext::image_drm_format_modifier::NAME`] is
    // unsupported and causes Vulkan init to fail. You can sort of get around
    // this extension if you use a `vk::ImageTiling::LINEAR` image instead of
    // `vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT`, but I think this is less
    // correct.

    let vk_format = vk::Format::R8G8B8A8_SRGB;
    let drm_modifier = DrmModifier::Linear;

    // check if we can use this modifier with this format
    if !get_drm_modifiers_for_format(vk_instance, vk_physical_device, vk_format)
        .any(|valid_modifier| valid_modifier == drm_modifier)
    {
        return Err(format!("modifier {drm_modifier:?} is not available for {vk_format:?}").into());
    }

    let mut x = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT {
        drm_format_modifier: drm_modifier.into(),
        sharing_mode: vk::SharingMode::EXCLUSIVE,
        queue_family_index_count: 0,
        ..default()
    };
    let format_info = vk::PhysicalDeviceImageFormatInfo2 {
        format: vk_format,
        ty: VK_DIM,
        tiling: VK_TILING,
        usage: vk_usage,
        ..default()
    }
    .push_next(&mut x);
    let mut image_format_prop = vk::ImageFormatProperties2::default();
    unsafe {
        vk_instance.get_physical_device_image_format_properties2(
            vk_physical_device,
            &format_info,
            &mut image_format_prop,
        )?;
    }

    let max_extent = image_format_prop.image_format_properties.max_extent;
    if width > max_extent.width {
        return Err(format!("width too large: {width} / {}", max_extent.width).into());
    }
    if height > max_extent.height {
        return Err(format!("height too large: {height} / {}", max_extent.height).into());
    }

    // image can be backed by external memory
    let mut external_memory_image_create = vk::ExternalMemoryImageCreateInfo {
        handle_types: MEMORY_HANDLE_TYPE,
        ..default()
    };
    // image tiling is defined by a DRM format modifier
    //
    //
    // TODO: what types do we support?
    let drm_format_modifiers = [DrmModifier::Linear, DrmModifier::Linear];

    // let mut image_drm_format_modifier_list_create =
    //     vk::ImageDrmFormatModifierExplicitCreateInfoEXT {
    //         drm_format_modifier: DrmModifier::Linear,
    //         drm_format_modifier_plane_count: 1,
    //         drm_format_modifier_count: drm_format_modifiers.len() as u32,
    //         p_drm_format_modifiers: &drm_format_modifiers as *const _ as *const
    // u64,         ..default()
    //     };
    let mut image_drm_format_modifier_list_create = vk::ImageDrmFormatModifierListCreateInfoEXT {
        drm_format_modifier_count: drm_format_modifiers.len() as u32,
        p_drm_format_modifiers: &drm_format_modifiers as *const _ as *const u64,
        ..default()
    };

    // make the image
    let image_create = vk::ImageCreateInfo {
        image_type: VK_DIM,
        format: vk_format,
        extent: vk::Extent3D {
            width,
            height,
            depth: DEPTH,
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
    .push_next(&mut image_drm_format_modifier_list_create)
    .push_next(&mut external_memory_image_create);
    let vk_image = unsafe { vk_device.create_image(&image_create, None) }?;

    // to allocate memory for the image, we get what requirements the memory has for
    // this image

    // memory requirements are based on the image we just made
    let image_memory_requirements = vk::ImageMemoryRequirementsInfo2 {
        image: vk_image,
        ..default()
    };
    // get the requirements
    let mut memory_requirements = vk::MemoryRequirements2::default();
    unsafe {
        vk_device
            .get_image_memory_requirements2(&image_memory_requirements, &mut memory_requirements);
    }

    // TODO
    let memory_type = find_memory_type(memory_requirements.memory_requirements.memory_type_bits);

    // this memory will be bound to exactly one image
    let mut memory_dedicated_allocate = vk::MemoryDedicatedAllocateInfo {
        image: vk_image,
        ..default()
    };
    // this memory must be exportable
    let mut export_memory_allocate = vk::ExportMemoryAllocateInfo {
        handle_types: MEMORY_HANDLE_TYPE,
        ..default()
    };
    // allocate the image memory
    let allocate_info = vk::MemoryAllocateInfo {
        allocation_size: memory_requirements.memory_requirements.size,
        memory_type_index: memory_type,
        ..default()
    }
    .push_next(&mut export_memory_allocate)
    .push_next(&mut memory_dedicated_allocate);
    let vk_memory = unsafe { vk_device.allocate_memory(&allocate_info, None) }?;

    // bind image to memory
    unsafe { vk_device.bind_image_memory(vk_image, vk_memory, 0) }?;

    // make wgpu resources out of vulkan resources

    let descriptor_hal = wgpu_hal::TextureDescriptor {
        label,
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: DEPTH,
        },
        mip_level_count: MIP_LEVELS,
        sample_count: WGPU_SAMPLES,
        dimension: WGPU_DIM,
        format: WGPU_FORMAT,
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
    let hal_texture =
        unsafe { hal_device.texture_from_raw(vk_image, &descriptor_hal, Some(drop_callback)) };
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
                    depth_or_array_layers: DEPTH,
                },
                mip_level_count: MIP_LEVELS,
                sample_count: WGPU_SAMPLES,
                dimension: WGPU_DIM,
                format: WGPU_FORMAT,
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
    })
}

fn get_drm_modifiers_for_format(
    vk_instance: &ash::Instance,
    vk_physical_device: vk::PhysicalDevice,
    vk_format: vk::Format,
) -> impl Iterator<Item = DrmModifier> {
    let mut drm_modifier_props = vk::DrmFormatModifierPropertiesList2EXT::default();
    let mut format_props = vk::FormatProperties2::default().push_next(&mut drm_modifier_props);
    unsafe {
        vk_instance.get_physical_device_format_properties2(
            vk_physical_device,
            vk_format,
            &mut format_props,
        );
    }
    let n_modifiers = drm_modifier_props.drm_format_modifier_count;

    let mut drm_modifiers = Vec::new();
    drm_modifiers.reserve_exact(n_modifiers as usize);
    let mut drm_modifier_props = vk::DrmFormatModifierPropertiesList2EXT {
        drm_format_modifier_count: drm_modifier_props.drm_format_modifier_count,
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

    (0..n_modifiers).map(move |i| {
        let format_props = unsafe {
            *drm_modifier_props
                .p_drm_format_modifier_properties
                .add(i as usize)
        };
        DrmModifier::from(format_props.drm_format_modifier)
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

    pub fn build_texture(&self) -> Result<gdk::Texture, BevyError> {
        let fd = self.open_fd()?;
        let (width, height) = (self.width(), self.height());
        let builder = gdk::DmabufTextureBuilder::new()
            .set_width(width)
            .set_height(height)
            .set_fourcc(DRM_FORMAT as u32)
            .set_modifier(0) // TODO
            .set_n_planes(1);

        // SAFETY: we use `build_with_release_func` to:
        // - move `fd` under the ownership of `gdk_texture`
        // - close `fd` when `gdk_texture` is destroyed
        let builder = unsafe { builder.set_fd(0, fd.as_raw_fd()) }
            .set_offset(0, 0)
            // <https://docs.kernel.org/userspace-api/dma-buf-alloc-exchange.html#term-stride>
            .set_stride(0, width * size_of::<u32>() as u32);

        // SAFETY: I have no clue what the invariants are.
        let gdk_texture = unsafe { builder.build_with_release_func(move || drop(fd))? };
        Ok(gdk_texture)
    }
}
