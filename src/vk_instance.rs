use std::ffi::{CStr, CString};
use std::sync::Arc;

use ash::extensions::{ext, khr};
use ash::vk;
use thiserror::Error;
use wgpu_hal::{vulkan, InstanceDescriptor};

fn cstr_from_bytes_until_nul(bytes: &[std::os::raw::c_char]) -> Option<&std::ffi::CStr> {
    if bytes.contains(&0) {
        // Safety for `CStr::from_ptr`:
        // - We've ensured that the slice does contain a null terminator.
        // - The range is valid to read, because the slice covers it.
        // - The memory won't be changed, because the slice borrows it.
        unsafe { Some(std::ffi::CStr::from_ptr(bytes.as_ptr())) }
    } else {
        None
    }
}

pub unsafe fn init(
    desc: &InstanceDescriptor,
    extra_extensions: impl IntoIterator<Item = &'static CStr>,
) -> Result<vulkan::Instance, InstanceError> {
    let entry = unsafe { ash::Entry::load() }.map_err(|err| {
        InstanceError::with_source(String::from("missing Vulkan entry points"), err)
    })?;
    let version = { entry.try_enumerate_instance_version() };
    let instance_api_version = match version {
        // Vulkan 1.1+
        Ok(Some(version)) => version,
        Ok(None) => vk::API_VERSION_1_0,
        Err(err) => {
            return Err(InstanceError::with_source(
                String::from("try_enumerate_instance_version() failed"),
                err,
            ));
        }
    };

    let app_name = CString::new(desc.name).unwrap();
    let app_info = vk::ApplicationInfo::builder()
        .application_name(app_name.as_c_str())
        .application_version(1)
        .engine_name(CStr::from_bytes_with_nul(b"wgpu-hal\0").unwrap())
        .engine_version(2)
        .api_version(
            // Vulkan 1.0 doesn't like anything but 1.0 passed in here...
            if instance_api_version < vk::API_VERSION_1_1 {
                vk::API_VERSION_1_0
            } else {
                // This is the max Vulkan API version supported by `wgpu-hal`.
                //
                // If we want to increment this, there are some things that must be done first:
                //  - Audit the behavioral differences between the previous and new API versions.
                //  - Audit all extensions used by this backend:
                //    - If any were promoted in the new API version and the behavior has changed, we must handle the new behavior in addition to the old behavior.
                //    - If any were obsoleted in the new API version, we must implement a fallback for the new API version
                //    - If any are non-KHR-vendored, we must ensure the new behavior is still correct (since backwards-compatibility is not guaranteed).
                vk::API_VERSION_1_3
            },
        );

    let mut extensions = desired_extensions(&entry, instance_api_version, desc.flags)?;
    extensions.extend(extra_extensions);

    let instance_layers = { entry.enumerate_instance_layer_properties() };
    let instance_layers = instance_layers.map_err(|e| {
        log::debug!("enumerate_instance_layer_properties: {:?}", e);
        InstanceError::with_source(
            String::from("enumerate_instance_layer_properties() failed"),
            e,
        )
    })?;

    fn find_layer<'layers>(
        instance_layers: &'layers [vk::LayerProperties],
        name: &CStr,
    ) -> Option<&'layers vk::LayerProperties> {
        instance_layers
            .iter()
            .find(|inst_layer| cstr_from_bytes_until_nul(&inst_layer.layer_name) == Some(name))
    }

    let validation_layer_name =
        CStr::from_bytes_with_nul(b"VK_LAYER_KHRONOS_validation\0").unwrap();
    let validation_layer_properties = find_layer(&instance_layers, validation_layer_name);

    // Determine if VK_EXT_validation_features is available, so we can enable
    // GPU assisted validation and synchronization validation.
    let validation_features_are_enabled = if validation_layer_properties.is_some() {
        // Get the all the instance extension properties.
        let exts = enumerate_instance_extension_properties(&entry, Some(validation_layer_name))?;
        // Convert all the names of the extensions into an iterator of CStrs.
        let mut ext_names = exts
            .iter()
            .filter_map(|ext| cstr_from_bytes_until_nul(&ext.extension_name));
        // Find the validation features extension.
        ext_names.any(|ext_name| ext_name == vk::ExtValidationFeaturesFn::name())
    } else {
        false
    };

    let should_enable_gpu_based_validation = desc
        .flags
        .intersects(wgpu::InstanceFlags::GPU_BASED_VALIDATION)
        && validation_features_are_enabled;

    let nv_optimus_layer = CStr::from_bytes_with_nul(b"VK_LAYER_NV_optimus\0").unwrap();
    let has_nv_optimus = find_layer(&instance_layers, nv_optimus_layer).is_some();

    let obs_layer = CStr::from_bytes_with_nul(b"VK_LAYER_OBS_HOOK\0").unwrap();
    let has_obs_layer = find_layer(&instance_layers, obs_layer).is_some();

    let mut layers: Vec<&'static CStr> = Vec::new();

    let has_debug_extension = extensions.contains(&ext::DebugUtils::name());
    // let mut debug_user_data = has_debug_extension.then(|| {
    //     // Put the callback data on the heap, to ensure it will never be
    //     // moved.
    //     Box::new(DebugUtilsMessengerUserData {
    //         validation_layer_properties: None,
    //         has_obs_layer,
    //     })
    // });

    // Request validation layer if asked.
    if desc.flags.intersects(wgpu::InstanceFlags::VALIDATION) || should_enable_gpu_based_validation
    {
        if let Some(layer_properties) = validation_layer_properties {
            layers.push(validation_layer_name);

            // if let Some(debug_user_data) = debug_user_data.as_mut() {
            //     debug_user_data.validation_layer_properties =
            //         Some(super::ValidationLayerProperties {
            //             layer_description: cstr_from_bytes_until_nul(&layer_properties.description)
            //                 .unwrap()
            //                 .to_owned(),
            //             layer_spec_version: layer_properties.spec_version,
            //         });
            // }
        } else {
            log::warn!(
                "InstanceFlags::VALIDATION requested, but unable to find layer: {}",
                validation_layer_name.to_string_lossy()
            );
        }
    }
    // let mut debug_utils = if let Some(callback_data) = debug_user_data {
    //     // having ERROR unconditionally because Vk doesn't like empty flags
    //     let mut severity = vk::DebugUtilsMessageSeverityFlagsEXT::ERROR;
    //     if log::max_level() >= log::LevelFilter::Debug {
    //         severity |= vk::DebugUtilsMessageSeverityFlagsEXT::VERBOSE;
    //     }
    //     if log::max_level() >= log::LevelFilter::Info {
    //         severity |= vk::DebugUtilsMessageSeverityFlagsEXT::INFO;
    //     }
    //     if log::max_level() >= log::LevelFilter::Warn {
    //         severity |= vk::DebugUtilsMessageSeverityFlagsEXT::WARNING;
    //     }

    //     let message_type = vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
    //         | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
    //         | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE;

    //     let create_info = DebugUtilsCreateInfo {
    //         severity,
    //         message_type,
    //         callback_data,
    //     };

    //     let vk_create_info = create_info.to_vk_create_info().build();

    //     Some((create_info, vk_create_info))
    // } else {
    //     None
    // };

    #[cfg(target_os = "android")]
    let android_sdk_version = {
        let properties = android_system_properties::AndroidSystemProperties::new();
        // See: https://developer.android.com/reference/android/os/Build.VERSION_CODES
        if let Some(val) = properties.get("ro.build.version.sdk") {
            match val.parse::<u32>() {
                Ok(sdk_ver) => sdk_ver,
                Err(err) => {
                    log::error!(
                        "Couldn't parse Android's ro.build.version.sdk system property ({val}): {err}"
                    );
                    0
                }
            }
        } else {
            log::error!("Couldn't read Android's ro.build.version.sdk system property");
            0
        }
    };
    #[cfg(not(target_os = "android"))]
    let android_sdk_version = 0;

    let mut flags = vk::InstanceCreateFlags::empty();

    // Avoid VUID-VkInstanceCreateInfo-flags-06559: Only ask the instance to
    // enumerate incomplete Vulkan implementations (which we need on Mac) if
    // we managed to find the extension that provides the flag.
    if extensions.contains(&ash::vk::KhrPortabilityEnumerationFn::name()) {
        flags |= vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR;
    }
    let vk_instance = {
        let str_pointers = layers
            .iter()
            .chain(extensions.iter())
            .map(|&s: &&'static _| {
                // Safe because `layers` and `extensions` entries have static lifetime.
                s.as_ptr()
            })
            .collect::<Vec<_>>();

        let mut create_info = vk::InstanceCreateInfo::builder()
            .flags(flags)
            .application_info(&app_info)
            .enabled_layer_names(&str_pointers[..layers.len()])
            .enabled_extension_names(&str_pointers[layers.len()..]);

        // if let Some(&mut (_, ref mut vk_create_info)) = debug_utils.as_mut() {
        //     create_info = create_info.push_next(vk_create_info);
        // }

        // Enable explicit validation features if available
        let mut validation_features;
        let mut validation_feature_list: Vec<_>;
        if validation_features_are_enabled {
            validation_feature_list = Vec::new();

            // Always enable synchronization validation
            validation_feature_list
                .push(vk::ValidationFeatureEnableEXT::SYNCHRONIZATION_VALIDATION);

            // Only enable GPU assisted validation if requested.
            if should_enable_gpu_based_validation {
                validation_feature_list.push(vk::ValidationFeatureEnableEXT::GPU_ASSISTED);
                validation_feature_list
                    .push(vk::ValidationFeatureEnableEXT::GPU_ASSISTED_RESERVE_BINDING_SLOT);
            }

            validation_features = vk::ValidationFeaturesEXT::builder()
                .enabled_validation_features(&validation_feature_list);
            create_info = create_info.push_next(&mut validation_features);
        }

        unsafe { entry.create_instance(&create_info, None) }.map_err(|e| {
            InstanceError::with_source(String::from("Entry::create_instance() failed"), e)
        })?
    };

    unsafe {
        Ok(vulkan::Instance::from_raw(
            entry,
            vk_instance,
            instance_api_version,
            android_sdk_version,
            None, // debug_utils.map(|(i, _)| i),
            extensions,
            desc.flags,
            has_nv_optimus,
            Some(Box::new(())), // `Some` signals that wgpu-hal is in charge of destroying vk_instance
        )
        .unwrap()) // TODO
    }
}

