use {
    arrayvec::ArrayVec,
    ash::vk,
    bevy_ecs::error::BevyError,
    bevy_utils::default,
    derive_more::{Debug, Deref},
    drm_fourcc::{DrmFormat, DrmFourcc, DrmModifier},
    log::trace,
    std::os::fd::{AsRawFd as _, FromRawFd, OwnedFd},
};

/// [`wgpu::Texture`] which is backed by DMA buffers.
///
/// See <https://docs.kernel.org/userspace-api/dma-buf-alloc-exchange.html> for
/// documentation on Linux DMA buffers.
// SAFETY: This struct stores a reference to a `wgpu::Texture` and
// `vk::DeviceMemory` objects which are used by that texture. We use the memory
// to open fds for creating GTK dmabuf textures, so the texture must outlive the
// memory.
// When the texture is dropped, a drop callback frees the device memory.
// This struct always has at least 1 strong ref to the texture, so as long as
// this struct is alive, both the texture and memory are alive.
// It is safe for consumers to access the raw `wgpu::Texture`, since even if
// they have the texture and drop the `DmabufTexture`, the memory won't be
// dropped along with the `DmabufTexture`.
#[derive(Debug, Clone, Deref)]
pub struct DmabufTexture {
    #[debug(skip)]
    vk_instance: ash::Instance,
    #[debug(skip)]
    vk_device: ash::Device,
    #[deref]
    wgpu_texture: wgpu::Texture,
    drm_format: DrmFormat,
    #[debug(skip)]
    vk_memory: vk::DeviceMemory,
    planes: ArrayVec<DmabufPlane, MAX_PLANES_U>,
}

const MAX_PLANES: u32 = 4;
const MAX_PLANES_U: usize = MAX_PLANES as usize;

#[derive(Debug, Clone)]
struct DmabufPlane {
    offset: u32,
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

