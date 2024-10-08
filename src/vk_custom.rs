use std::ffi::CStr;

use ash::vk;
use bevy::render::settings::{WgpuSettings, WgpuSettingsPriority};
use wgpu as wgt;
use wgpu_hal::{vulkan, DeviceError, OpenDevice};

pub unsafe fn open_adapter(
    adapter: &vulkan::Adapter,
    features: wgt::Features,
    extra_extensions: impl IntoIterator<Item = &'static CStr>,
) -> Result<OpenDevice<vulkan::Api>, DeviceError> {
    let mut enabled_extensions = adapter.required_device_extensions(features);
    enabled_extensions.extend(extra_extensions);
    let mut enabled_phd_features = adapter.physical_device_features(&enabled_extensions, features);

    let family_index = 0; //TODO
    let family_info = vk::DeviceQueueCreateInfo::builder()
        .queue_family_index(family_index)
        .queue_priorities(&[1.0])
        .build();
    let family_infos = [family_info];

    let str_pointers = enabled_extensions
        .iter()
        .map(|&s| {
            // Safe because `enabled_extensions` entries have static lifetime.
            s.as_ptr()
        })
        .collect::<Vec<_>>();

    let pre_info = vk::DeviceCreateInfo::builder()
        .queue_create_infos(&family_infos)
        .enabled_extension_names(&str_pointers);
    let info = enabled_phd_features
        .add_to_device_create_builder(pre_info)
        .build();
    let raw_device = {
        unsafe {
            adapter.shared_instance().raw_instance().create_device(
                adapter.raw_physical_device(),
                &info,
                None,
            )?
        }
    };

    unsafe {
        adapter.device_from_raw(
            raw_device,
            true,
            &enabled_extensions,
            features,
            family_info.queue_family_index,
            0,
        )
    }
}

