//! # Architecture
//!
//! When you [`GtkViewports::create`] a viewport:
//! - you get a [`GtkViewport`] which you can attach to a camera, to make the
//!   camera render into that viewport
//!   - you can also get a [`Handle<Image>`] to its image directly, if you need
//!     that
//! - you get a [`ViewportWidgetFactory`] which you can use to make a
//!   [`gtk::GraphicsOffload`] widget for your app
//! - a private [`ViewportPrivate`] entity is spawned which is copied into the
//!   render world, and drives rendering logic
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
//! - receiving [`DmabufTexture`]s from the app, making [`gdk::Texture`]s out of
//!   them, and rendering them to the GTK app
//!
//! GTK land effectively acts as our front buffer, and Bevy as our back buffer;
//! swapping buffers is implicit, by sending the rendered Bevy back buffer to
//! GTK. Bevy deals with dmabufs and wgpu textures, and GTK deals with dmabufs
//! and GDK textures; the dmabuf is the communication medium between the two.
//!
//! When you insert a [`GtkViewport`] into a camera entity, the viewport will
//! constantly update the camera's target to the viewport image, and extra
//! appropriate settings like scale factor.
//!
//! # Issues
//!
//! The main world and render world viewports keep track of `old_widget_size`
//! separately. This isn't a dealbreaker, as they will eventually converge to
//! the same image size, but it is possible (and common) that for maybe 1 or 2
//! frames, the main world image size and render world wgpu texture will be
//! different sizes.

use {
    alloc::sync::Arc,
    atomic_float::AtomicF64,
    atomicbox::AtomicOptionBox,
    bevy_app::prelude::*,
    bevy_asset::{Assets, Handle, RenderAssetUsages},
    bevy_camera::{Camera, CameraUpdateSystems, ImageRenderTarget, RenderTarget},
    bevy_ecs::{prelude::*, query::QueryItem, system::SystemParam},
    bevy_image::Image,
    bevy_math::FloatOrd,
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
    gdk::prelude::*,
    glib::clone,
    gtk::prelude::*,
    log::{debug, trace},
    wgpu::{Extent3d, TextureDimension, TextureFormat, TextureUsages, TextureViewDescriptor},
};

mod dmabuf;
pub use dmabuf::*;

pub(super) fn init_plugin(app: &mut App) {
    dmabuf::init_plugin(app);
}

pub(super) fn plugin(app: &mut App) {
    app.add_plugins(ExtractComponentPlugin::<RenderViewport>::default())
        .add_systems(
            PostStartup,
            (sync_viewport_and_camera, update_images)
                .chain()
                .before(CameraUpdateSystems),
        )
        .add_systems(
            PostUpdate,
            (
                (sync_viewport_and_camera, update_images)
                    .chain()
                    .before(CameraUpdateSystems),
                despawn_destroyed_viewports,
            ),
        );

    let render_app = app
        .get_sub_app_mut(RenderApp)
        .expect("`GtkPlugin` with `render` feature requires `RenderApp`");
    render_app.add_systems(
        Render,
        (
            // I tested; this exact scheduling is correct.
            set_target_images.after(RenderSystems::ExtractCommands),
            present_frames.after(RenderSystems::Render),
        ),
    );
}

/// Represents a [`gtk::Widget`] which renders Bevy content.
///
/// Use [`GtkViewports::create`] to create one, and insert this into a
/// [`Camera`] entity to force the camera to render into the GTK viewport. This
/// component will automatically handle details like scale factor.
///
/// Note that this component does not keep the viewport alive and does not drive
/// rendering logic; only camera logic. The actual GTK viewport and underlying
/// rendering logic lives for as long as the GTK widget lives.
#[derive(Debug, Component)]
pub struct GtkViewport {
    image_handle: Handle<Image>,
    widget_scale_factor: Arc<AtomicF64>,
}

impl GtkViewport {
    /// [`Handle`] to the [`Image`] used as a [`Camera::target`] for rendering.
    ///
    /// If you have more advanced needs you can use the image handle directly,
    /// but this will not account for window scale factor.
    ///
    /// [`Camera::target`]: bevy_camera::Camera::target
    #[must_use]
    pub fn image_handle(&self) -> &Handle<Image> {
        &self.image_handle
    }

