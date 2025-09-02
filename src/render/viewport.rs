use {
    crate::render::DmabufTexture,
    alloc::sync::Arc,
    atomicbox::AtomicOptionBox,
    bevy_app::prelude::*,
    bevy_asset::{Assets, Handle, RenderAssetUsages},
    bevy_ecs::{prelude::*, query::QueryItem, system::SystemParam},
    bevy_image::Image,
    bevy_render::{
        ExtractSchedule, Render, RenderApp, RenderSystems,
        extract_component::{ExtractComponent, ExtractComponentPlugin},
        render_asset::RenderAssets,
        render_resource::{Texture, TextureView},
        renderer::{RenderAdapter, RenderDevice},
        sync_world::{RenderEntity, SyncToRenderWorld, SyncWorldPlugin},
        texture::{DefaultImageSampler, GpuImage},
    },
    core::{
        cell::Cell,
        mem,
        sync::atomic::{self, AtomicU32},
    },
    glib::clone,
    gtk::prelude::*,
    log::{debug, trace, warn},
    wgpu::{Extent3d, TextureDimension, TextureFormat, TextureUsages, TextureViewDescriptor},
};

pub(super) fn plugin(app: &mut App) {
    app.add_plugins(ExtractComponentPlugin::<RenderViewport>::default())
        .add_systems(First, despawn_destroyed_viewports);

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
    next_frame: Arc<AtomicOptionBox<DmabufTexture>>,
    widget_size: Arc<(AtomicU32, AtomicU32)>,
    /// Marks if the GTK-side widget is still alive.
    widget_alive: Arc<()>,
}

#[derive(Debug)]
struct Framebuffer {
    dmabuf: DmabufTexture,
    // even though we can make a Bevy `Texture` from the `dmabuf`'s `wgpu::Texture`,
    // we should cache it here, because each new `Texture` increments an ID counter.
    // see `TextureId`
    texture: Texture,
    texture_view: TextureView,
}

#[derive(Debug, Component)]
struct RenderViewport {
    image_handle: Handle<Image>,
    next_frame: Arc<AtomicOptionBox<DmabufTexture>>,
    widget_size: Arc<(AtomicU32, AtomicU32)>,
    front_buffer: Option<Framebuffer>,
    back_buffer: Option<Framebuffer>,
    old_widget_size: (u32, u32),
}

// creation logic

#[derive(SystemParam)]
pub struct GtkViewports<'w, 's> {
    images: ResMut<'w, Assets<Image>>,
    commands: Commands<'w, 's>,
}

const TEXTURE_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;

impl GtkViewports<'_, '_> {
    pub fn create(&mut self) -> (Handle<Image>, ViewportWidgetFactory) {
        // the parameters of this image don't actually matter,
        // we're gonna replace this image when we get into the render world
        let mut image = Image::new_uninit(
            Extent3d {
                width: 512,
                height: 512,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            TEXTURE_FORMAT,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        );
        image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
            | TextureUsages::COPY_DST
            | TextureUsages::RENDER_ATTACHMENT;
        let image_handle = self.images.add(image);

        let next_frame = Arc::new(AtomicOptionBox::none());
        let widget_size = Arc::new((AtomicU32::new(0), AtomicU32::new(0)));
        let widget_alive = Arc::new(());

        let entity = self
            .commands
            .spawn((
                SyncToRenderWorld,
                Viewport {
                    image_handle: image_handle.clone(),
                    next_frame: next_frame.clone(),
                    widget_size: widget_size.clone(),
                    widget_alive: widget_alive.clone(),
                },
            ))
            .id();
        debug!("Spawned viewport {entity}");

        (
            image_handle,
            ViewportWidgetFactory {
                next_frame,
                widget_size,
                widget_alive,
            },
        )
    }
}

impl ExtractComponent for RenderViewport {
    type QueryData = &'static Viewport;
    type QueryFilter = ();
    type Out = Self;

