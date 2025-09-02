//! # Architecture
//!
//! When you [`GtkViewports::create`] a viewport:
//! - you get a [`Handle<Image>`] which you can use as a camera render target
//! - you get a [`ViewportWidgetFactory`] which you can use to make a
//!   [`gtk::GraphicsOffload`] widget for your app
//! - a private entity is spawned which maintains the viewport state on the Bevy
//!   app/render side
//!
//! There is a real Bevy image that backs the handle, but we don't actually use
//! that image for rendering into. We only need it for some compatibility stuff
//! with [`bevy_render`]. Instead, we make a [`DmabufTexture`] and set that as
//! the GPU image which Bevy renders into.
//!
//! The GTK app is a very thin layer, because it's somewhat annoying to work
//! with GTK from Bevy. All of its logic is tied to the widget you get from
//! creating a viewport, which makes cleanup easy - as soon as the widget is
//! destroyed, everything else goes with it. We then manually propagate this
//! cleanup to the Bevy world.
//!
//! The widget is responsible for:
//! - reading its own width and height, and sending that to the Bevy app
//! - receiving [`DmabufTexture`]s from the app, downloading them
//!
//! We implement our own swapchain via a front and back buffer. Bevy only ever
//! writes into the back buffer - this includes texture creation (when the
//! backing widget is resized), and actual rendering. Then once we've rendered
//! into the back buffer, we swap the buffers and pass a copy of the (now) front
//! buffer to the GTK app.

// architecture v2:
//! Here are the core limitations:
//! - We have a Bevy app which can push frames at X frames/sec
//! - We have a GTK app which can consume frames at Y frames/sec
//! - X and Y may not be the same
//! - GTK must
//!
//! wait...

use {
    crate::render::DmabufTexture,
    alloc::sync::Arc,
    atomicbox::AtomicOptionBox,
    bevy_app::prelude::*,
    bevy_asset::{Assets, Handle, RenderAssetUsages},
    bevy_camera::CameraUpdateSystems,
    bevy_ecs::{prelude::*, query::QueryItem, system::SystemParam},
    bevy_image::Image,
    bevy_render::{
        Render, RenderApp, RenderSystems,
        extract_component::{ExtractComponent, ExtractComponentPlugin},
        render_asset::RenderAssets,
        render_resource::{Texture, TextureView},
        renderer::{RenderAdapter, RenderDevice},
        sync_world::SyncToRenderWorld,
        texture::{DefaultImageSampler, GpuImage},
    },
    core::{
        cell::{Cell, RefCell},
        mem,
        sync::atomic::{self, AtomicU32},
    },
    glib::clone,
    gtk::prelude::*,
    log::{debug, trace},
    wgpu::{Extent3d, TextureDimension, TextureFormat, TextureUsages, TextureViewDescriptor},
};

#[derive(SystemParam)]
pub struct GtkViewports<'w, 's> {
    images: ResMut<'w, Assets<Image>>,
    commands: Commands<'w, 's>,
}

pub(super) fn plugin(app: &mut App) {
    app.add_plugins(ExtractComponentPlugin::<RenderViewport>::default())
        .add_systems(PostStartup, update_images.before(CameraUpdateSystems))
        .add_systems(
            PostUpdate,
            (
                update_images.before(CameraUpdateSystems),
                despawn_destroyed_viewports,
            ),
        );

    let render_app = app
        .get_sub_app_mut(RenderApp)
        .expect("`GtkPlugin` with `render` feature requires `RenderApp`");
    render_app.add_systems(
        Render,
        (
            // TODO: change scheduling?
            set_target_images.after(RenderSystems::ExtractCommands),
            present_frames.after(RenderSystems::Render),
        ),
    );
}

#[derive(Debug, Component)]
struct Viewport {
    /// [`Handle`] to the [`Image`] used as a [`Camera::target`] for rendering.
    ///
    /// [`Camera::target`]: bevy_camera::Camera::target
    image_handle: Handle<Image>,
    next_dmabuf: Arc<AtomicOptionBox<DmabufTexture>>,
    widget_size: Arc<(AtomicU32, AtomicU32)>,
    /// Marks if the GTK-side widget is still alive.
    widget_alive: Arc<()>,
    old_widget_size: (u32, u32),
}

