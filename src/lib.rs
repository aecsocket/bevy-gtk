mod hal_custom;
mod render;

use std::{
    num::NonZero,
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc,
    },
    thread,
};

use adw::prelude::*;
use bevy::{
    ecs::system::EntityCommand,
    prelude::*,
    render::{
        camera::{ManualTextureViewHandle, ManualTextureViews, RenderTarget},
        renderer::RenderDevice,
    },
    window::WindowRef,
};
use gtk::{gdk, glib};
use sync_wrapper::SyncWrapper;

#[derive(Debug)]
pub struct AdwaitaPlugin;

impl AdwaitaPlugin {
    #[must_use]
    pub fn window_plugin() -> WindowPlugin {
        WindowPlugin::default()
        // WindowPlugin {
        //     primary_window: None,
        //     exit_condition: ExitCondition::DontExit,
        //     close_when_requested: false,
        // }
    }
}

impl Plugin for AdwaitaPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreUpdate, update_frame_size)
            .observe(change_default_render_target);
    }
}

#[derive(Debug, Component)]
pub struct AdwaitaWindow {
    recv_close_code: SyncWrapper<oneshot::Receiver<i32>>,
    render_target_view_handle: ManualTextureViewHandle,
    send_dmabuf_info: flume::Sender<DmabufInfo>,
    frame_width: Arc<AtomicI32>,
    frame_height: Arc<AtomicI32>,
    last_frame_size: Option<(NonZero<u32>, NonZero<u32>)>,
}

#[derive(Debug, Clone, Copy, Component, Reflect)]
#[reflect(Component)]
pub struct PrimaryAdwaitaWindow;

const DEFAULT_SIZE: UVec2 = UVec2::new(512, 512);

impl AdwaitaWindow {
    #[must_use]
    pub fn open(app_id: impl Into<String>) -> impl EntityCommand {
        let application_id = app_id.into();
        move |entity: Entity, world: &mut World| {
            open(
                entity,
                world,
                application_id,
                NonZero::new(1280).unwrap(),
                NonZero::new(720).unwrap(),
            )
        }
    }

    #[must_use]
    pub const fn render_target_view_handle(&self) -> ManualTextureViewHandle {
        self.render_target_view_handle
    }

    #[must_use]
    pub const fn render_target(&self) -> RenderTarget {
        RenderTarget::TextureView(self.render_target_view_handle)
    }
}

/// Info that that the Adwaita window requires to be able to make a
/// `GraphicsOffload` for rendering the Bevy app contents.
#[derive(Debug, Clone, Copy)]
struct DmabufInfo {
    /// Width of the buffer.
    width: NonZero<u32>,
    /// Height of the buffer.
    height: NonZero<u32>,
    /// File descriptor of the buffer.
    fd: i32,
}

// Spawns a thread to run the Adwaita window event loop, and creates a texture
// view handle for the render target, but does *not* set up any rendering stuff
// yet - we do that when we first update the frame size.
fn open(
    entity: Entity,
    world: &mut World,
    app_id: String,
    width: NonZero<u32>,
    height: NonZero<u32>,
) {
    let (width_i, height_i) = (
        i32::try_from(width.get()).expect("width should fit within an i32"),
        i32::try_from(height.get()).expect("height should fit within an i32"),
    );

    // Make a random handle which doesn't conflict with any existing handles.
    let manual_texture_views = world.resource::<ManualTextureViews>();
    let view_handle = loop {
        let view_handle = ManualTextureViewHandle(rand::random());
        if !manual_texture_views.contains_key(&view_handle) {
            break view_handle;
        }
    };

    let (send_dmabuf_info, recv_dmabuf_info) = flume::bounded::<DmabufInfo>(1);
    let (frame_width, frame_height) = (
        Arc::new(AtomicI32::new(width_i)),
        Arc::new(AtomicI32::new(height_i)),
    );
    let (send_close_code, recv_close_code) = oneshot::channel::<i32>();
    thread::spawn({
        let (frame_width, frame_height) = (frame_width.clone(), frame_height.clone());
        move || {
            run(
                app_id,
                recv_dmabuf_info,
                frame_width,
                frame_height,
                send_close_code,
            )
        }
    });
    world.entity_mut(entity).insert(AdwaitaWindow {
        recv_close_code: SyncWrapper::new(recv_close_code),
        render_target_view_handle: view_handle,
        send_dmabuf_info,
        frame_width,
        frame_height,
        last_frame_size: None,
    });
}