fn desired_extensions(
    entry: &ash::Entry,
    _instance_api_version: u32,
    flags: wgpu::InstanceFlags,
) -> Result<Vec<&'static CStr>, InstanceError> {
    let instance_extensions = enumerate_instance_extension_properties(entry, None)?;

    // Check our extensions against the available extensions
    let mut extensions: Vec<&'static CStr> = Vec::new();

    // VK_KHR_surface
    extensions.push(khr::Surface::name());

    // Platform-specific WSI extensions
    if cfg!(all(
        unix,
        not(target_os = "android"),
        not(target_os = "macos")
    )) {
        // VK_KHR_xlib_surface
        extensions.push(khr::XlibSurface::name());
        // VK_KHR_xcb_surface
        extensions.push(khr::XcbSurface::name());
        // VK_KHR_wayland_surface
        extensions.push(khr::WaylandSurface::name());
    }
    if cfg!(target_os = "android") {
        // VK_KHR_android_surface
        extensions.push(khr::AndroidSurface::name());
    }
    if cfg!(target_os = "windows") {
        // VK_KHR_win32_surface
        extensions.push(khr::Win32Surface::name());
    }
    if cfg!(target_os = "macos") {
        // VK_EXT_metal_surface
        extensions.push(ext::MetalSurface::name());
        extensions.push(ash::vk::KhrPortabilityEnumerationFn::name());
    }

    if flags.contains(wgpu::InstanceFlags::DEBUG) {
        // VK_EXT_debug_utils
        extensions.push(ext::DebugUtils::name());
    }

    // VK_EXT_swapchain_colorspace
    // Provides wide color gamut
    extensions.push(vk::ExtSwapchainColorspaceFn::name());

    // VK_KHR_get_physical_device_properties2
    // Even though the extension was promoted to Vulkan 1.1, we still require the extension
    // so that we don't have to conditionally use the functions provided by the 1.1 instance
    extensions.push(vk::KhrGetPhysicalDeviceProperties2Fn::name());

    // Only keep available extensions.
    extensions.retain(|&ext| {
        if instance_extensions
            .iter()
            .any(|inst_ext| cstr_from_bytes_until_nul(&inst_ext.extension_name) == Some(ext))
        {
            true
        } else {
            log::warn!("Unable to find extension: {}", ext.to_string_lossy());
            false
        }
    });
    Ok(extensions)
}

