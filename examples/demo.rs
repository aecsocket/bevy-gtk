use bevy::prelude::*;
use bevy_mod_adwaita::{AdwaitaPlugin, AdwaitaWindow, PrimaryAdwaitaWindow};

fn main() -> AppExit {
    App::new()
        .add_plugins((
            DefaultPlugins
                .set(AdwaitaPlugin::window_plugin())
                .set(AdwaitaPlugin::render_plugin()),
            AdwaitaPlugin,
        ))
        .add_systems(Startup, (spawn_adwaita_window, setup_scene).chain())
        .add_systems(Update, rotate_cube)
        .run()
}

fn spawn_adwaita_window(mut commands: Commands) {
    commands
        .spawn(PrimaryAdwaitaWindow)
        .add(AdwaitaWindow::open("io.github.aecsocket.bevy_mod_adwaita"));
}

#[derive(Debug, Component)]
struct Rotated;

/// set up a simple 3D scene
fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // circular base
    commands.spawn(PbrBundle {
        mesh: meshes.add(Circle::new(4.0)),
        material: materials.add(Color::WHITE),
        transform: Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
        ..default()
    });
    // cube
    commands.spawn((
        PbrBundle {
            mesh: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
            material: materials.add(Color::srgb_u8(124, 144, 255)),
            transform: Transform::from_xyz(0.0, 0.8, 0.0),
            ..default()
        },
        Rotated,
    ));
    // light
    commands.spawn(PointLightBundle {
        point_light: PointLight {
            shadows_enabled: true,
            ..default()
        },
        transform: Transform::from_xyz(4.0, 8.0, 4.0),
        ..default()
    });
    // camera
    commands.spawn(Camera3dBundle {
        transform: Transform::from_xyz(-2.5, 4.5, 9.0).looking_at(Vec3::ZERO, Vec3::Y),
        ..default()
    });
}

fn rotate_cube(time: Res<Time>, mut query: Query<&mut Transform, With<Rotated>>) {
    for mut transform in &mut query {
        transform.rotate_x(0.9 * time.delta_seconds());
        transform.rotate_y(0.7 * time.delta_seconds());
    }
}
