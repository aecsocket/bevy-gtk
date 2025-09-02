use {
    bevy::{camera::RenderTarget, prelude::*, window::PrimaryWindow, winit::WinitPlugin},
    bevy_gtk::{GtkInitPlugin, GtkPlugin, NewWindowContent, render::GtkViewports},
};

#[derive(Debug, clap::Parser)]
#[allow(clippy::struct_excessive_bools, reason = "`clap` args")]
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

const APP_ID: &str = "io.github.aecsocket.BevyGtk";

fn main() -> AppExit {
    let args = <Args as clap::Parser>::parse();
    let mut app = App::new();

    let default_plugins = DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            titlebar_shown: !args.no_titlebar,
            titlebar_transparent: args.titlebar_transparent,
            titlebar_show_title: !args.no_title,
            titlebar_show_buttons: !args.no_buttons,
            resize_constraints: WindowResizeConstraints {
                min_width: 360.0,
                min_height: 200.0,
                max_width: f32::INFINITY,
                max_height: f32::INFINITY,
            },
            ..default()
        }),
        ..default()
    });
    match args.mode {
        DemoMode::Winit => app.add_plugins(default_plugins),
        DemoMode::Gtk => app.add_plugins((
            GtkInitPlugin,
            default_plugins.build().disable::<WinitPlugin>(),
            GtkPlugin::new(APP_ID).without_adw(),
        )),
        DemoMode::Adw => app.add_plugins((
            GtkInitPlugin,
            default_plugins.build().disable::<WinitPlugin>(),
            GtkPlugin::new(APP_ID).with_adw(),
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
    mut viewports: GtkViewports,
    window: Single<Entity, With<PrimaryWindow>>,
) {
    // circular base
    commands.spawn((
        Mesh3d(meshes.add(Circle::new(4.0))),
        MeshMaterial3d(materials.add(Color::WHITE)),
        Transform::from_rotation(Quat::from_rotation_x(-core::f32::consts::FRAC_PI_2)),
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
    let (image, widget_factory) = viewports.create();

    commands.spawn((
        Camera {
            target: RenderTarget::Image(image.into()),
            ..default()
        },
        Camera3d::default(),
        Transform::from_xyz(-2.5, 4.5, 9.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    commands
        .entity(*window)
        .insert(NewWindowContent::from(move || widget_factory.make()));
}

fn rotate_cube(time: Res<Time>, mut query: Query<&mut Transform, With<Rotating>>) {
    for mut transform in &mut query {
        transform.rotate_x(0.9 * time.delta_secs());
        transform.rotate_y(0.7 * time.delta_secs());
    }
}