fn enumerate_instance_extension_properties(
    entry: &ash::Entry,
    layer_name: Option<&CStr>,
) -> Result<Vec<vk::ExtensionProperties>, InstanceError> {
    let instance_extensions = { entry.enumerate_instance_extension_properties(layer_name) };
    instance_extensions.map_err(|e| {
        InstanceError::with_source(
            String::from("enumerate_instance_extension_properties() failed"),
            e,
        )
    })
}

/// Error occurring while trying to create an instance, or create a surface from an instance;
/// typically relating to the state of the underlying graphics API or hardware.
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct InstanceError {
    /// These errors are very platform specific, so do not attempt to encode them as an enum.
    ///
    /// This message should describe the problem in sufficient detail to be useful for a
    /// user-to-developer “why won't this work on my machine” bug report, and otherwise follow
    /// <https://rust-lang.github.io/api-guidelines/interoperability.html#error-types-are-meaningful-and-well-behaved-c-good-err>.
    message: String,

    /// Underlying error value, if any is available.
    #[source]
    source: Option<Arc<dyn std::error::Error + Send + Sync + 'static>>,
}

impl InstanceError {
    #[allow(dead_code)] // may be unused on some platforms
    pub(crate) fn new(message: String) -> Self {
        Self {
            message,
            source: None,
        }
    }
    #[allow(dead_code)] // may be unused on some platforms
    pub(crate) fn with_source(
        message: String,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            message,
            source: Some(Arc::new(source)),
        }
    }
}