pub fn make_device_descriptor<'a>(
    settings: &'a WgpuSettings,
    adapter: &wgpu::Adapter,
    adapter_info: &wgpu::AdapterInfo,
) -> wgpu::DeviceDescriptor<'a> {
    let mut features = wgpu::Features::empty();
    let mut limits = settings.limits.clone();
    if matches!(settings.priority, WgpuSettingsPriority::Functionality) {
        features = adapter.features();
        if adapter_info.device_type == wgpu::DeviceType::DiscreteGpu {
            // `MAPPABLE_PRIMARY_BUFFERS` can have a significant, negative performance impact for
            // discrete GPUs due to having to transfer data across the PCI-E bus and so it
            // should not be automatically enabled in this case. It is however beneficial for
            // integrated GPUs.
            features -= wgpu::Features::MAPPABLE_PRIMARY_BUFFERS;
        }

        // RAY_QUERY and RAY_TRACING_ACCELERATION STRUCTURE will sometimes cause DeviceLost failures on platforms
        // that report them as supported:
        // <https://github.com/gfx-rs/wgpu/issues/5488>
        // WGPU also currently doesn't actually support these features yet, so we should disable
        // them until they are safe to enable.
        features -= wgpu::Features::RAY_QUERY;
        features -= wgpu::Features::RAY_TRACING_ACCELERATION_STRUCTURE;

        limits = adapter.limits();
    }

    // Enforce the disabled features
    if let Some(disabled_features) = settings.disabled_features {
        features -= disabled_features;
    }
    // NOTE: |= is used here to ensure that any explicitly-enabled features are respected.
    features |= settings.features;

    // Enforce the limit constraints
    if let Some(constrained_limits) = settings.constrained_limits.as_ref() {
        // NOTE: Respect the configured limits as an 'upper bound'. This means for 'max' limits, we
        // take the minimum of the calculated limits according to the adapter/backend and the
        // specified max_limits. For 'min' limits, take the maximum instead. This is intended to
        // err on the side of being conservative. We can't claim 'higher' limits that are supported
        // but we can constrain to 'lower' limits.
        limits = wgpu::Limits {
            max_texture_dimension_1d: limits
                .max_texture_dimension_1d
                .min(constrained_limits.max_texture_dimension_1d),
            max_texture_dimension_2d: limits
                .max_texture_dimension_2d
                .min(constrained_limits.max_texture_dimension_2d),
            max_texture_dimension_3d: limits
                .max_texture_dimension_3d
                .min(constrained_limits.max_texture_dimension_3d),
            max_texture_array_layers: limits
                .max_texture_array_layers
                .min(constrained_limits.max_texture_array_layers),
            max_bind_groups: limits
                .max_bind_groups
                .min(constrained_limits.max_bind_groups),
            max_dynamic_uniform_buffers_per_pipeline_layout: limits
                .max_dynamic_uniform_buffers_per_pipeline_layout
                .min(constrained_limits.max_dynamic_uniform_buffers_per_pipeline_layout),
            max_dynamic_storage_buffers_per_pipeline_layout: limits
                .max_dynamic_storage_buffers_per_pipeline_layout
                .min(constrained_limits.max_dynamic_storage_buffers_per_pipeline_layout),
            max_sampled_textures_per_shader_stage: limits
                .max_sampled_textures_per_shader_stage
                .min(constrained_limits.max_sampled_textures_per_shader_stage),
            max_samplers_per_shader_stage: limits
                .max_samplers_per_shader_stage
                .min(constrained_limits.max_samplers_per_shader_stage),
            max_storage_buffers_per_shader_stage: limits
                .max_storage_buffers_per_shader_stage
                .min(constrained_limits.max_storage_buffers_per_shader_stage),
            max_storage_textures_per_shader_stage: limits
                .max_storage_textures_per_shader_stage
                .min(constrained_limits.max_storage_textures_per_shader_stage),
            max_uniform_buffers_per_shader_stage: limits
                .max_uniform_buffers_per_shader_stage
                .min(constrained_limits.max_uniform_buffers_per_shader_stage),
            max_uniform_buffer_binding_size: limits
                .max_uniform_buffer_binding_size
                .min(constrained_limits.max_uniform_buffer_binding_size),
            max_storage_buffer_binding_size: limits
                .max_storage_buffer_binding_size
                .min(constrained_limits.max_storage_buffer_binding_size),
            max_vertex_buffers: limits
                .max_vertex_buffers
                .min(constrained_limits.max_vertex_buffers),
            max_vertex_attributes: limits
                .max_vertex_attributes
                .min(constrained_limits.max_vertex_attributes),
            max_vertex_buffer_array_stride: limits
                .max_vertex_buffer_array_stride
                .min(constrained_limits.max_vertex_buffer_array_stride),
            max_push_constant_size: limits
                .max_push_constant_size
                .min(constrained_limits.max_push_constant_size),
            min_uniform_buffer_offset_alignment: limits
                .min_uniform_buffer_offset_alignment
                .max(constrained_limits.min_uniform_buffer_offset_alignment),
            min_storage_buffer_offset_alignment: limits
                .min_storage_buffer_offset_alignment
                .max(constrained_limits.min_storage_buffer_offset_alignment),
            max_inter_stage_shader_components: limits
                .max_inter_stage_shader_components
                .min(constrained_limits.max_inter_stage_shader_components),
            max_compute_workgroup_storage_size: limits
                .max_compute_workgroup_storage_size
                .min(constrained_limits.max_compute_workgroup_storage_size),
            max_compute_invocations_per_workgroup: limits
                .max_compute_invocations_per_workgroup
                .min(constrained_limits.max_compute_invocations_per_workgroup),
            max_compute_workgroup_size_x: limits
                .max_compute_workgroup_size_x
                .min(constrained_limits.max_compute_workgroup_size_x),
            max_compute_workgroup_size_y: limits
                .max_compute_workgroup_size_y
                .min(constrained_limits.max_compute_workgroup_size_y),
            max_compute_workgroup_size_z: limits
                .max_compute_workgroup_size_z
                .min(constrained_limits.max_compute_workgroup_size_z),
            max_compute_workgroups_per_dimension: limits
                .max_compute_workgroups_per_dimension
                .min(constrained_limits.max_compute_workgroups_per_dimension),
            max_buffer_size: limits
                .max_buffer_size
                .min(constrained_limits.max_buffer_size),
            max_bindings_per_bind_group: limits
                .max_bindings_per_bind_group
                .min(constrained_limits.max_bindings_per_bind_group),
            max_non_sampler_bindings: limits
                .max_non_sampler_bindings
                .min(constrained_limits.max_non_sampler_bindings),
            max_color_attachments: limits
                .max_color_attachments
                .min(constrained_limits.max_color_attachments),
            max_color_attachment_bytes_per_sample: limits
                .max_color_attachment_bytes_per_sample
                .min(constrained_limits.max_color_attachment_bytes_per_sample),
            min_subgroup_size: limits
                .min_subgroup_size
                .max(constrained_limits.min_subgroup_size),
            max_subgroup_size: limits
                .max_subgroup_size
                .min(constrained_limits.max_subgroup_size),
        };
    }

    wgpu::DeviceDescriptor {
        label: settings.device_label.as_ref().map(|a| a.as_ref()),
        required_features: features,
        required_limits: limits,
    }
}
