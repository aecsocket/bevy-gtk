use {
    crate::render::DmabufTexture,
    alloc::sync::Arc,
    bevy_app::prelude::*,
    bevy_asset::{Assets, Handle, RenderAssetUsages},
    bevy_ecs::{prelude::*, system::SystemParam},
    bevy_image::Image,
    bevy_render::{
        Extract, ExtractSchedule, RenderApp,
        render_asset::RenderAssets,
        renderer::RenderDevice,
        texture::{DefaultImageSampler, GpuImage},
    },
    core::sync::atomic::{self, AtomicI32},
    gtk::prelude::*,
    wgpu::{TextureUsages, TextureViewDescriptor},
};

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<ViewportState>()
        .add_systems(First, clear_extracted_viewports); // TODO what schedule?

    let render_app = app
        .get_sub_app_mut(RenderApp)
        .expect("render plugin requires `RenderApp`");
    render_app.add_systems(ExtractSchedule, extract_viewports);
}

#[derive(Debug, Default, Resource)]
pub struct ViewportState {
    to_render_world: Vec<(
        Handle<Image>,
        bevy_render::render_resource::Texture,
        bevy_render::render_resource::TextureView,
    )>,
}

pub fn create_viewport(
    images: &mut Assets<Image>,
    render_device: &RenderDevice,
    viewport_state: &mut ViewportState,
) -> (Handle<Image>, ViewportWidgetFactory) {
    let mut image = Image::new_uninit(
        wgpu::Extent3d {
            width: 512,
            height: 512,
            depth_or_array_layers: 1,
        },
        wgpu::TextureDimension::D2,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );

    image.texture_descriptor.usage =
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::RENDER_ATTACHMENT;
    let image_handle = images.add(image);

    // TODO: if the image is too small, like 1x1, vulkan fails
    let dmabuf_texture = DmabufTexture::new(render_device.wgpu_device(), 512, 512, None).unwrap();
    let bevy_texture =
        bevy_render::render_resource::Texture::from(dmabuf_texture.wgpu_texture().clone());
    let texture_view = bevy_texture.create_view(&TextureViewDescriptor::default());
    viewport_state
        .to_render_world
        .push((image_handle.clone(), bevy_texture, texture_view));

    let graphics_width = Arc::new(AtomicI32::new(0));
    let graphics_height = Arc::new(AtomicI32::new(0));

    (
        image_handle,
        ViewportWidgetFactory {
            graphics_width,
            graphics_height,
            dmabuf_texture,
        },
    )
}

#[derive(SystemParam)]
pub struct Viewports<'w> {
    images: ResMut<'w, Assets<Image>>,
    render_device: Res<'w, RenderDevice>,
    viewport_state: ResMut<'w, ViewportState>,
}

impl Viewports<'_> {
    #[must_use]
    pub fn create(&mut self) -> (Handle<Image>, ViewportWidgetFactory) {
        create_viewport(
            &mut self.images,
            &self.render_device,
            &mut self.viewport_state,
        )
    }
}

fn extract_viewports(
    viewport_state: Extract<Res<ViewportState>>,
    mut gpu_images: ResMut<RenderAssets<GpuImage>>,
    default_image_sampler: Res<DefaultImageSampler>,
) {
    for (image_handle, bevy_texture, texture_view) in &viewport_state.to_render_world {
        let gpu_image = GpuImage {
            texture: bevy_texture.clone(),
            texture_view: texture_view.clone(),
            texture_format: bevy_texture.format(),
            sampler: (**default_image_sampler).clone(),
            size: bevy_texture.size(),
            mip_level_count: 1,
        };

        gpu_images.insert(image_handle, gpu_image);
    }
}

fn clear_extracted_viewports(mut viewport_state: ResMut<ViewportState>) {
    // viewport_state.to_render_world.clear();
}

#[derive(Debug)]
pub struct ViewportWidgetFactory {
    graphics_width: Arc<AtomicI32>,
    graphics_height: Arc<AtomicI32>,
    dmabuf_texture: DmabufTexture,
}

impl ViewportWidgetFactory {
    #[must_use]
    pub fn make(self) -> gtk::Widget {
        let picture = gtk::Picture::new();

        picture.set_paintable(Some(&self.dmabuf_texture.build_gdk_texture().unwrap()));

        let offload = gtk::GraphicsOffload::builder()
            .black_background(true)
            .child(&picture)
            .hexpand(true)
            .vexpand(true)
            .build();

        let container = {
            // Use a trick to detect when the picture is resized.
            // https://stackoverflow.com/questions/70488187/get-calculated-size-of-widget-in-gtk-4-0
            // +-----------------------+
            // |          WL           |  WL: width_listener  (height 0)
            // |-----------------------|  HL: height_listener (width 0)
            // |   |                   |
            // | H |     picture       |
            // | L |                   |
            // |   |                   |
            // +-----------------------+

            let width_listener = gtk::DrawingArea::builder().hexpand(true).build();
            width_listener.set_draw_func(move |_, _, width, _| {
                self.graphics_width.store(width, atomic::Ordering::SeqCst);
            });

            let height_listener = gtk::DrawingArea::builder().vexpand(true).build();
            height_listener.set_draw_func(move |_, _, _, height| {
                self.graphics_height.store(height, atomic::Ordering::SeqCst);
            });

            let frame_content_h = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            frame_content_h.append(&height_listener);
            frame_content_h.append(&offload);

            let frame_content_v = gtk::Box::new(gtk::Orientation::Vertical, 0);
            frame_content_v.append(&width_listener);
            frame_content_v.append(&frame_content_h);

            frame_content_v
        };

        offload.add_tick_callback(move |_, _| glib::ControlFlow::Continue);

        container.upcast()
    }
}
