extern crate gdk4 as gdk;
extern crate gio;
extern crate gtk4 as gtk;
#[cfg(feature = "adwaita")]
extern crate libadwaita as adw;

macro_rules! if_adw {
    ($with_adw:expr, $without_adw:expr $(,)?) => {{
        #[cfg(feature = "adwaita")]
        {
            $with_adw
        }
        #[cfg(not(feature = "adwaita"))]
        {
            $without_adw
        }
    }};
    ($is_adw:expr, $with_adw:expr, $without_adw:expr $(,)?) => {{
        #[cfg(feature = "adwaita")]
        {
            if $is_adw { $with_adw } else { $without_adw }
        }
        #[cfg(not(feature = "adwaita"))]
        {
            $without_adw
        }
    }};
}

use {
    bevy_app::{PluginsState, prelude::*},
    bevy_derive::Deref,
    bevy_ecs::prelude::*,
    glib::clone,
    gtk::prelude::*,
    log::debug,
    std::{
        cell::{Cell, RefCell},
        rc::Rc,
    },
};

mod window;
pub use window::*;

#[cfg(feature = "render")]
pub mod render;

#[derive(Default)]
pub struct GtkPlugin {
    pub use_adw: bool,
    pub app_id: Option<String>,
    pub app_flags: gio::ApplicationFlags,
}

#[derive(Debug, Clone, Deref)]
pub struct GtkApplication(pub gtk::Application);

impl GtkPlugin {
    pub fn new(app_id: impl Into<String>) -> Self {
        Self {
            use_adw: if_adw!(true, false),
            app_id: Some(app_id.into()),
            app_flags: gio::ApplicationFlags::empty(),
        }
    }

    pub fn with_adw(self) -> Self {
        Self {
            use_adw: true,
            ..self
        }
    }

    pub fn without_adw(self) -> Self {
        Self {
            use_adw: false,
            ..self
        }
    }
}

impl Plugin for GtkPlugin {
    fn build(&self, app: &mut App) {
        let gtk_app = if_adw!(
            self.use_adw,
            adw::Application::new(self.app_id.as_deref(), self.app_flags)
                .upcast::<gtk::Application>(),
            gtk::Application::new(self.app_id.as_deref(), self.app_flags),
        );
        // prevent app closing when there are no windows;
        // this is `bevy_window`'s responsibility
        let app_hold = gtk_app.hold();

        let (tx_activated, rx_activated) = oneshot::channel::<()>();
        let tx_activated = RefCell::new(Some(tx_activated));
        gtk_app.connect_activate(move |_| {
            if let Some(tx) = tx_activated.take() {
                _ = tx.send(());
            }
        });

        debug!("Registering GTK app");
        gtk_app
            .register(None::<&gio::Cancellable>)
            .expect("failed to register GTK app");
        debug!("Activating GTK app");
        gtk_app.activate();
        rx_activated
            .recv()
            .expect("channel dropped while activating GTK app");
        debug!("App activated");

        #[cfg(feature = "render")]
        render::post_activate(app);

        app.insert_non_send_resource(app_hold)
            .insert_non_send_resource(GtkApplication(gtk_app.clone()))
            .insert_non_send_resource(GtkWindows::new(self.use_adw))
            .set_runner(|bevy_app| gtk_runner(bevy_app, gtk_app))
            .add_systems(
                Last,
                (
                    window::create_bevy_to_gtk,
                    window::despawn,
                    window::sync_bevy_to_gtk,
                    window::sync_gtk_to_bevy,
                )
                    .chain(),
            );
    }
}