    /// Builds a [`gdk::Texture`] backed by a file descriptor to this DMA
    /// buffer.
    ///
    /// # Errors
    ///
    /// Errors if opening the plane file descriptors or building the
    /// [`gdk::DmabufTexture`] fails.
    pub fn build_gdk_texture(&self) -> Result<gdk::Texture, BevyError> {
        let (width, height) = (self.width(), self.height());
        let mut builder = gdk::DmabufTextureBuilder::new()
            .set_width(width)
            .set_height(height)
            .set_fourcc(self.drm_format.code as u32)
            .set_modifier(self.drm_format.modifier.into());

        let mut plane_fds = ArrayVec::<_, MAX_PLANES_U>::new();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "there should be no more than `u32::MAX` planes"
        )]
        {
            builder = builder.set_n_planes(self.planes.len() as u32);
            for (plane_index, plane) in self.planes.iter().enumerate() {
                let plane_index = plane_index as u32;
                let fd = self.open_fd()?;
                // SAFETY: we use `build_with_release_func` to:
                // - move `fd` under the ownership of `gdk_texture`
                // - close `fd` when `gdk_texture` is destroyed
                builder = unsafe { builder.set_fd(plane_index, fd.as_raw_fd()) }
                    .set_offset(plane_index, plane.offset)
                    .set_stride(plane_index, plane.stride);
                plane_fds.push(fd);
            }
        }

        // SAFETY: I have no clue what the safety invariants are.
        let gdk_texture = unsafe { builder.build_with_release_func(move || drop(plane_fds))? };
        Ok(gdk_texture)
    }

    fn open_fd(&self) -> Result<OwnedFd, BevyError> {
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

    // create an image with a potentially multi-planar layout
    let (vk_image, drm_modifier, plane_count) =
        unsafe { create_image(&dev, width, height, wgpu_format) }?;
    trace!(
        "Using DRM format {drm_format}:0x{:016x} with {plane_count} plane(s) ({drm_modifier:?} \
         vendor {:?})",
        u64::from(drm_modifier),
        drm_modifier.vendor(),
    );

    // <https://www.reddit.com/r/vulkan/comments/11r29hb/what_are_the_different_memory_allocation/>

    // get how much memory each plane uses, and make 1 big allocation for all of
    // them
    // - find offsets for each plane and store them for the DmabufTexture
    // - find a common alignment and memory type for the big allocation
    // - after we've done all planes, do the big allocation
    let mut allocation_size = 0u64;
    let mut memory_type_bits = u32::MAX;
    let mut planes = ArrayVec::new();
    let mut bind_plane_image_memory_list = ArrayVec::<_, MAX_PLANES_U>::new();
    for plane_index in 0..plane_count {
        let plane_aspect = match plane_index {
            0 => vk::ImageAspectFlags::MEMORY_PLANE_0_EXT,
            1 => vk::ImageAspectFlags::MEMORY_PLANE_1_EXT,
            2 => vk::ImageAspectFlags::MEMORY_PLANE_2_EXT,
            3 => vk::ImageAspectFlags::MEMORY_PLANE_3_EXT,
            _ => panic!("there should be no more than 4 memory planes"),
        };

        let plane_memory_requirements =
            unsafe { get_plane_memory_requirements(&dev, vk_image, plane_aspect) };

        let size = plane_memory_requirements.size;
        memory_type_bits &= plane_memory_requirements.memory_type_bits; // TODO: or `|=`?
        trace!("Plane {plane_index} requires {size} bytes");

        planes.push(DmabufPlane {
            offset: u32::try_from(allocation_size).expect("memory allocation too large"),
            stride: width * 4, // TODO
        });
        bind_plane_image_memory_list.push(vk::BindImagePlaneMemoryInfo {
            plane_aspect,
            ..default()
        });
        allocation_size = allocation_size
            .checked_add(size)
            .expect("memory allocation too large");
    }

    let vk_memory = unsafe { allocate_memory(&dev, allocation_size, memory_type_bits) }?;

    // iterator gymnastics to avoid aliasing mut refs
    let bind_image_memory_list = planes
        .iter_mut()
        .zip(bind_plane_image_memory_list.iter_mut())
        .map(|(plane, bind_plane_image_memory)| {
            vk::BindImageMemoryInfo {
                image: vk_image,
                memory: vk_memory,
                memory_offset: u64::from(plane.offset),
                ..default()
            }
            .push_next(bind_plane_image_memory)
        })
        .collect::<Box<[_]>>();
    unsafe { dev.vk_device.bind_image_memory2(&bind_image_memory_list) }?;

    let wgpu_texture = vk_texture_to_wgpu(&dev, vk_image, vk_memory, width, height, wgpu_format);
    Ok(DmabufTexture {
        vk_instance: dev.vk_instance.clone(),
        vk_device: dev.vk_device.clone(),
        wgpu_texture,
        drm_format: DrmFormat {
            code: drm_format,
            modifier: drm_modifier,
        },
        vk_memory,
        planes,
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

#[derive(Debug, Clone, Copy)]
struct DrmModifierInfo {
    modifier: DrmModifier,
    plane_count: u32,
}

unsafe fn get_drm_modifier_infos(
    dev: &Devices,
    wgpu_format: wgpu::TextureFormat,
) -> Box<[DrmModifierInfo]> {
    let vk_format = dev.hal_adapter.texture_format_as_raw(wgpu_format);

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
        .map(|props| DrmModifierInfo {
            modifier: DrmModifier::from(props.drm_format_modifier),
            plane_count: props.drm_format_modifier_plane_count,
        })
        .collect::<Box<[_]>>()
}

unsafe fn create_image(
    dev: &Devices,
    width: u32,
    height: u32,
    wgpu_format: wgpu::TextureFormat,
) -> Result<(vk::Image, DrmModifier, u32), BevyError> {
    let vk_format = dev.hal_adapter.texture_format_as_raw(wgpu_format);

    // for this texture format, figure out what DRM modifiers we can use
    let drm_modifier_infos = unsafe { get_drm_modifier_infos(dev, wgpu_format) };
    trace!("Available DRM format modifiers");
    for info in &drm_modifier_infos {
        trace!(
            "- 0x{:016x} with {} plane(s) ({:?} vendor {:?})",
            u64::from(info.modifier),
            info.plane_count,
            info.modifier,
            info.modifier.vendor(),
        );
    }

    // we tell the device that we can make an image with any of the above modifiers,
    // we're not picky
    let drm_modifiers = drm_modifier_infos
        .iter()
        .map(|info| u64::from(info.modifier))
        .collect::<Box<[_]>>();
    let mut with_drm_modifiers = vk::ImageDrmFormatModifierListCreateInfoEXT {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "there will be no more than `u32::MAX` modifiers"
        )]
        drm_format_modifier_count: drm_modifiers.len() as u32,
        p_drm_format_modifiers: drm_modifiers.as_ptr(),
        ..default()
    };

    // our image can be backed by external memory
    let mut with_external_memory = vk::ExternalMemoryImageCreateInfo {
        handle_types: MEMORY_HANDLE_TYPE,
        ..default()
    };

    let params = vk::ImageCreateInfo {
        flags:
            // We must bind each plane separately, since we need to know the
            // memory offset of each plane. So we have one memory allocation but
            // multiple image plane binds into that one allocation, at different
            // offsets.
            // This is because when we import a dmabuf in GTK, we need to
            // specify the memory planes in the image; so we need to get each
            // plane's offset ourselves.
            vk::ImageCreateFlags::DISJOINT
            // This prevents validation errors when using a single-planar
            // `wgpu_format` with the `DISJOINT` flag.
            //
            // <https://registry.khronos.org/vulkan/specs/latest/man/html/VkImageCreateFlagBits.html>
            // `VK_IMAGE_CREATE_ALIAS_BIT`
            //
            //     This flag further specifies that [...] a single-plane image
            //     can share an in-memory non-linear representation with a plane
            //     of a multi-planar disjoint image [...]
            //
            //     If the pNext chain includes a VkExternalMemoryImageCreateInfo
            //     or VkExternalMemoryImageCreateInfoNV structure whose
            //     handleTypes member is not 0 [which we do], it is as if
            //     `VK_IMAGE_CREATE_ALIAS_BIT` is set.
            //
            | vk::ImageCreateFlags::ALIAS,
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
    let vk_image = unsafe { dev.vk_device.create_image(&params, None) }?;

    // when we create the image, we give the GPU a list of what DRM modifiers it
    // *could* use, but which one it chooses is implementation-specific.
    // after creating the image, we query which modifier it actually chose.
    let drm_modifier = {
        let mut out = vk::ImageDrmFormatModifierPropertiesEXT::default();
        let device =
            ash::ext::image_drm_format_modifier::Device::new(dev.vk_instance, dev.vk_device);
        unsafe { device.get_image_drm_format_modifier_properties(vk_image, &mut out) }?;
        DrmModifier::from(out.drm_format_modifier)
    };

    let drm_modifier_info = drm_modifier_infos
        .iter()
        .find(|info| info.modifier == drm_modifier)
        .unwrap_or_else(|| {
            panic!(
                "created an image with DRM modifier {drm_modifier:?}, but this was not in the \
                 initial modifier list - Vulkan driver bug?"
            )
        });

    Ok((
        vk_image,
        drm_modifier_info.modifier,
        drm_modifier_info.plane_count,
    ))
}

unsafe fn get_plane_memory_requirements(
    dev: &Devices,
    vk_image: vk::Image,
    plane_aspect: vk::ImageAspectFlags,
) -> vk::MemoryRequirements {
    let mut image_plane_memory_requirements = vk::ImagePlaneMemoryRequirementsInfo {
        plane_aspect,
        ..default()
    };
    let image_memory_requirements = vk::ImageMemoryRequirementsInfo2 {
        image: vk_image,
        ..default()
    }
    .push_next(&mut image_plane_memory_requirements);
    let mut out = vk::MemoryRequirements2::default();
    unsafe {
        dev.vk_device
            .get_image_memory_requirements2(&image_memory_requirements, &mut out);
    }
    out.memory_requirements
}

unsafe fn allocate_memory(
    dev: &Devices,
    allocation_size: vk::DeviceSize,
    memory_type_bits: u32,
) -> Result<vk::DeviceMemory, BevyError> {
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
    // let mut with_dedicated = vk::MemoryDedicatedAllocateInfo {
    //     image: vk_image,
    //     ..default()
    // };
    // this memory must be exportable
    let mut with_export = vk::ExportMemoryAllocateInfo {
        handle_types: MEMORY_HANDLE_TYPE,
        ..default()
    };

    let params = vk::MemoryAllocateInfo {
        allocation_size,
        memory_type_index,
        ..default()
    }
    .push_next(&mut with_export);
    // .push_next(&mut with_dedicated);
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

    let wgpu_descriptor = wgpu::TextureDescriptor {
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
    };
    // SAFETY:
    // - `hal_texture` was just created from the device's internal handle
    // - `hal_texture`'s descriptor is the same as the descriptor we're making now,
    //   enforced via the `VK_` and `WGPU_` variable parity
    // - `hal_texture` has just been initialized
    unsafe {
        dev.wgpu_device
            .create_texture_from_hal::<wgpu_hal::vulkan::Api>(hal_texture, &wgpu_descriptor)
    }
}

fn format_to_fourcc(format: wgpu::TextureFormat) -> Option<DrmFourcc> {
    use {DrmFourcc as Cc, wgpu::TextureFormat as Tf};
    match format {
        Tf::Rgba8Unorm | Tf::Rgba8UnormSrgb => Some(Cc::Abgr8888),
        _ => None, // TODO
    }
}