    fn extract_component(viewport: QueryItem<Self::QueryData>) -> Option<Self::Out> {
        Some(Self {
            image_handle: viewport.image_handle.clone(),
            widget_size: viewport.widget_size.clone(),
            next_frame: viewport.next_frame.clone(),
            front_buffer: None,
            back_buffer: None,
            old_widget_size: (u32::MAX, u32::MAX),
        })
    }
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
        // recreate the back buffer if we need to
        // because e.g. widget size has changed
        let (new_width, new_height) = (
            viewport.widget_size.0.load(atomic::Ordering::SeqCst),
            viewport.widget_size.1.load(atomic::Ordering::SeqCst),
        );
        let (new_width, new_height) = (new_width.max(1), new_height.max(1));
        // TODO
        let (new_width, new_height) = (new_width.div_ceil(64) * 64, new_height.div_ceil(64) * 64);

        let (old_width, old_height) = viewport.old_widget_size;
        if new_width != old_width || new_height != old_height {
            let dmabuf = DmabufTexture::new(
                &*render_adapter,
                render_device.wgpu_device(),
                new_width,
                new_height,
                TEXTURE_FORMAT,
                None,
            )
            .unwrap();
            let texture = Texture::from(dmabuf.wgpu_texture().clone());
            let texture_view = texture.create_view(&TextureViewDescriptor::default());
            viewport.back_buffer = Some(Framebuffer {
                dmabuf,
                texture,
                texture_view,
            });
        }

        if let Some(back_buffer) = &viewport.back_buffer {
            // make our image handle point into this back buffer
            // remember: we constantly swap between front and back buffers,
            // so we need to update this on every frame
            let gpu_image = GpuImage {
                texture: back_buffer.texture.clone(),
                texture_view: back_buffer.texture_view.clone(),
                texture_format: back_buffer.texture.format(),
                sampler: (**default_image_sampler).clone(),
                size: back_buffer.texture.size(),
                mip_level_count: 1,
            };
            gpu_images.insert(&viewport.image_handle, gpu_image);
        }
    }
}

fn present_frames(mut viewports: Query<&mut RenderViewport>) {
    for mut viewport in &mut viewports {
        let viewport = &mut *viewport;

        // we've just rendered into the back buffer, now we flip buffers
        mem::swap(&mut viewport.back_buffer, &mut viewport.front_buffer);

        if let Some(front_buffer) = &viewport.front_buffer {
            // the front buffer has our rendered contents;
            // hand a (ref-counted) clone over to GTK.
            // we clone instead of moving because we want to reuse images
            // for rendering on subsequent frames.
            let dmabuf = front_buffer.dmabuf.clone();
            viewport
                .next_frame
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
pub struct ViewportWidgetFactory {
    next_frame: Arc<AtomicOptionBox<DmabufTexture>>,
    widget_size: Arc<(AtomicU32, AtomicU32)>,
    widget_alive: Arc<()>,
}

impl ViewportWidgetFactory {
    #[must_use]
    pub fn make(self) -> gtk::Widget {
        let Self {
            next_frame,
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
                move |_, _, width, _| widget_size.0.store(width as u32, atomic::Ordering::SeqCst),
            ));

            let height_listener = gtk::DrawingArea::builder().vexpand(true).build();
            height_listener.set_draw_func(clone!(
                #[strong]
                widget_size,
                move |_, _, _, height| widget_size.1.store(height as u32, atomic::Ordering::SeqCst),
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
            (|| {
                let Some(dmabuf) = next_frame.take(atomic::Ordering::SeqCst) else {
                    return;
                };
                let texture = match dmabuf.build_gdk_texture() {
                    Ok(t) => t,
                    Err(err) => {
                        warn!("Failed to build GDK texture from dmabuf texture: {err:?}");
                        return;
                    }
                };

                picture.set_paintable(Some(&texture));
            })();
            glib::ControlFlow::Continue
        });

        let widget_alive = Cell::new(Some(widget_alive));
        offload.connect_destroy(move |_| {
            // signal that the original texture is now safe to drop
            drop(widget_alive.take());
        });

        container.upcast()
    }
}