fn gtk_runner(mut bevy_app: App, gtk_app: gtk::Application) -> AppExit {
    if bevy_app.plugins_state() == PluginsState::Ready {
        bevy_app.finish();
        bevy_app.cleanup();
    }

    debug!("Starting GTK app");

    let bevy_exit = Rc::new(Cell::new(None::<AppExit>));
    glib::idle_add_local(clone!(
        #[strong]
        bevy_exit,
        move || {
            if let Some(exit) = idle_update(&mut bevy_app) {
                bevy_exit.set(Some(exit));
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        }
    ));

    // don't handle CLI args, since that's Bevy's job
    let gtk_exit = gtk_app.run_with_args::<&str>(&[]);
    debug!("GTK app exited with code {gtk_exit:?}");
    bevy_exit
        .take()
        .unwrap_or_else(|| AppExit::from_code(gtk_exit.get()))
}

fn idle_update(bevy_app: &mut App) -> Option<AppExit> {
    if bevy_app.plugins_state() == PluginsState::Cleaned {
        bevy_app.update();
    }

    bevy_app.should_exit()
}

/*mod adwaita_app;
mod hal_custom;
mod render;

use {
    adwaita_app::{WindowCommand, WindowOpen},
    atomicbox::AtomicOptionBox,
    bevy::{
        ecs::system::EntityCommand,
        prelude::*,
        render::{
            Extract, Render, RenderApp, RenderPlugin, RenderSet,
            camera::{ManualTextureViewHandle, ManualTextureViews, RenderTarget},
            renderer::RenderDevice,
            settings::WgpuSettings,
        },
        window::{ExitCondition, WindowRef},
    },
    render::{DmabufInfo, FrameInfo},
    std::{
        any::type_name,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicI32, Ordering},
        },
        thread,
    },
};

#[derive(Clone)]
pub struct AdwaitaWindowPlugin {
    pub primary_window_config: Option<AdwaitaWindowConfig>,
    pub exit_condition: ExitCondition,
}

impl Default for AdwaitaWindowPlugin {
    fn default() -> Self {
        Self {
            primary_window_config: Some(AdwaitaWindowConfig::default()),
            exit_condition: ExitCondition::OnAllClosed,
        }
    }
}

impl Plugin for AdwaitaWindowPlugin {
    fn build(&self, app: &mut App) {
        let (send_window_open, recv_window_open) = flume::bounded::<WindowOpen>(1);
        thread::spawn(|| adwaita_app::main_thread_loop(recv_window_open));

        app.insert_resource(SendWindowOpen(send_window_open))
            .add_systems(PreUpdate, poll_windows)
            .observe(update_default_camera_render_target)
            .observe(update_existing_cameras_render_target);

        match self.exit_condition {
            ExitCondition::OnPrimaryClosed => {
                app.add_systems(PostUpdate, exit_on_primary_closed);
            }
            ExitCondition::OnAllClosed => {
                app.add_systems(PostUpdate, exit_on_all_closed);
            }
            ExitCondition::DontExit => {}
        }

        let render_app = app.sub_app_mut(RenderApp);
        render_app
            .add_systems(ExtractSchedule, extract_windows)
            .add_systems(Render, send_frame_to_windows.after(RenderSet::Render))
            .add_systems(Last, put_back_next_frame_if_not_sent);

        if let Some(config) = self.primary_window_config.clone() {
            let world = app.world_mut();
            let entity = world.spawn_empty().id();
            AdwaitaWindow::open(config).apply(entity, world);
            world.entity_mut(entity).insert(PrimaryAdwaitaWindow);
        }
    }
}

impl AdwaitaWindowPlugin {
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

#[derive(Debug, Component)]
pub struct AdwaitaWindow {
    send_command: flume::Sender<WindowCommand>,
    render_target_width: Arc<AtomicI32>,
    render_target_height: Arc<AtomicI32>,
    scale_factor: Arc<AtomicI32>,
    shared_next_frame: Arc<AtomicOptionBox<FrameInfo>>,
    closed: Arc<AtomicBool>,
    render_target_handle: ManualTextureViewHandle,
    last_render_target_size: UVec2,
    next_frame_to_render: Arc<AtomicOptionBox<FrameInfo>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Component, Reflect)]
#[reflect(Default, Component)]
pub struct PrimaryAdwaitaWindow;

#[derive(Debug, Clone, Reflect)]
#[reflect(Default)]
pub struct AdwaitaWindowConfig {
    pub width: u32,
    pub height: u32,
    pub title: String,
    pub resizable: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    pub header_bar: AdwaitaHeaderBar,
}

impl Default for AdwaitaWindowConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            title: "App".into(),
            resizable: true,
            maximized: false,
            fullscreen: false,
            header_bar: AdwaitaHeaderBar::default(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Reflect)]
#[reflect(Default)]
pub enum AdwaitaHeaderBar {
    #[default]
    Full,
    OverContent,
    None,
}

#[derive(Debug, Resource)]
struct SendWindowOpen(flume::Sender<WindowOpen>);

impl AdwaitaWindow {
    #[must_use]
    pub fn open(config: AdwaitaWindowConfig) -> impl EntityCommand {
        move |entity, world: &mut World| {
            info!(
                "Creating new Adwaita window \"{}\" ({entity})",
                config.title
            );

            let (send_command, recv_command) = flume::bounded::<WindowCommand>(16);
            let render_target_width = Arc::new(AtomicI32::new(-1));
            let render_target_height = Arc::new(AtomicI32::new(-1));
            let scale_factor = Arc::new(AtomicI32::new(-1));
            let shared_next_frame = Arc::new(AtomicOptionBox::<FrameInfo>::none());
            let closed = Arc::new(AtomicBool::new(false));
            let request = WindowOpen {
                config,
                recv_command,
                render_target_width: render_target_width.clone(),
                render_target_height: render_target_height.clone(),
                shared_next_frame: shared_next_frame.clone(),
                scale_factor: scale_factor.clone(),
                closed: closed.clone(),
            };

            let manual_texture_views = world.resource::<ManualTextureViews>();
            let render_target_handle = loop {
                let handle = ManualTextureViewHandle(rand::random());
                if !manual_texture_views.contains_key(&handle) {
                    break handle;
                }
            };

            world.entity_mut(entity).insert(AdwaitaWindow {
                send_command,
                render_target_width,
                render_target_height,
                scale_factor,
                shared_next_frame,
                closed,
                render_target_handle,
                last_render_target_size: UVec2::new(0, 0),
                next_frame_to_render: Arc::new(AtomicOptionBox::none()),
            });
            world
                .resource::<SendWindowOpen>()
                .0
                .send(request)
                .expect("Adwaita main thread dropped");
        }
    }

    #[must_use]
    pub const fn render_target_handle(&self) -> ManualTextureViewHandle {
        self.render_target_handle
    }

    #[must_use]
    pub const fn render_target(&self) -> RenderTarget {
        RenderTarget::TextureView(self.render_target_handle)
    }

    pub fn set_maximized(&self, maximized: bool) {
        _ = self
            .send_command
            .send(WindowCommand::SetMaximized(maximized));
    }

    pub fn maximize(&self) {
        self.set_maximized(true);
    }

    pub fn unmaximize(&self) {
        self.set_maximized(false);
    }

    pub fn set_fullscreen(&self, fullscreen: bool) {
        _ = self
            .send_command
            .send(WindowCommand::SetFullscreen(fullscreen));
    }

    pub fn fullscreen(&self) {
        self.set_fullscreen(true);
    }

    pub fn unfullscreen(&self) {
        self.set_fullscreen(false);
    }

    pub fn set_title(&self, title: impl Into<String>) {
        let title = title.into();
        _ = self.send_command.send(WindowCommand::SetTitle(title));
    }
}

fn update_default_camera_render_target(
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

fn update_existing_cameras_render_target(
    trigger: Trigger<OnInsert, PrimaryAdwaitaWindow>,
    windows: Query<&AdwaitaWindow>,
    mut cameras: Query<&mut Camera>,
) {
    let entity = trigger.entity();
    let window = windows.get(entity).unwrap_or_else(|_| {
        panic!(
            "inserting `{}` onto {entity} without `{}`",
            type_name::<PrimaryAdwaitaWindow>(),
            type_name::<AdwaitaWindow>()
        )
    });

    for mut camera in &mut cameras {
        if matches!(camera.target, RenderTarget::Window(WindowRef::Primary)) {
            camera.target = window.render_target();
        }
    }
}

fn exit_on_primary_closed(
    mut app_exit_events: EventWriter<AppExit>,
    windows: Query<(), (With<AdwaitaWindow>, With<PrimaryAdwaitaWindow>)>,
) {
    if windows.is_empty() {
        info!("Primary Adwaita window was closed, exiting");
        app_exit_events.send(AppExit::Success);
    }
}

fn exit_on_all_closed(
    mut app_exit_events: EventWriter<AppExit>,
    windows: Query<(), With<AdwaitaWindow>>,
) {
    if windows.is_empty() {
        info!("No Adwaita windows are open, exiting");
        app_exit_events.send(AppExit::Success);
    }
}

fn poll_windows(
    mut commands: Commands,
    mut windows: Query<(Entity, &mut AdwaitaWindow)>,
    render_device: Res<RenderDevice>,
    mut manual_texture_views: ResMut<ManualTextureViews>,
) {
    for (entity, mut window) in &mut windows {
        if window.closed.load(Ordering::SeqCst) {
            info!("Adwaita window {entity} closed");
            commands.entity(entity).despawn_recursive();
            continue;
        }

        let (width, height, scale_factor) = (
            window.render_target_width.load(Ordering::SeqCst),
            window.render_target_height.load(Ordering::SeqCst),
            window.scale_factor.load(Ordering::SeqCst),
        );
        let (Ok(width), Ok(height), Ok(scale_factor)) = (
            u32::try_from(width),
            u32::try_from(height),
            u32::try_from(scale_factor),
        ) else {
            continue;
        };

        let size = UVec2::new(width.max(1) * scale_factor, height.max(1) * scale_factor);
        if size == window.last_render_target_size {
            continue;
        }
        info!("Window resized to {size}");
        window.last_render_target_size = size;

        let (manual_texture_view, dmabuf_fd) =
            render::setup_render_target(size, render_device.as_ref());
        // give a shared ref of this texture view to the Adwaita app
        // so that, even if *we* drop it while the window is rendering this frame,
        // the GPU resources won't be deallocated until the window *also* drops it
        let texture_view = manual_texture_view.texture_view.clone();
        manual_texture_views.insert(window.render_target_handle.clone(), manual_texture_view);
        let next_frame_info = FrameInfo {
            dmabuf: DmabufInfo {
                size,
                fd: dmabuf_fd,
            },
            _texture_view: texture_view,
        };
        info!("Stored next frame info {next_frame_info:?}");
        window
            .next_frame_to_render
            .store(Some(Box::new(next_frame_info)), Ordering::SeqCst);
    }
}

#[derive(Debug, Component)]
struct RenderWindow {
    shared_next_frame: Arc<AtomicOptionBox<FrameInfo>>,
    next_frame_to_render: Arc<AtomicOptionBox<FrameInfo>>,
    next_frame_to_send: Option<Box<FrameInfo>>,
}

fn extract_windows(mut commands: Commands, windows: Extract<Query<&AdwaitaWindow>>) {
    info!("-- RUNNING extract_windows");
    for window in &windows {
        let Some(next_frame_to_send) = window.next_frame_to_render.take(Ordering::SeqCst) else {
            continue;
        };
        info!("--extract: Got next frame info {next_frame_to_send:?}");

        commands.spawn(RenderWindow {
            shared_next_frame: window.shared_next_frame.clone(),
            next_frame_to_render: window.next_frame_to_render.clone(),
            next_frame_to_send: Some(next_frame_to_send),
        });
    }
}

fn send_frame_to_windows(mut windows: Query<&mut RenderWindow>) {
    info!("-- RUNNING send_frame_info_to_windows");
    for mut window in &mut windows {
        let Some(next_frame_info) = window.next_frame_to_send.take() else {
            continue;
        };

        info!("Sending next frame {next_frame_info:?} now.");
        window
            .shared_next_frame
            .store(Some(next_frame_info), Ordering::SeqCst);
    }
}

fn put_back_next_frame_if_not_sent(mut windows: Query<&mut RenderWindow>) {
    for mut window in &mut windows {
        if let Some(frame_info) = window.next_frame_to_send.take() {
            window
                .next_frame_to_render
                .store(Some(frame_info), Ordering::SeqCst);
        }
    }
}

//
//            | set `next_to_render`            | set `next_to_render`
//            v                     extract     v
// update  ---+-------------------|---------|---+--------------|---
// render                         |-+-------|--------------+-+-|---
//                                  ^                      ^ ^
//            take `next_to_render` |                      | | in `Last`:
//          store in `next_to_send` |                      | | if we still have
// a `next_to_send`,                                                         | | put it back
//                                 after RenderSet::Render |
//                            take and send `next_to_send` |
*/
