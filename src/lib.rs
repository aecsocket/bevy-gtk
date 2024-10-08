use std::sync::{Arc, Mutex};
use std::{num::NonZero, thread};

use adw::gtk::gdk_pixbuf::{Colorspace, Pixbuf};
use adw::{glib, gtk, prelude::*};
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssets;
use bevy::render::texture::GpuImage;
use bevy::render::{
    camera::RenderTarget,
    render_resource::{
        Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
    },
};
use bevy::render::{Render, RenderApp, RenderSet};
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
        let (send_close_code, recv_close_code) = oneshot::channel::<i32>();
        let (send_frame, recv_frame) = flume::bounded::<Frame>(1);

        {
            let application_id = self.application_id.clone();
            thread::spawn(move || run_adwaita_app(application_id, send_close_code, recv_frame));
        }

        app.insert_non_send_resource(RecvCloseCode(recv_close_code))
            .add_systems(PreStartup, setup_render_target)
            .add_systems(PreUpdate, exit_if_adwaita_closed)
            .observe(change_camera_render_target);
        let render_app = app.sub_app_mut(RenderApp);
        render_app
            .insert_resource(SendFrame(send_frame))
            .add_systems(Render, send_frame_to_adwaita.after(RenderSet::Render));
    }
}

#[derive(Debug)]
struct RecvCloseCode(oneshot::Receiver<i32>);

#[derive(Debug, Resource)]
struct SendFrame(flume::Sender<Frame>);

#[derive(Debug)]
struct Frame {
    data: Vec<u8>,
    width: u32,
}

#[derive(Debug, Clone, Resource)]
pub struct AdwaitaRenderTarget(pub Handle<Image>);

fn setup_render_target(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let size = Extent3d {
        width: 512,
        height: 512,
        ..default()
    };
    let mut image = Image {
        texture_descriptor: TextureDescriptor {
            label: None,
            size,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8UnormSrgb,
            mip_level_count: 1,
            sample_count: 1,
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        },
        ..default()
    };
    image.resize(size);
    image.data.fill(255);
    let render_target = images.add(image);
    commands.insert_resource(AdwaitaRenderTarget(render_target));
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
        camera.target = RenderTarget::Image(render_target.0.clone());
    }
}

fn run_adwaita_app(
    application_id: String,
    send_close_code: oneshot::Sender<i32>,
    recv_frame: flume::Receiver<Frame>,
) {
    struct State {
        recv_frame: flume::Receiver<Frame>,
        frame: Option<Pixbuf>,
    }

    let state = Arc::new(Mutex::new(State {
        recv_frame,
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
        drawing_area.set_draw_func(move |area, cairo, width, height| {
            let mut state = state.lock().unwrap();
            while let Ok(next_frame) = state.recv_frame.try_recv() {
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
        Colorspace::Rgb,
        true, // has_alpha
        8,    // bits_per_sample
        width as i32,
        height as i32,
        (width * 4) as i32, // row_stride
    )
}

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

fn send_frame_to_adwaita(
    images: Res<RenderAssets<GpuImage>>,
    render_target: Res<AdwaitaRenderTarget>,
    send_frame: Res<SendFrame>,
) {
    let Some(image) = images.get(&render_target.0) else {
        info!("no such img");
        return;
    };

    debug_assert_eq!(TextureFormat::Rgba8UnormSrgb, image.texture_format);
    match send_frame.0.try_send(Frame {
        data: image.data.clone(),
        width: image.width(),
    }) {
        Ok(()) | Err(flume::TrySendError::Full(_)) => {}
        Err(flume::TrySendError::Disconnected(_)) => {
            error!("Adwaita frame receiver disconnected");
        }
    }
}
