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
use atomicbox::AtomicOptionBox;
use bevy::{
    ecs::system::EntityCommand,
    prelude::*,
    render::{
        camera::{ManualTextureViewHandle, ManualTextureViews, RenderTarget},
        render_resource::TextureView,
        renderer::RenderDevice,
        Extract, Render, RenderApp, RenderSet,
    },
    window::WindowRef,
};
use gtk::glib;
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
        let render_app = app.sub_app_mut(RenderApp);
        render_app
            .add_systems(ExtractSchedule, extract_windows)
            .add_systems(Render, update_dmabufs.after(RenderSet::Render));
    }
}

// How does the texture creation work?
// - Window is initialized or resized and updates the `frame_width`, `frame_height`
// - App detects the new size is different from the old one
// - App allocates a new texture for the render target
// - App replaces the old render target texture with the new texture
//   - Window will still hold a reference to the view it's currently drawing,
//     so the resources (GPU memory etc.) won't be freed yet
// - App performs one render pass, and the render target now has content in it
// - App sends the new dmabuf and a clone of the render target texture over to the window
// - Window receives the new dmabuf and updates the Paintable to point to this
//   new dmabuf
// - Window holds on to the render target texture as well, replacing any old one it had
//   - If it had an old one, now it will be dropped

// Working notes:
// - Works fine at 512, 1280
// - Works fine at 1152, 1216, 1344
//   - Multiples of 64

#[derive(Debug, Component)]
pub struct AdwaitaWindow {
    // app only
    recv_close_code: SyncWrapper<oneshot::Receiver<i32>>,
    render_target_view_handle: ManualTextureViewHandle,
    last_frame_size: Option<UVec2>,
    next_draw_info: AtomicOptionBox<DrawInfo>,
    // shared
    shared_draw_info: Arc<AtomicOptionBox<DrawInfo>>,
    frame_width: Arc<AtomicI32>,
    frame_height: Arc<AtomicI32>,
}

#[derive(Debug)]
struct DrawInfo {
    dmabuf: DmabufInfo,
    texture_view: Option<TextureView>,
}

#[derive(Debug, Clone, Copy, Component, Reflect)]
#[reflect(Component)]
pub struct PrimaryAdwaitaWindow;

