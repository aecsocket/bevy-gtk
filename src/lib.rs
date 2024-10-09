mod hal_custom;
mod render;

use std::thread;

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
use gtk::gdk;
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
        app.observe(change_default_render_target);
    }
}

#[derive(Debug, Component)]
pub struct AdwaitaWindow {
    render_target_view_handle: ManualTextureViewHandle,
    recv_close_code: SyncWrapper<oneshot::Receiver<i32>>,
}

#[derive(Debug, Clone, Copy, Component, Reflect)]
#[reflect(Component)]
pub struct PrimaryAdwaitaWindow;

const DEFAULT_SIZE: UVec2 = UVec2::new(512, 512);

impl AdwaitaWindow {
    #[must_use]
    pub fn open(app_id: impl Into<String>) -> impl EntityCommand {
        let application_id = app_id.into();
        move |entity: Entity, world: &mut World| open(entity, world, application_id)
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

fn open(entity: Entity, world: &mut World, app_id: String) {
    let (send_close_code, recv_close_code) = oneshot::channel::<i32>();

    let (view_handle, dmabuf_fd) =
        world.resource_scope(|world, mut manual_texture_views: Mut<ManualTextureViews>| {
            let view_handle = loop {
                let view_handle = ManualTextureViewHandle(rand::random());
                if !manual_texture_views.contains_key(&view_handle) {
                    break view_handle;
                }
            };

            let dmabuf_fd = world.resource_scope(|world, render_device: Mut<RenderDevice>| {
                render::setup_render_target(
                    DEFAULT_SIZE,
                    view_handle,
                    manual_texture_views.as_mut(),
                    render_device.as_ref(),
                )
            });

            (view_handle, dmabuf_fd)
        });

    thread::spawn(move || run(app_id, dmabuf_fd, send_close_code));
    world.entity_mut(entity).insert(AdwaitaWindow {
        render_target_view_handle: view_handle,
        recv_close_code: SyncWrapper::new(recv_close_code),
    });
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

fn run(app_id: String, dmabuf_fd: i32, send_close_code: oneshot::Sender<i32>) {
    let app = adw::Application::builder().application_id(app_id).build();

    app.connect_activate(move |app| {
        let header_bar = adw::HeaderBar::new();

        let texture = render::build_dmabuf_texture(DEFAULT_SIZE, dmabuf_fd);
        let picture = gtk::Picture::builder().paintable(&texture).build();
        let graphics_offload = gtk::GraphicsOffload::builder()
            // .black_background(true)
            .hexpand(true)
            .vexpand(true)
            .child(&picture)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header_bar);
        content.append(&graphics_offload);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("First App")
            .default_width(1280)
            .default_height(720)
            .content(&content)
            .build();
        window.present();
    });

    let close_code = app.run().value();
    let _ = send_close_code.send(close_code);
}