#[derive(Debug, Component)]
struct RenderViewport {
    image_handle: Handle<Image>,
    next_dmabuf: Arc<AtomicOptionBox<DmabufTexture>>,
    widget_size: Arc<(AtomicU32, AtomicU32)>,
    /// Texture and view that this viewport will render into.
    back_buffer: Option<(Texture, TextureView)>,
    /// Value of [`RenderViewport::widget_size`] from the previous frame.
    ///
    /// If this is different to the current size, we will create a new texture
    /// with the new size and render into that.
    old_widget_size: (u32, u32),
    /// Texture which will next be stored in [`RenderViewport::next_dmabuf`].
    ///
    /// When we need to create a new texture because the size has changed, we
    /// do the following:
    /// - before rendering
    ///   - create a new [`DmabufTexture`]
    ///   - set that texture as the [`RenderViewport::back_buffer`]
    ///   - set that texture as the queued dmabuf
    ///   - do *not* put it in `next_dmabuf` yet, since we've just made it and
    ///     it has no rendered content
    /// - after rendering
    ///   - the dmabuf now has drawn content, so take the dmabuf and put it into
    ///     `next_dmabuf`
    queued_dmabuf: Option<DmabufTexture>,
}

// creation logic

const TEXTURE_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;

impl GtkViewports<'_, '_> {
    pub fn create(&mut self) -> (Handle<Image>, WidgetFactory) {
        let image_handle = self.images.reserve_handle();
        let next_dmabuf = Arc::new(AtomicOptionBox::none());
        let widget_size = Arc::new((AtomicU32::new(0), AtomicU32::new(0)));
        let widget_alive = Arc::new(());

        let entity = self
            .commands
            .spawn((
                SyncToRenderWorld,
                Viewport {
                    image_handle: image_handle.clone(),
                    next_dmabuf: next_dmabuf.clone(),
                    widget_size: widget_size.clone(),
                    widget_alive: widget_alive.clone(),
                    old_widget_size: (u32::MAX, u32::MAX),
                },
            ))
            .id();
        debug!("Spawned viewport {entity}");

        (
            image_handle,
            WidgetFactory {
                next_dmabuf,
                widget_size,
                widget_alive,
            },
        )
    }
}

impl ExtractComponent for RenderViewport {
    type QueryData = &'static Viewport;
    type QueryFilter = Added<Viewport>;
    type Out = Self;

    fn extract_component(viewport: QueryItem<Self::QueryData>) -> Option<Self::Out> {
        Some(Self {
            image_handle: viewport.image_handle.clone(),
            widget_size: viewport.widget_size.clone(),
            next_dmabuf: viewport.next_dmabuf.clone(),
            back_buffer: None,
            old_widget_size: (u32::MAX, u32::MAX),
            queued_dmabuf: None,
        })
    }
}

// frame-to-frame rendering logic, in the main world