    /// Current scale factor of the GTK widget.
    ///
    /// This takes fractional scaling into account, and the resulting render
    /// target output is already properly scaled by this factor.
    #[must_use]
    pub fn widget_scale_factor(&self) -> f64 {
        self.widget_scale_factor.load(atomic::Ordering::SeqCst)
    }
}

#[derive(Debug, Component)]
#[require(SyncToRenderWorld)]
struct ViewportPrivate {
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

/// Allows creating a [`GtkViewport`].
#[derive(SystemParam)]
pub struct GtkViewports<'w, 's> {
    images: ResMut<'w, Assets<Image>>,
    commands: Commands<'w, 's>,
}

impl GtkViewports<'_, '_> {
    /// Creates a viewport, exposing the Bevy [`GtkViewport`] and GTK
    /// [`WidgetFactory`] for this viewport.
    ///
    /// This does not directly create the [`gtk::Widget`] as GTK types are
    /// `!Send`, so it would be useless to create one off of the GTK thread.
    /// Instead, call [`WidgetFactory::make`] inside [`GtkWindowContent`] to
    /// set the content on the GTK thread.
    ///
    /// [`GtkWindowContent`]: crate::GtkWindowContent
    pub fn create(&mut self) -> (GtkViewport, WidgetFactory) {
        let image_handle = self.images.reserve_handle();
        let next_dmabuf = Arc::new(AtomicOptionBox::none());
        let widget_size = Arc::new((AtomicU32::new(0), AtomicU32::new(0)));
        let widget_scale_factor = Arc::new(AtomicF64::new(1.0));
        let widget_alive = Arc::new(());

        self.commands.spawn(ViewportPrivate {
            image_handle: image_handle.clone(),
            next_dmabuf: next_dmabuf.clone(),
            widget_size: widget_size.clone(),
            widget_alive: widget_alive.clone(),
            old_widget_size: (u32::MAX, u32::MAX),
        });

        (
            GtkViewport {
                image_handle,
                widget_scale_factor: widget_scale_factor.clone(),
            },
            WidgetFactory {
                next_dmabuf,
                widget_size,
                widget_scale_factor,
                widget_alive,
            },
        )
    }
}

impl ExtractComponent for RenderViewport {
    type QueryData = &'static ViewportPrivate;
    type QueryFilter = Added<ViewportPrivate>;
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

const TEXTURE_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;

fn sync_viewport_and_camera(mut viewports: Query<(&GtkViewport, &mut Camera)>) {
    for (viewport, mut camera) in &mut viewports {
        camera.target = RenderTarget::Image(ImageRenderTarget {
            handle: viewport.image_handle.clone(),
            #[expect(clippy::cast_possible_truncation, reason = "しょうがないね")]
            scale_factor: FloatOrd(viewport.widget_scale_factor() as f32),
        });
    }
}

fn update_images(mut viewports: Query<&mut ViewportPrivate>, mut images: ResMut<Assets<Image>>) {
    for mut viewport in &mut viewports {
        let (new_width, new_height) = (
            viewport.widget_size.0.load(atomic::Ordering::SeqCst),
            viewport.widget_size.1.load(atomic::Ordering::SeqCst),
        );
        let (old_width, old_height) = viewport.old_widget_size;
        if new_width != old_width || new_height != old_height {
            trace!(
                "Old/new widget size: {old_width}x{old_height} / {new_width}x{new_height}, \
                 creating new main world image"
            );
            viewport.old_widget_size = (new_width, new_height);

            let (tex_width, tex_height) = texture_size(new_width, new_height);
            let mut image = Image::new_uninit(
                Extent3d {
                    width: tex_width,
                    height: tex_height,
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

fn texture_size(width: u32, height: u32) -> (u32, u32) {
    (width.max(1), height.max(1))
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
            trace!(
                "Old/new widget size: {old_width}x{old_height} / {new_width}x{new_height}, \
                 creating new dmabuf"
            );
            viewport.old_widget_size = (new_width, new_height);

            let (tex_width, tex_height) = texture_size(new_width, new_height);

            let dmabuf = DmabufTexture::new(
                &render_adapter,
                render_device.wgpu_device(),
                tex_width,
                tex_height,
                TEXTURE_FORMAT,
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
        if let Some(dmabuf) = viewport.queued_dmabuf.take() {
            viewport
                .next_dmabuf
                .store(Some(Box::new(dmabuf)), atomic::Ordering::SeqCst);
        }
    }
}

// destroy logic

fn despawn_destroyed_viewports(
    viewports: Query<(Entity, &ViewportPrivate)>,
    mut commands: Commands,
) {
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
    widget_scale_factor: Arc<AtomicF64>,
    widget_alive: Arc<()>,
}

impl WidgetFactory {
    #[must_use]
    #[expect(
        clippy::cast_sign_loss,
        reason = "GTK should never give us a negative width"
    )]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "widget widths are relatively small"
    )]
    pub fn make(self) -> gtk::Widget {
        #[derive(Debug)]
        struct Swapchain {
            // these aren't `front` and `back` buffers,
            // because their role constantly swaps
            texture_a: gdk::Texture,
            texture_b: gdk::Texture,
        }

        let Self {
            next_dmabuf,
            widget_size,
            widget_scale_factor,
            widget_alive,
        } = self;

        let picture = gtk::Picture::new();
        let offload = gtk::GraphicsOffload::builder()
            .black_background(true)
            .child(&picture)
            .hexpand(true)
            .vexpand(true)
            .build();

        let get_scale = |widget: &gtk::Widget| {
            widget
                .native()
                .and_then(|native| native.surface())
                .map(|surface| surface.scale())
        };

        offload.connect_scale_factor_notify(clone!(
            #[strong]
            widget_size,
            move |widget| {
                let Some(scale) = get_scale(widget.upcast_ref()) else {
                    return;
                };
                widget_scale_factor.store(scale, atomic::Ordering::SeqCst);

                #[expect(
                    clippy::cast_sign_loss,
                    clippy::cast_possible_truncation,
                    reason = "GTK should never give us a negative width"
                )]
                let (width, height) = (
                    (f64::from(widget.width()) * scale) as u32,
                    (f64::from(widget.height()) * scale) as u32,
                );
                widget_size.0.store(width, atomic::Ordering::SeqCst);
                widget_size.1.store(height, atomic::Ordering::SeqCst);
            },
        ));

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
                move |widget, _, width, _| {
                    let Some(scale) = get_scale(widget.upcast_ref()) else {
                        return;
                    };

                    let width = (f64::from(width) * scale) as u32;
                    widget_size.0.store(width, atomic::Ordering::SeqCst);
                },
            ));

            let height_listener = gtk::DrawingArea::builder().vexpand(true).build();
            height_listener.set_draw_func(clone!(
                #[strong]
                widget_size,
                move |widget, _, _, height| {
                    let Some(scale) = get_scale(widget.upcast_ref()) else {
                        return;
                    };

                    let height = (f64::from(height) * scale) as u32;
                    widget_size.1.store(height, atomic::Ordering::SeqCst);
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
                // "wait.. why do we build 2 gdk textures for the same dmabuf?"
                //
                // GTK doesn't redraw the picture unless you manually change the
                // paintable inside it. I couldn't find a way to force it to redraw.
                // So instead, we have 2 paintables with the same underlying content
                // (same dmabuf), and switch between them.
                let (texture_a, texture_b) = (
                    dmabuf
                        .build_gdk_texture()
                        .expect("failed to build dmabuf texture"),
                    dmabuf
                        .build_gdk_texture()
                        .expect("failed to build dmabuf texture"),
                );
                swapchain.replace(Some(Swapchain {
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

        let widget_alive = Cell::new(widget_alive);
        offload.connect_destroy(move |_| drop(widget_alive.take()));

        container.upcast()
    }
}
