mod hal_custom;
mod render;

use std::{sync::Arc, thread, time::Duration};

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
        app.add_systems(PreUpdate, update_windows)
            .observe(change_default_render_target);
    }
}

#[derive(Debug, Component)]
pub struct AdwaitaWindow {
    render_target_view_handle: ManualTextureViewHandle,
    recv_size: flume::Receiver<(i32, i32)>,
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
    let manual_texture_views = world.resource::<ManualTextureViews>();
    let render_device = world.resource::<RenderDevice>();

    let view_handle = loop {
        let view_handle = ManualTextureViewHandle(rand::random());
        if !manual_texture_views.contains_key(&view_handle) {
            break view_handle;
        }
    };
    let (view, dmabuf_fd) = render::setup_render_target(DEFAULT_SIZE, render_device);

    world
        .resource_mut::<ManualTextureViews>()
        .insert(view_handle, view);

    let (send_size, recv_size) = flume::bounded::<(i32, i32)>(1);
    let (send_close_code, recv_close_code) = oneshot::channel::<i32>();
    thread::spawn(move || run(app_id, dmabuf_fd, send_size, send_close_code));
    world.entity_mut(entity).insert(AdwaitaWindow {
        render_target_view_handle: view_handle,
        recv_size,
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

fn run(
    app_id: String,
    dmabuf_fd: i32,
    send_size: flume::Sender<(i32, i32)>,
    send_close_code: oneshot::Sender<i32>,
) {
    let app = adw::Application::builder().application_id(app_id).build();

    let send_size = Arc::new(send_size);
    app.connect_activate(move |app| {
        let header_bar = adw::HeaderBar::new();

        let texture = render::build_dmabuf_texture(DEFAULT_SIZE, dmabuf_fd);
        let picture = gtk::Picture::builder().paintable(&texture).build();
        let graphics_offload = gtk::GraphicsOffload::builder()
            .black_background(true)
            .hexpand(true)
            .vexpand(true)
            .child(&picture)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header_bar);
        content.append(&graphics_offload);

        // glib::timeout_add_local(Duration::from_millis(100), {
        //     let graphics_offload = graphics_offload.clone();
        //     let send_size = send_size.clone();
        //     move || {
        //         graphics_offload.queue_draw();
        //         let (width, height) = (graphics_offload.width(), graphics_offload.height());
        //         _ = send_size.try_send((width, height));

        //         info!("new size = {width} x {height}");
        //         glib::ControlFlow::Continue
        //     }
        // });

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("First App")
            .default_width(1280)
            .default_height(720)
            .content(&content)
            .build();

        window.present();

        window
            .surface()
            .unwrap()
            .connect_notify_local(Some("state"), {
                let window = window.clone();
                let send_size = send_size.clone();
                move |_, _| {
                    let (width, height) = (window.width(), window.height());
                    _ = send_size.send((width, height));
                }
            });

        window.connect_default_width_notify({
            let send_size = send_size.clone();
            move |window| {
                let (width, height) = (window.width(), window.height());
                _ = send_size.send((width, height));
            }
        });

        window.connect_default_height_notify({
            let send_size = send_size.clone();
            move |window| {
                let (width, height) = (window.width(), window.height());
                _ = send_size.send((width, height));
            }
        });
    });

    let close_code = app.run().value();
    _ = send_close_code.send(close_code);
}

fn update_windows(mut windows: Query<&mut AdwaitaWindow>) {
    for mut window in &mut windows {
        let Some((width, height)) = window.recv_size.try_iter().last() else {
            continue;
        };

        info!("new size = {width}x{height}");
    }
}
