use {
    crate::render::DmabufTexture,
    alloc::sync::Arc,
    atomicbox::AtomicOptionBox,
    bevy_app::prelude::*,
    bevy_asset::{Assets, Handle, RenderAssetUsages},
    bevy_camera::CameraUpdateSystems,
    bevy_ecs::{prelude::*, system::SystemParam},
    bevy_image::Image,
    bevy_render::{
        Extract, ExtractSchedule, RenderApp,
        render_asset::RenderAssets,
        render_resource::{Texture, TextureView},
        renderer::{RenderAdapter, RenderDevice},
        texture::{DefaultImageSampler, GpuImage},
    },
    core::sync::atomic::{self, AtomicI32},
    glib::clone,
    gtk::prelude::*,
    wgpu::{Extent3d, TextureDimension, TextureFormat, TextureUsages, TextureViewDescriptor},
};

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<Viewports>()
        .add_systems(PostStartup, sync_gtk_to_bevy.before(CameraUpdateSystems))
        .add_systems(PostUpdate, sync_gtk_to_bevy.before(CameraUpdateSystems));

    let render_app = app
        .get_sub_app_mut(RenderApp)
        .expect("render plugin requires `RenderApp`");
    render_app.add_systems(ExtractSchedule, extract_viewports);
}

#[derive(Debug, Default, Resource)]
pub struct Viewports(Vec<Viewport>);

#[derive(Debug)]
struct Viewport {
    image_handle: Handle<Image>,
    inner: Arc<ViewportInner>,
    render: Option<(Texture, TextureView)>,
}

#[derive(Debug)]
struct ViewportInner {
    widget_width: AtomicI32,
    widget_height: AtomicI32,
    next_dmabuf: AtomicOptionBox<DmabufTexture>,
}

const TEXTURE_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;

pub fn create_viewport(
    images: &mut Assets<Image>,
    render_adapter: &RenderAdapter,
    render_device: &RenderDevice,
    viewports: &mut Viewports,
) -> (Handle<Image>, ViewportWidgetFactory) {
    let image_handle = images.reserve_handle();
    let inner = Arc::new(ViewportInner {
        // TODO: better initial w/h
        widget_width: AtomicI32::new(520),
        widget_height: AtomicI32::new(520),
        next_dmabuf: AtomicOptionBox::none(),
    });
    viewports.0.push(Viewport {
        image_handle: image_handle.clone(),
        inner: inner.clone(),
        render: None,
    });
    (image_handle, ViewportWidgetFactory { inner })
}

#[derive(SystemParam)]
pub struct GtkViewports<'w> {
    images: ResMut<'w, Assets<Image>>,
    render_adapter: Res<'w, RenderAdapter>,
    render_device: Res<'w, RenderDevice>,
    viewport_state: ResMut<'w, Viewports>,
}

impl GtkViewports<'_> {
    #[must_use]
    pub fn create(&mut self) -> (Handle<Image>, ViewportWidgetFactory) {
        create_viewport(
            &mut self.images,
            &self.render_adapter,
            &self.render_device,
            &mut self.viewport_state,
        )
    }
}

fn sync_gtk_to_bevy(
    mut viewports: ResMut<Viewports>,
    mut images: ResMut<Assets<Image>>,
    render_adapter: Res<RenderAdapter>,
    render_device: Res<RenderDevice>,
) {
    viewports.0.retain_mut(|viewport| {
        // TODO: if the gtk widget is dropped, unretain here

        let (width, height) = (
            viewport.inner.widget_width.load(atomic::Ordering::SeqCst) as u32,
            viewport.inner.widget_height.load(atomic::Ordering::SeqCst) as u32,
        );

        let (width, height) = (width.div_ceil(64) * 64, height.div_ceil(64) * 64);

        let texture_same_size = viewport
            .render
            .as_ref()
            .is_some_and(|(texture, _)| width == texture.width() && height == texture.height());
        if !texture_same_size {
            let mut image = Image::new_uninit(
                Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                TEXTURE_FORMAT,
                RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
            );
            image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::RENDER_ATTACHMENT;
            images.insert(&viewport.image_handle, image).unwrap();

            // TODO: if the image is too small, like 1x1, vulkan fails
            let dmabuf = DmabufTexture::new(
                &*render_adapter,
                render_device.wgpu_device(),
                width,
                height,
                TEXTURE_FORMAT,
                None,
            )
            .unwrap();
            let texture = Texture::from(dmabuf.wgpu_texture().clone());
            let texture_view = texture.create_view(&TextureViewDescriptor::default());
            viewport.render = Some((texture, texture_view));
            viewport
                .inner
                .next_dmabuf
                .store(Some(Box::new(dmabuf)), atomic::Ordering::SeqCst);
        }

        true
    });
}

fn extract_viewports(
    viewports: Extract<Res<Viewports>>,
    mut gpu_images: ResMut<RenderAssets<GpuImage>>,
    default_image_sampler: Res<DefaultImageSampler>,
) {
    for viewport in &viewports.0 {
        if let Some((texture, texture_view)) = &viewport.render {
            let gpu_image = GpuImage {
                texture: texture.clone(),
                texture_view: texture_view.clone(),
                texture_format: texture.format(),
                sampler: (**default_image_sampler).clone(),
                size: texture.size(),
                mip_level_count: 1,
            };
            gpu_images.insert(&viewport.image_handle, gpu_image);
        }
    }
}

#[derive(Debug)]
pub struct ViewportWidgetFactory {
    inner: Arc<ViewportInner>,
}

impl ViewportWidgetFactory {
    #[must_use]
    pub fn make(self) -> gtk::Widget {
        let picture = gtk::Picture::new();
        let offload = gtk::GraphicsOffload::builder()
            .black_background(true)
            .child(&picture)
            .hexpand(true)
            .vexpand(true)
            .build();

        let inner = self.inner;
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
            width_listener.set_draw_func(clone!(
                #[strong]
                inner,
                move |_, _, width, _| inner.widget_width.store(width, atomic::Ordering::SeqCst),
            ));

            let height_listener = gtk::DrawingArea::builder().vexpand(true).build();
            height_listener.set_draw_func(clone!(
                #[strong]
                inner,
                move |_, _, _, height| inner.widget_height.store(height, atomic::Ordering::SeqCst),
            ));

            let frame_content_h = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            frame_content_h.append(&height_listener);
            frame_content_h.append(&offload);

            let frame_content_v = gtk::Box::new(gtk::Orientation::Vertical, 0);
            frame_content_v.append(&width_listener);
            frame_content_v.append(&frame_content_h);

            frame_content_v
        };

        offload.add_tick_callback(move |_, _| {
            if let Some(dmabuf) = inner.next_dmabuf.take(atomic::Ordering::SeqCst) {
                let texture = dmabuf.build_gdk_texture().unwrap();
                picture.set_paintable(Some(&texture));
            }
            glib::ControlFlow::Continue
        });

        container.upcast()
    }
}
