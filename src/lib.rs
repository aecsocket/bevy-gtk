use std::sync::{Arc, Mutex};
use std::{num::NonZero, thread};

use adw::gtk::gdk_pixbuf::{Colorspace, Pixbuf};
use adw::{gtk, prelude::*};
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_asset::RenderAssets;
use bevy::render::texture::GpuImage;
use bevy::render::{
    camera::RenderTarget,
    render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages},
};
use bevy::window::{ExitCondition, WindowPlugin, WindowRef};
use tracing::info;

#[derive(Debug)]
pub struct AdwaitaPlugin {
    pub application_id: String,
}

impl AdwaitaPlugin {
    #[must_use]
    pub const fn window_plugin() -> WindowPlugin {
        WindowPlugin {
            primary_window: None,
            exit_condition: ExitCondition::DontExit,
            close_when_requested: false,
        }
    }
}

impl Plugin for AdwaitaPlugin {
    fn build(&self, app: &mut App) {
        // Bevy -> Adwaita
        let (send_frame, recv_frame) = flume::bounded::<Frame>(1);
        // Adwaita -> Bevy
        let (send_frame_size, recv_frame_size) = flume::bounded::<(u32, u32)>(1);
        let (send_close_code, recv_close_code) = oneshot::channel::<i32>();

        {
            let application_id = self.application_id.clone();
            thread::spawn(move || {
                run_adwaita_app(application_id, recv_frame, send_frame_size, send_close_code)
            });
        }

        app.insert_resource(SendFrame(send_frame))
            .insert_non_send_resource(RecvCloseCode(recv_close_code))
            .insert_non_send_resource(RecvFrameSize(recv_frame_size))
            .add_systems(PreStartup, setup_render_target)
            .add_systems(PreUpdate, (exit_if_adwaita_closed, update_frame_size))
            .observe(change_camera_render_target);
    }
}

// event loop logic

#[derive(Debug)]
struct RecvCloseCode(oneshot::Receiver<i32>);

fn exit_if_adwaita_closed(
    close_code: NonSend<RecvCloseCode>,
    mut exit_events: EventWriter<AppExit>,
) {
    let exit = match close_code.0.try_recv() {
        Ok(code) => match NonZero::new(code as u8) {
            None => AppExit::Success,
            Some(code) => AppExit::Error(code),
        },
        Err(oneshot::TryRecvError::Disconnected) => AppExit::error(),
        Err(oneshot::TryRecvError::Empty) => return,
    };

    info!("Window closed, exiting ({exit:?})");
    exit_events.send(exit);
}

// sending frames

#[derive(Debug, Clone, Deref, Resource)]
pub struct AdwaitaRenderTarget(pub Handle<Image>);

#[derive(Debug)]
struct RecvFrameSize(flume::Receiver<(u32, u32)>);

#[derive(Debug, Resource)]
struct SendFrame(flume::Sender<Frame>);

#[derive(Debug)]
struct Frame {
    data: Vec<u8>,
    width: u32,
}

fn setup_render_target(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let size = Extent3d {
        width: 512,
        height: 512,
        ..default()
    };

    let mut render_image = Image::new_fill(
        size,
        TextureDimension::D2,
        &[0; 4],
        // Cairo expects RGBA
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    render_image.texture_descriptor.usage |=
        TextureUsages::COPY_SRC | TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
    let render_image = images.add(render_image);

    commands
        .spawn(Readback::texture(render_image.clone()))
        .observe(|trigger: Trigger<ReadbackComplete>| {
            info!("got readback");
            // let data = trigger.event().0.clone();
            // send_frame.0.try_send(Frame { data, width: 512 });
        });
    info!("spawned readback");

    commands.insert_resource(AdwaitaRenderTarget(render_image));
}

fn change_camera_render_target(
    trigger: Trigger<OnInsert, Camera>,
    mut cameras: Query<&mut Camera>,
    render_target: Res<AdwaitaRenderTarget>,
) {
    let entity = trigger.entity();
    let mut camera = cameras
        .get_mut(entity)
        .expect("should exist because we are inserting this component onto this entity");

    if matches!(camera.target, RenderTarget::Window(WindowRef::Primary)) {
        camera.target = render_target.0.clone().into();
    }
}

fn update_frame_size(
    recv_frame_size: NonSend<RecvFrameSize>,
    render_target: Res<AdwaitaRenderTarget>,
    mut images: ResMut<Assets<Image>>,
) {
    // let Some((width, height)) = recv_frame_size.0.try_iter().last() else {
    //     return;
    // };

    // let image = images.get_mut(&render_target.0).unwrap();
    // image.resize(Extent3d {
    //     width,
    //     height,
    //     depth_or_array_layers: 1,
    // });
}

// Adwaita-side logic

fn run_adwaita_app(
    application_id: String,
    recv_frame: flume::Receiver<Frame>,
    send_frame_size: flume::Sender<(u32, u32)>,
    send_close_code: oneshot::Sender<i32>,
) {
    struct State {
        recv_frame: flume::Receiver<Frame>,
        send_frame_size: flume::Sender<(u32, u32)>,
        frame: Option<Pixbuf>,
    }

    // TODO Cell?
    let state = Arc::new(Mutex::new(State {
        recv_frame,
        send_frame_size,
        frame: None,
    }));

    let app = adw::Application::builder()
        .application_id(application_id)
        .build();

    let state = state.clone();
    app.connect_activate(move |app| {
        let header_bar = adw::HeaderBar::new();
        let drawing_area = gtk::DrawingArea::builder()
            .hexpand(true)
            .vexpand(true)
            .build();

        let state = state.clone();
        drawing_area.set_draw_func(move |_, cairo, width, height| {
            let mut state = state.lock().unwrap();

            // tell Bevy what the new render dimensions should be
            debug_assert!(width > 0);
            debug_assert!(height > 0);
            let _ = state
                .send_frame_size
                .try_send((width as u32, height as u32));

            // receive the next frame to draw
            // in case we have multiple frames buffered up,
            // drop all the intermediary ones and just make a pixbuf from the last one
            if let Some(next_frame) = state.recv_frame.try_iter().last() {
                state.frame = Some(pixbuf_from(next_frame));
            }

            if let Some(frame) = state.frame.as_ref() {
                cairo.set_source_pixbuf(frame, 0.0, 0.0);
                cairo.paint().unwrap();

                cairo.set_source_rgb(1.0, 0.0, 0.0);
                cairo.rectangle(16.0, 16.0, 64.0, 64.0);
                cairo.fill().unwrap();

                info!("mom look i rendered {} x {}", frame.width(), frame.height());
            }
        });

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header_bar);
        content.append(&drawing_area);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Whatever app TODO")
            .content(&content)
            .build();
        window.present();
    });

    let close_code = app.run().value();
    let _ = send_close_code.send(close_code);
}

fn pixbuf_from(frame: Frame) -> Pixbuf {
    let Frame { data, width } = frame;

    debug_assert!(data.len() % 4 == 0);
    let pixels = (data.len() / 4) as u32;
    debug_assert!(pixels % width == 0);
    let height = pixels / width;

    Pixbuf::from_mut_slice(
        data,
        Colorspace::Rgb, // this is why we explicitly set the TextureFormat
        true,            // has_alpha
        8,               // bits_per_sample
        width as i32,
        height as i32,
        (width * 4) as i32, // row_stride
    )
}
