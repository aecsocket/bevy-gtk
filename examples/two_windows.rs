use {
    bevy::{prelude::*, winit::WinitPlugin},
    bevy_gtk::{GtkInitPlugin, GtkPlugin, GtkViewports, GtkWindowContent},
};

const APP_ID: &str = "io.github.aecsocket.BevyGtk";

fn main() -> AppExit {
    App::new()
        .add_plugins((
            GtkInitPlugin,
            DefaultPlugins
                .build()
                .disable::<WinitPlugin>()
                .set(WindowPlugin {
                    primary_window: None,
                    ..default()
                }),
            GtkPlugin::new(APP_ID),
        ))
        .add_systems(Startup, (setup_scene, setup_windows))
        .run()
}

fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
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
    ));
    // light
    commands.spawn((
        PointLight {
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(4.0, 8.0, 4.0),
    ));
}

fn setup_windows(mut viewports: GtkViewports, mut commands: Commands) {
    let (viewport_a, viewport_widget_a) = viewports.create();
    commands.spawn((
        Camera3d::default(),
        viewport_a,
        Transform::from_xyz(-2.5, 4.5, 9.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Window::default(),
        GtkWindowContent::from(move || viewport_widget_a.make()),
    ));

    let (viewport_b, viewport_widget_b) = viewports.create();
    commands.spawn((
        Camera3d::default(),
        viewport_b,
        Transform::from_xyz(0.5, 4.5, 2.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Window::default(),
        GtkWindowContent::from(move || viewport_widget_b.make()),
    ));
}
