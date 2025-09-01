use {
    bevy::{
        camera::{ManualTextureViewHandle, RenderTarget},
        prelude::*,
        render::renderer::RenderDevice,
        winit::WinitPlugin,
    },
    bevy_gtk::{GtkPlugin, NewWindowContent, render::DmabufTexture},
    bevy_render::texture::ManualTextureView,
    bevy_window::PrimaryWindow,
    wgpu::TextureViewDescriptor,
};

#[derive(Debug, clap::Parser)]
struct Args {
    #[arg(long, value_enum, default_value_t = DemoMode::Adw)]
    mode: DemoMode,
    #[arg(long)]
    no_titlebar: bool,
    #[arg(long)]
    titlebar_transparent: bool,
    #[arg(long)]
    no_title: bool,
    #[arg(long)]
    no_buttons: bool,
}

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
enum DemoMode {
    Winit,
    Gtk,
    #[default]
    Adw,
}

const APP_ID: &str = "io.github.aecsocket.bevy_gtk";

fn main() -> AppExit {
    let args = <Args as clap::Parser>::parse();
    let mut app = App::new();

    let default_plugins = DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            titlebar_shown: !args.no_titlebar,
            titlebar_transparent: args.titlebar_transparent,
            titlebar_show_title: !args.no_title,
            titlebar_show_buttons: !args.no_buttons,
            ..default()
        }),
        ..default()
    });
    match args.mode {
        DemoMode::Winit => app.add_plugins(default_plugins),
        DemoMode::Gtk => app.add_plugins((
            GtkPlugin::new(APP_ID).without_adw(),
            default_plugins.build().disable::<WinitPlugin>(),
        )),
        DemoMode::Adw => app.add_plugins((
            GtkPlugin::new(APP_ID).with_adw(),
            default_plugins.build().disable::<WinitPlugin>(),
        )),
    };
    app.add_systems(Startup, setup)
        .add_systems(Update, rotate_cube)
        .run()
}

#[derive(Debug, Component)]
struct Rotating;

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut manual_texture_views: ResMut<ManualTextureViews>,
    render_device: Res<RenderDevice>,
    window: Single<Entity, With<PrimaryWindow>>,
) {
    // circular base
    commands.spawn((
        Mesh3d(meshes.add(Circle::new(4.0))),
        MeshMaterial3d(materials.add(Color::WHITE)),
        Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
    ));
    // cube
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
        MeshMaterial3d(materials.add(Color::srgb_u8(124, 144, 255))),
        Transform::from_xyz(0.0, 0.5, 0.0),
        Rotating,
    ));
    // light
    commands.spawn((
        PointLight {
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(4.0, 8.0, 4.0),
    ));

    // camera
    let (width, height) = (512, 512);
    let texture = DmabufTexture::new(render_device.wgpu_device(), width, height).unwrap();
    let wg_texture_view = texture.create_view(&TextureViewDescriptor::default());
    let manual_texture_view = ManualTextureViewHandle(0);
    manual_texture_views.insert(
        manual_texture_view,
        ManualTextureView {
            texture_view: wg_texture_view.into(),
            size: (width, height).into(),
            format: texture.format(),
        },
    );

    commands.spawn((
        Camera {
            target: RenderTarget::TextureView(manual_texture_view),
            ..default()
        },
        Camera3d::default(),
        Transform::from_xyz(-2.5, 4.5, 9.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    commands
        .entity(*window)
        .insert(NewWindowContent::from(move || {
            let content_texture = bevy_gtk::render::gtk_dmabuf(&texture).unwrap();
            let content_picture = gtk4::Picture::for_paintable(&content_texture);
            gtk4::GraphicsOffload::builder()
                .black_background(true)
                .child(&content_picture)
                .hexpand(true)
                .vexpand(true)
                .build()
        }));
}

fn rotate_cube(time: Res<Time>, mut query: Query<&mut Transform, With<Rotating>>) {
    for mut transform in &mut query {
        transform.rotate_x(0.9 * time.delta_secs());
        transform.rotate_y(0.7 * time.delta_secs());
    }
}
