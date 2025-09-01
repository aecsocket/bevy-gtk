use {
    ash::vk,
    bevy_derive::Deref,
    bevy_ecs::error::BevyError,
    bevy_utils::default,
    drm_fourcc::DrmFourcc,
    std::os::fd::{AsRawFd as _, FromRawFd, OwnedFd},
};

/// [`wgpu::Texture`] which is backed by Linux dmabuf memory.
#[derive(Deref)]
pub struct DmabufTexture {
    instance_vk: ash::Instance,
    device_vk: ash::Device,
    #[deref]
    texture_wg: wgpu::Texture,
    memory_vk: vk::DeviceMemory,
}

impl DmabufTexture {
    /// Creates a dmabuf-backed texture on a Vulkan [`wgpu::Device`].
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Result<Self, BevyError> {
        // SAFETY: `hal_device` is not manually destroyed by us
        let hal_device = unsafe { device.as_hal::<wgpu_hal::vulkan::Api>() }
            .expect("render device is not a Vulkan device");
        create_dmabuf_texture(device, &*hal_device, width, height)
    }
}

const MEMORY_HANDLE_TYPE_VK: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;

const FORMAT_WG: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const FORMAT_VK: vk::Format = vk::Format::R8G8B8A8_SRGB;
const FORMAT_FOURCC: DrmFourcc = DrmFourcc::Abgr8888;