fn run(
    app_id: String,
    recv_dmabuf_info: flume::Receiver<DmabufInfo>,
    frame_width: Arc<AtomicI32>,
    frame_height: Arc<AtomicI32>,
    send_close_code: oneshot::Sender<i32>,
) {
    let app = adw::Application::builder().application_id(app_id).build();

    let recv_dmabuf_info = Arc::new(recv_dmabuf_info);
    app.connect_activate(move |app| {
        let header_bar = adw::HeaderBar::new();

        let picture = gtk::Picture::new();
        let graphics_offload = gtk::GraphicsOffload::builder()
            .black_background(true)
            .hexpand(true)
            .vexpand(true)
            .child(&picture)
            .build();

        let frame = {
            // https://stackoverflow.com/questions/70488187/get-calculated-size-of-widget-in-gtk-4-0
            // +-----------------------+
            // |          WL           |  WL: width_listener  (height 0)
            // |-----------------------|  HL: height_listener (width 0)
            // |   |                   |
            // | H |     graphics      |
            // | L |     offload       |
            // |   |                   |
            // +-----------------------+

            let width_listener = gtk::DrawingArea::builder().hexpand(true).build();
            width_listener.set_draw_func({
                let frame_width = frame_width.clone();
                move |_, _, width, _| {
                    frame_width.store(width, Ordering::SeqCst);
                }
            });

            let height_listener = gtk::DrawingArea::builder().vexpand(true).build();
            height_listener.set_draw_func({
                let frame_height = frame_height.clone();
                move |_, _, _, height| {
                    frame_height.store(height, Ordering::SeqCst);
                }
            });

            let frame_content_h = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            frame_content_h.append(&height_listener);
            frame_content_h.append(&graphics_offload);

            let frame_content_v = gtk::Box::new(gtk::Orientation::Vertical, 0);
            frame_content_v.append(&width_listener);
            frame_content_v.append(&frame_content_h);

            frame_content_v
        };

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header_bar);
        content.append(&frame);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("First App")
            .default_width(frame_width.load(Ordering::SeqCst))
            .default_height(frame_height.load(Ordering::SeqCst))
            .content(&content)
            .build();

        window.present();

        let recv_dmabuf_info = recv_dmabuf_info.clone();
        glib::idle_add_local(move || {
            let dmabuf_info = match recv_dmabuf_info.try_recv() {
                Ok(info) => info,
                Err(flume::TryRecvError::Empty) => {
                    return glib::ControlFlow::Continue;
                }
                Err(flume::TryRecvError::Disconnected) => {
                    return glib::ControlFlow::Break;
                }
            };

            let texture = render::build_dmabuf_texture(dmabuf_info);
            picture.set_paintable(Some(&texture));

            glib::ControlFlow::Continue
        });
    });

    let close_code = app.run().value();
    _ = send_close_code.send(close_code);
}

fn update_frame_size(
    mut windows: Query<(Entity, &mut AdwaitaWindow)>,
    render_device: Res<RenderDevice>,
    mut manual_texture_views: ResMut<ManualTextureViews>,
) {
    for (entity, mut window) in &mut windows {
        let span = trace_span!("update", window = %entity);
        let _span = span.enter();

        let (width, height) = (
            window.frame_width.load(Ordering::SeqCst),
            window.frame_height.load(Ordering::SeqCst),
        );
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            warn!("Frame of window {entity} has negative size: {width}x{height}");
            continue;
        };
        let (Some(width), Some(height)) = (NonZero::new(width), NonZero::new(height)) else {
            warn!("Frame of window {entity} has zero size: {width}x{height}");
            continue;
        };

        if Some((width, height)) == window.last_frame_size {
            continue;
        }
        window.last_frame_size = Some((width, height));

        let (texture_view, fd) = render::setup_render_target(width, height, render_device.as_ref());
        info!("New frame size {width}x{height} rendering to {fd}");
        manual_texture_views.insert(window.render_target_view_handle, texture_view);
        _ = window
            .send_dmabuf_info
            .try_send(DmabufInfo { width, height, fd });
    }
}

fn change_default_render_target(
    trigger: Trigger<OnInsert, Camera>,
    mut cameras: Query<&mut Camera>,
    primary_window: Query<&AdwaitaWindow, With<PrimaryAdwaitaWindow>>,
) {
    let Ok(primary_window) = primary_window.get_single() else {
        return;
    };

    let entity = trigger.entity();
    let mut camera = cameras
        .get_mut(entity)
        .expect("we are adding this component to this entity");
    if matches!(camera.target, RenderTarget::Window(WindowRef::Primary)) {
        camera.target = primary_window.render_target();
    }
}