impl AdwaitaWindow {
    #[must_use]
    pub fn open(app_id: impl Into<String>) -> impl EntityCommand {
        let application_id = app_id.into();
        move |entity: Entity, world: &mut World| {
            open(
                entity,
                world,
                application_id,
                NonZero::new(1177).unwrap(),
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

    let shared_draw_info = Arc::new(AtomicOptionBox::<DrawInfo>::none());
    let (frame_width, frame_height) = (
        Arc::new(AtomicI32::new(width_i)),
        Arc::new(AtomicI32::new(height_i)),
    );
    let (send_close_code, recv_close_code) = oneshot::channel::<i32>();
    thread::spawn({
        let shared_draw_info = shared_draw_info.clone();
        let (frame_width, frame_height) = (frame_width.clone(), frame_height.clone());
        move || {
            run(
                app_id,
                shared_draw_info,
                frame_width,
                frame_height,
                send_close_code,
            )
        }
    });
    world.entity_mut(entity).insert(AdwaitaWindow {
        recv_close_code: SyncWrapper::new(recv_close_code),
        render_target_view_handle: view_handle,
        last_frame_size: None,
        next_draw_info: AtomicOptionBox::none(),
        // shared
        shared_draw_info,
        frame_width,
        frame_height,
    });
}

/// Info that that the Adwaita window requires to be able to make a
/// `GraphicsOffload` for rendering the Bevy app contents.
#[derive(Debug, Clone, Copy)]
struct DmabufInfo {
    /// Width and height of the buffer.
    size: UVec2,
    /// File descriptor of the buffer.
    fd: i32,
}

fn run(
    app_id: String,
    shared_draw_info: Arc<AtomicOptionBox<DrawInfo>>,
    frame_width: Arc<AtomicI32>,
    frame_height: Arc<AtomicI32>,
    send_close_code: oneshot::Sender<i32>,
) {
    let app = adw::Application::builder().application_id(app_id).build();

    let current_texture_view = Arc::new(AtomicOptionBox::<TextureView>::none());
    app.connect_activate(move |app| {
        let header_bar = adw::HeaderBar::new();

        let frame_picture = gtk::Picture::new();
        let graphics_offload = gtk::GraphicsOffload::builder()
            .black_background(true)
            .hexpand(true)
            .vexpand(true)
            .child(&frame_picture)
            .build();

        let frame = {
            // Use a trick to detect when the actual "frame" (Bevy app content)
            // is resized, and send this new frame size to the app.
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
        // content.append(&header_bar);
        content.append(&frame);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("First App")
            .default_width(frame_width.load(Ordering::SeqCst))
            .default_height(frame_height.load(Ordering::SeqCst))
            .content(&content)
            .build();

        window.present();

        let shared_draw_info = shared_draw_info.clone();
        let frame_picture = frame_picture.clone();
        let current_texture_view = current_texture_view.clone();
        glib::idle_add_local(move || {
            poll_window(&shared_draw_info, &frame_picture, &current_texture_view);
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
        // TODO fix this
        let (width, height) = (
            (width as u32 / 64).max(1) * 64,
            (height as u32 / 64).max(1) * 64,
        );
        let size = UVec2::new(width, height);
        if Some(size) == window.last_frame_size {
            continue;
        }
        window.last_frame_size = Some(size);
        info!("Updating render target size to {width}x{height}");

        let (new_texture_view, dmabuf_fd) =
            render::setup_render_target(size, render_device.as_ref());
        let old_texture_view = manual_texture_views
            .insert(window.render_target_view_handle, new_texture_view)
            .map(|old| old.texture_view);

        // However, we don't want to give this new dmabuf over to the window
        // yet, because we've literally just made it. It'll have some garbage
        // memory, and we don't want the window to try reading that.
        // Instead, we wait until the render app has done one full render, and
        // only then send over the new dmabuf info.
        let dmabuf_info = DmabufInfo {
            size,
            fd: dmabuf_fd,
        };
        // If the old value is dropped, it's OK - the associated texture view
        // will just be buffered up in the channel, and dropped the next time
        // the window swaps its target.
        window.next_draw_info.store(
            Some(Box::new(DrawInfo {
                dmabuf: dmabuf_info,
                texture_view: old_texture_view,
            })),
            Ordering::SeqCst,
        );
    }
}

#[derive(Debug, Component)]
struct RenderWindow {
    shared_draw_info: Arc<AtomicOptionBox<DrawInfo>>,
    next_draw_info: Option<Box<DrawInfo>>,
}

fn extract_windows(mut commands: Commands, windows: Extract<Query<&AdwaitaWindow>>) {
    for window in &windows {
        let Some(next_draw_info) = window.next_draw_info.take(Ordering::SeqCst) else {
            continue;
        };

        commands.spawn(RenderWindow {
            shared_draw_info: window.shared_draw_info.clone(),
            next_draw_info: Some(next_draw_info),
        });
    }
}

fn update_dmabufs(mut windows: Query<&mut RenderWindow>) {
    for mut window in &mut windows {
        let Some(dmabuf_info) = window.next_draw_info.take() else {
            continue;
        };

        window
            .shared_draw_info
            .store(Some(dmabuf_info), Ordering::SeqCst);
    }
}

fn poll_window(
    shared_draw_info: &Arc<AtomicOptionBox<DrawInfo>>,
    frame_picture: &gtk::Picture,
    current_texture_view: &Arc<AtomicOptionBox<TextureView>>,
) {
    let Some(mut draw_info) = shared_draw_info.take(Ordering::SeqCst) else {
        return;
    };

    let paintable = render::build_dmabuf_texture(draw_info.dmabuf);
    frame_picture.set_paintable(Some(&paintable));

    if let Some(texture_view) = draw_info.texture_view.take() {
        current_texture_view.store(Some(Box::new(texture_view)), Ordering::SeqCst);
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