fn create_dmabuf_texture(
    device_wg: &wgpu::Device,
    device_hal: &wgpu_hal::vulkan::Device,
    width: u32,
    height: u32,
) -> Result<DmabufTexture, BevyError> {
    const LABEL: &str = "bevy_gtk render target";
    const MIP_LEVELS: u32 = 1;
    const DEPTH: u32 = 1;
    const SAMPLES_VK: vk::SampleCountFlags = vk::SampleCountFlags::TYPE_1;
    const SAMPLES_WG: u32 = 1;
    const DIM_VK: vk::ImageType = vk::ImageType::TYPE_2D;
    const DIM_WG: wgpu::TextureDimension = wgpu::TextureDimension::D2;
    let usage_vk: vk::ImageUsageFlags =
        vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::COLOR_ATTACHMENT;
    let usage_hal: wgpu::TextureUses =
        wgpu::TextureUses::COPY_SRC | wgpu::TextureUses::COLOR_TARGET;
    let usage_wg: wgpu::TextureUsages =
        wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::RENDER_ATTACHMENT;

    let device_vk = device_hal.raw_device();
    let instance_vk = device_hal.shared_instance().raw_instance();

    // image can be backed by external memory
    let mut external_memory_image_create = vk::ExternalMemoryImageCreateInfo {
        handle_types: MEMORY_HANDLE_TYPE_VK,
        ..default()
    };
    // image tiling is defined by a DRM format modifier
    // TODO: what types do we support?
    let drm_format_modifiers = [
        DrmFourcc::Abgr8888,
        DrmFourcc::Abgr8888,
        DrmFourcc::Abgr8888,
        DrmFourcc::Abgr8888,
    ];
    let mut image_drm_format_modifier_list_create = vk::ImageDrmFormatModifierListCreateInfoEXT {
        drm_format_modifier_count: drm_format_modifiers.len() as u32,
        p_drm_format_modifiers: &drm_format_modifiers as *const _ as *const u64,
        ..default()
    };
    // make the image
    let image_create = vk::ImageCreateInfo {
        image_type: DIM_VK,
        format: FORMAT_VK,
        extent: vk::Extent3D {
            width,
            height,
            depth: DEPTH,
        },
        mip_levels: MIP_LEVELS,
        array_layers: 1,
        samples: SAMPLES_VK,
        tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
        usage: usage_vk,
        sharing_mode: vk::SharingMode::EXCLUSIVE,
        initial_layout: vk::ImageLayout::UNDEFINED,
        ..default()
    }
    .push_next(&mut image_drm_format_modifier_list_create)
    .push_next(&mut external_memory_image_create);
    let image_vk = unsafe { device_vk.create_image(&image_create, None) }?;

    // to allocate memory for the image, we get what requirements the memory has for
    // this image

    // out parameter for the requirements
    let mut memory_requirements = vk::MemoryRequirements2::default();
    // memory requirements are based on the image we just made
    let image_memory_requirements = vk::ImageMemoryRequirementsInfo2 {
        image: image_vk,
        ..default()
    };
    // get the requirements
    unsafe {
        device_vk
            .get_image_memory_requirements2(&image_memory_requirements, &mut memory_requirements);
    }

    // TODO
    let memory_type = find_memory_type(memory_requirements.memory_requirements.memory_type_bits);

    // this memory will be bound to exactly one image
    let mut memory_dedicated_allocate = vk::MemoryDedicatedAllocateInfo {
        image: image_vk,
        ..default()
    };
    // this memory must be exportable
    let mut export_memory_allocate = vk::ExportMemoryAllocateInfo {
        handle_types: MEMORY_HANDLE_TYPE_VK,
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
    let memory_vk = unsafe { device_vk.allocate_memory(&allocate_info, None) }?;

    // bind image to memory
    unsafe { device_vk.bind_image_memory(image_vk, memory_vk, 0) }?;

    // make wgpu resources out of vulkan resources

    let descriptor_hal = wgpu_hal::TextureDescriptor {
        label: Some(LABEL),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: DEPTH,
        },
        mip_level_count: MIP_LEVELS,
        sample_count: SAMPLES_WG,
        dimension: DIM_WG,
        format: FORMAT_WG,
        usage: usage_hal,
        memory_flags: wgpu_hal::MemoryFlags::empty(),
        view_formats: Vec::new(),
    };
    let drop_callback = {
        let vk_device = device_vk.clone();
        Box::new(move || unsafe {
            vk_device.destroy_image(image_vk, None);
            vk_device.free_memory(memory_vk, None);
        })
    };
    let texture_hal =
        unsafe { device_hal.texture_from_raw(image_vk, &descriptor_hal, Some(drop_callback)) };
    // SAFETY:
    // - `hal_texture` was just created from the device's internal handle
    // - `hal_texture`'s descriptor is the same as the descriptor we're making now,
    //   enforced via the `_VK` and `_WG` variable parity
    // - `hal_texture` has just been initialized
    let texture_wg = unsafe {
        device_wg.create_texture_from_hal::<wgpu_hal::vulkan::Api>(
            texture_hal,
            &wgpu::TextureDescriptor {
                label: Some(LABEL),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: DEPTH,
                },
                mip_level_count: MIP_LEVELS,
                sample_count: SAMPLES_WG,
                dimension: DIM_WG,
                format: FORMAT_WG,
                usage: usage_wg,
                view_formats: &[],
            },
        )
    };

    Ok(DmabufTexture {
        instance_vk: instance_vk.clone(),
        device_vk: device_vk.clone(),
        texture_wg,
        memory_vk,
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
            memory: self.memory_vk,
            handle_type: MEMORY_HANDLE_TYPE_VK,
            ..default()
        };
        let raw_fd = unsafe {
            ash::khr::external_memory_fd::Device::new(&self.instance_vk, &self.device_vk)
                .get_memory_fd(&get_fd_info)
        }?;
        // SAFETY: Vulkan just created a new open fd for us.
        // <https://registry.khronos.org/vulkan/specs/latest/man/html/vkGetMemoryFdKHR.html>
        //
        //     Each call to vkGetMemoryFdKHR must create a new file descriptor...
        //
        Ok(unsafe { OwnedFd::from_raw_fd(raw_fd) })
    }
}

pub fn gtk_dmabuf(texture: &DmabufTexture) -> Result<gdk::Texture, BevyError> {
    let fd = texture.open_fd()?;
    let dmabuf_builder = gdk::DmabufTextureBuilder::new()
        .set_width(texture.width())
        .set_height(texture.height())
        .set_fourcc(FORMAT_FOURCC as u32)
        .set_modifier(0) // TODO
        .set_n_planes(1);

    // plane 0
    // SAFETY: we use `build_with_release_func` to:
    // - move `fd` under the ownership of `gdk_texture`
    // - close `fd` when `gdk_texture` is destroyed
    let dmabuf_builder = unsafe { dmabuf_builder.set_fd(0, fd.as_raw_fd()) }
        .set_offset(0, 0)
        .set_stride(0, texture.width() * 4); // bytes per row?

    // SAFETY: I have no clue what the invariants are.
    let gdk_texture = unsafe {
        dmabuf_builder.build_with_release_func(move || {
            drop(fd);
        })?
    };
    Ok(gdk_texture)
}
