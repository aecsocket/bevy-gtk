mod hal_custom;
mod render;

use std::{
    any::type_name,
    num::NonZero,
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc, Mutex,
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
        settings::WgpuSettings,
        Extract, Render, RenderApp, RenderPlugin, RenderSet,
    },
    window::{ExitCondition, WindowRef},
};
use gtk::glib;
use sync_wrapper::SyncWrapper;

#[derive(Debug)]
pub struct AdwaitaPlugin {
    pub primary_window: Option<AdwaitaWindowConfig>,
}

impl AdwaitaPlugin {
    #[must_use]
    pub fn window_plugin() -> WindowPlugin {
        WindowPlugin {
            primary_window: None,
            exit_condition: ExitCondition::DontExit,
            close_when_requested: false,
        }
    }

    #[must_use]
    pub fn render_plugin(settings: WgpuSettings) -> RenderPlugin {
        let render_creation = render::create_renderer(settings);
        RenderPlugin {
            render_creation,
            synchronous_pipeline_compilation: false,
        }
    }
}

impl Plugin for AdwaitaPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreUpdate, update_frame_size)
            .observe(change_camera_default_render_target)
            .observe(update_existing_camera_default_render_targets);

        let render_app = app.sub_app_mut(RenderApp);
        render_app
            .add_systems(ExtractSchedule, extract_windows)
            .add_systems(Render, update_dmabufs.after(RenderSet::Render));

        if let Some(config) = self.primary_window.clone() {
            app.add_systems(Startup, spawn_primary_window_system(config));
        }
    }
}

fn spawn_primary_window_system(config: AdwaitaWindowConfig) -> impl IntoSystem<(), (), ()> {
    IntoSystem::into_system(move |mut commands: Commands| {
        commands
            .spawn_empty()
            .add(AdwaitaWindow::open(config.clone()))
            .insert(PrimaryAdwaitaWindow);
    })
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

#[derive(Debug, Clone)]
pub struct AdwaitaWindowConfig {
    pub app_id: String,
}

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
    pub fn open(config: AdwaitaWindowConfig) -> impl EntityCommand {
        move |entity: Entity, world: &mut World| {
            open(
                entity,
                world,
                config,
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
    config: AdwaitaWindowConfig,
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
                config.app_id,
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

    let current_draw_info = Arc::new(Mutex::new(None::<DrawInfo>));
    app.connect_activate(move |app| {
        let window_controls = gtk::WindowControls::builder()
            .side(gtk::PackType::End)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .build();

        let frame_picture = gtk::Picture::new();
        frame_picture.add_tick_callback({
            let current_draw_info = current_draw_info.clone();
            move |frame_picture, _| {
                (|| {
                    let Ok(draw_info) = current_draw_info.try_lock() else {
                        return;
                    };
                    let Some(draw_info) = draw_info.as_ref() else {
                        return;
                    };

                    let paintable = render::build_dmabuf_texture(&draw_info.dmabuf);
                    frame_picture.queue_draw();
                    frame_picture.set_paintable(Some(&paintable));
                })();
                glib::ControlFlow::Continue
            }
        });

        let frame = {
            let graphics_offload = gtk::GraphicsOffload::builder()
                .black_background(true)
                .hexpand(true)
                .vexpand(true)
                .child(&frame_picture)
                .build();

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

        let content = gtk::Overlay::new();
        content.set_child(Some(&frame));
        content.add_overlay(&window_controls);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("First App")
            .default_width(frame_width.load(Ordering::SeqCst))
            .default_height(frame_height.load(Ordering::SeqCst))
            .content(&content)
            .build();

        window.present();

        // don't use `glib::idle_add` so that we have some delay
        window.add_tick_callback({
            let shared_draw_info = shared_draw_info.clone();
            let current_draw_info = current_draw_info.clone();
            move |_, _| {
                (|| {
                    let Some(draw_info) = shared_draw_info.take(Ordering::SeqCst) else {
                        return;
                    };
                    *current_draw_info.lock().unwrap() = Some(*draw_info);
                })();
                glib::ControlFlow::Continue
            }
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
    const WORKING_MULTIPLES: u32 = 64;

    for (entity, mut window) in &mut windows {
        let span = trace_span!("update", window = %entity);
        let _span = span.enter();

        let (width, height) = (
            window.frame_width.load(Ordering::SeqCst),
            window.frame_height.load(Ordering::SeqCst),
        );
        // TODO fix this
        let (width, height) = (
            (width as u32 / WORKING_MULTIPLES).max(1) * WORKING_MULTIPLES,
            (height as u32).max(1),
        );
        let size = UVec2::new(width, height);
        if Some(size) == window.last_frame_size {
            continue;
        }
        window.last_frame_size = Some(size);
        trace!("Updating render target size to {width}x{height}");

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
        // If the old value is dropped, it's OK - that texture view will be
        // dropped, and GPU resources will be cleaned up.
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

fn change_camera_default_render_target(
    trigger: Trigger<OnInsert, Camera>,
    mut cameras: Query<&mut Camera>,
    primary_windows: Query<&AdwaitaWindow, With<PrimaryAdwaitaWindow>>,
) {
    let Ok(primary_window) = primary_windows.get_single() else {
        return;
    };

    let entity = trigger.entity();
    let mut camera = cameras
        .get_mut(entity)
        .expect("we are inserting this component into this entity");
    if matches!(camera.target, RenderTarget::Window(WindowRef::Primary)) {
        camera.target = primary_window.render_target();
    }
}

fn update_existing_camera_default_render_targets(
    trigger: Trigger<OnInsert, PrimaryAdwaitaWindow>,
    windows: Query<&AdwaitaWindow>,
    mut cameras: Query<&mut Camera>,
) {
    let entity = trigger.entity();
    let window = windows.get(entity).unwrap_or_else(|_| {
        panic!(
            "entity with `{}` should have `{}`",
            type_name::<PrimaryAdwaitaWindow>(),
            type_name::<AdwaitaWindow>()
        );
    });

    for mut camera in &mut cameras {
        if matches!(camera.target, RenderTarget::Window(WindowRef::Primary)) {
            camera.target = window.render_target();
        }
    }
}