fn update_images(mut viewports: Query<&mut Viewport>, mut images: ResMut<Assets<Image>>) {
    for mut viewport in &mut viewports {
        let (new_width, new_height) = read_size(&viewport.widget_size);
        let (old_width, old_height) = viewport.old_widget_size;
        if new_width != old_width || new_height != old_height {
            viewport.old_widget_size = (new_width, new_height);

            let mut image = Image::new_uninit(
                Extent3d {
                    width: new_width,
                    height: new_height,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                TEXTURE_FORMAT,
                RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
            );
            image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::RENDER_ATTACHMENT;
            images
                .insert(&viewport.image_handle, image)
                .expect("should be able to insert image asset");
        }
    }
}

fn read_size(widget_size: &Arc<(AtomicU32, AtomicU32)>) -> (u32, u32) {
    let (width, height) = (
        widget_size.0.load(atomic::Ordering::SeqCst),
        widget_size.1.load(atomic::Ordering::SeqCst),
    );
    // (width.max(1), height.max(1))
    let (width, height) = (width.max(1), height.max(1));
    (width.div_ceil(64) * 64, height.div_ceil(64) * 64)
}

// frame-to-frame rendering logic, in the render world

fn set_target_images(
    mut viewports: Query<&mut RenderViewport>,
    render_adapter: Res<RenderAdapter>,
    render_device: Res<RenderDevice>,
    default_image_sampler: Res<DefaultImageSampler>,
    mut gpu_images: ResMut<RenderAssets<GpuImage>>,
) {
    for mut viewport in &mut viewports {
        let (new_width, new_height) = (
            viewport.widget_size.0.load(atomic::Ordering::SeqCst),
            viewport.widget_size.1.load(atomic::Ordering::SeqCst),
        );

        let (old_width, old_height) = viewport.old_widget_size;
        if new_width != old_width || new_height != old_height {
            viewport.old_widget_size = (new_width, new_height);
            trace!(
                "Old/new window size: {old_width}x{old_height} / {new_width}x{new_height}, \
                 creating new dmabuf"
            );

            let (tex_width, tex_height) = (
                new_width.max(1).div_ceil(64) * 64,
                new_height.max(1).div_ceil(64) * 64,
            );

            let dmabuf = DmabufTexture::new(
                &render_adapter,
                render_device.wgpu_device(),
                tex_width,
                tex_height,
                TEXTURE_FORMAT,
                None,
            )
            .expect("failed to create dmabuf texture");

            let texture = Texture::from(dmabuf.wgpu_texture().clone());
            let texture_view = texture.create_view(&TextureViewDescriptor::default());
            viewport.back_buffer = Some((texture, texture_view));
            viewport.queued_dmabuf = Some(dmabuf);
        }

        if let Some((texture, texture_view)) = &viewport.back_buffer {
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

fn present_frames(mut viewports: Query<&mut RenderViewport>) {
    for mut viewport in &mut viewports {
        let viewport = &mut *viewport;

        if let Some(dmabuf) = viewport.queued_dmabuf.take() {
            viewport
                .next_dmabuf
                .store(Some(Box::new(dmabuf)), atomic::Ordering::SeqCst);
        }
    }
}

// destroy logic

fn despawn_destroyed_viewports(viewports: Query<(Entity, &Viewport)>, mut commands: Commands) {
    for (entity, viewport) in &viewports {
        if Arc::strong_count(&viewport.widget_alive) == 1 {
            debug!("Despawned viewport {entity} because its GTK widget was dropped");
            commands.entity(entity).despawn();
        }
    }
}

// GTK-side logic

#[derive(Debug)]
pub struct WidgetFactory {
    next_dmabuf: Arc<AtomicOptionBox<DmabufTexture>>,
    widget_size: Arc<(AtomicU32, AtomicU32)>,
    widget_alive: Arc<()>,
}

impl WidgetFactory {
    #[must_use]
    pub fn make(self) -> gtk::Widget {
        #[derive(Debug)]
        struct Swapchain {
            // keep the dmabuf alive until we get a new texture
            _dmabuf: DmabufTexture,
            // these aren't `front` and `back` buffers,
            // because their role constantly swaps
            texture_a: gdk::Texture,
            texture_b: gdk::Texture,
        }

        let Self {
            next_dmabuf,
            widget_size,
            widget_alive,
        } = self;

        let picture = gtk::Picture::new();
        let offload = gtk::GraphicsOffload::builder()
            .black_background(true)
            .child(&picture)
            .hexpand(true)
            .vexpand(true)
            .build();

        let container = {
            // Use a trick to detect when the picture is resized.
            // <https://stackoverflow.com/questions/70488187/get-calculated-size-of-widget-in-gtk-4-0>
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
                widget_size,
                move |_, _, width, _| {
                    #[expect(
                        clippy::cast_sign_loss,
                        reason = "GTK should never give us a negative width"
                    )]
                    widget_size.0.store(width as u32, atomic::Ordering::SeqCst);
                },
            ));

            let height_listener = gtk::DrawingArea::builder().vexpand(true).build();
            height_listener.set_draw_func(clone!(
                #[strong]
                widget_size,
                move |_, _, _, height| {
                    #[expect(
                        clippy::cast_sign_loss,
                        reason = "GTK should never give us a negative height"
                    )]
                    widget_size.1.store(height as u32, atomic::Ordering::SeqCst);
                },
            ));

            let frame_content_h = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            frame_content_h.append(&height_listener);
            frame_content_h.append(&offload);

            let frame_content_v = gtk::Box::new(gtk::Orientation::Vertical, 0);
            frame_content_v.append(&width_listener);
            frame_content_v.append(&frame_content_h);

            frame_content_v
        };

        let swapchain = RefCell::new(None::<Swapchain>);
        offload.add_tick_callback(move |_, _| {
            if let Some(dmabuf) = next_dmabuf.take(atomic::Ordering::SeqCst) {
                trace!("Downloading new dmabufs from GTK");
                let (texture_a, texture_b) = (
                    dmabuf
                        .build_gdk_texture()
                        .expect("failed to build dmabuf texture"),
                    dmabuf
                        .build_gdk_texture()
                        .expect("failed to build dmabuf texture"),
                );
                swapchain.replace(Some(Swapchain {
                    _dmabuf: *dmabuf,
                    texture_a,
                    texture_b,
                }));
            }

            if let Some(swapchain) = &mut *swapchain.borrow_mut() {
                picture.set_paintable(Some(&swapchain.texture_a));
                mem::swap(&mut swapchain.texture_a, &mut swapchain.texture_b);
            }

            glib::ControlFlow::Continue
        });

        let widget_alive = Cell::new(Some(widget_alive));
        offload.connect_destroy(move |_| drop(widget_alive.take()));

        container.upcast()
    }
}
