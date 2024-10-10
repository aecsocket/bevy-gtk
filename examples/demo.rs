use bevy::{prelude::*, render::settings::WgpuSettings};
use bevy_mod_adwaita::{AdwaitaHeaderBar, AdwaitaWindowConfig, AdwaitaWindowPlugin};

fn main() -> AppExit {
    App::new()
        .add_plugins((
            DefaultPlugins
                .set(AdwaitaWindowPlugin::window_plugin())
                .set(AdwaitaWindowPlugin::render_plugin(WgpuSettings::default())),
            AdwaitaWindowPlugin {
                primary_window_config: Some(AdwaitaWindowConfig {
                    header_bar: AdwaitaHeaderBar::OverContent,
                    ..default()
                }),
                ..default()
            },
        ))
        .add_systems(PreStartup, setup_scene)
        .add_systems(Update, rotate_cube)
        .run()
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
