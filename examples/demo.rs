use {
    bevy::{
        prelude::*,
        render::{
            camera::{
                ManualTextureView, ManualTextureViewHandle, ManualTextureViews, RenderTarget,
            },
            render_resource::{
                Extent3d, TextureAspect, TextureDescriptor, TextureDimension, TextureFormat,
                TextureUsages, TextureView, TextureViewDescriptor,
            },
            renderer::RenderDevice,
        },
        winit::WinitPlugin,
    },
    bevy_gtk::{GtkPlugin, NewWindowContent},
    bevy_window::PrimaryWindow,
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
            default_plugins.build().disable::<WinitPlugin>(),
            GtkPlugin::new(APP_ID).without_adw(),
        )),
        DemoMode::Adw => app.add_plugins((
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
    // let size = Extent3d {
    //     width: 512,
    //     height: 512,
    //     depth_or_array_layers: 1,
    // };
    // let texture_format = TextureFormat::bevy_default();
    // let texture = render_device.create_texture(&TextureDescriptor {
    //     label: None,
    //     size,
    //     mip_level_count: 1,
    //     sample_count: 1,
    //     dimension: TextureDimension::D2,
    //     format: texture_format,
    //     usage: TextureUsages::RENDER_ATTACHMENT,
    //     view_formats: &[],
    // });
    // let texture_view = texture.create_view(&TextureViewDescriptor {
    //     label: None,
    //     format: None,
    //     dimension: None,
    //     usage: None,
    //     aspect: TextureAspect::All,
    //     base_mip_level: 1,
    //     mip_level_count: Some(1),
    //     base_array_layer: 0,
    //     array_layer_count: None,
    // });
    // let manual_texture_view = ManualTextureViewHandle(0);
    // manual_texture_views.insert(
    //     manual_texture_view,
    //     ManualTextureView {
    //         texture_view,
    //         size: (size.width, size.height).into(),
    //         format: texture_format,
    //     },
    // );

    commands.spawn((
        Camera {
            // target: RenderTarget::TextureView(manual_texture_view),
            ..default()
        },
        Camera3d::default(),
        Transform::from_xyz(-2.5, 4.5, 9.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    commands
        .entity(*window)
        .insert(NewWindowContent::from(|| gtk4::Label::new(Some("foobar"))));
}

fn rotate_cube(time: Res<Time>, mut query: Query<&mut Transform, With<Rotating>>) {
    for mut transform in &mut query {
        transform.rotate_x(0.9 * time.delta_secs());
        transform.rotate_y(0.7 * time.delta_secs());
    }
}
