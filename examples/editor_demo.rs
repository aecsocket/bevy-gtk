use {
    adw::prelude::*,
    bevy::{camera::RenderTarget, prelude::*, window::PrimaryWindow, winit::WinitPlugin},
    bevy_gtk::{GtkInitPlugin, GtkPlugin, NewWindowContent, render::GtkViewports},
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
                    primary_window: Some(Window {
                        titlebar_transparent: true,
                        resize_constraints: WindowResizeConstraints {
                            // default size as given by
                            // <https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.7/class.Window.html>
                            min_width: 360.0,
                            min_height: 200.0,
                            ..default()
                        },
                        ..default()
                    }),
                    ..default()
                }),
            GtkPlugin::new(APP_ID),
        ))
        .add_systems(Startup, (setup_scene, setup_cameras))
        .add_systems(Update, rotate_cube)
        .run()
}

#[derive(Debug, Component)]
struct Rotating;

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
}

fn rotate_cube(time: Res<Time>, mut query: Query<&mut Transform, With<Rotating>>) {
    for mut transform in &mut query {
        transform.rotate_x(0.9 * time.delta_secs());
        transform.rotate_y(0.7 * time.delta_secs());
    }
}

fn setup_cameras(
    mut commands: Commands,
    window: Single<Entity, With<PrimaryWindow>>,
    mut viewports: GtkViewports,
) {
    let (left_image, left_viewport_factory) = viewports.create();
    let (right_image, right_viewport_factory) = viewports.create();

    commands.spawn((
        Camera {
            target: RenderTarget::Image(left_image.into()),
            ..default()
        },
        Camera3d::default(),
        Transform::from_xyz(-2.5, 4.5, 9.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Camera {
            target: RenderTarget::Image(right_image.into()),
            ..default()
        },
        Camera3d::default(),
        Transform::from_xyz(0.5, 4.5, 2.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    commands
        .entity(*window)
        .insert(NewWindowContent::from(move || {
            let editor = editor::EditorDemo::new();
            editor
                .bevy_content_left()
                .set_child(Some(&left_viewport_factory.make()));
            editor
                .bevy_content_right()
                .set_child(Some(&right_viewport_factory.make()));
            editor
        }));
}

mod editor {
    use adw::subclass::prelude::*;

    mod imp {
        use adw::subclass::prelude::*;

        #[derive(Debug, Default, gtk::CompositeTemplate)]
        #[template(file = "examples/editor_demo.blp")]
        pub struct EditorDemo {
            #[template_child]
            pub bevy_content_left: TemplateChild<adw::Bin>,
            #[template_child]
            pub bevy_content_right: TemplateChild<adw::Bin>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for EditorDemo {
            const NAME: &str = "EditorDemo";
            type Type = super::EditorDemo;
            type ParentType = adw::Bin;

            fn class_init(klass: &mut Self::Class) {
                klass.bind_template();
            }

            fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
                obj.init_template();
            }
        }

        impl ObjectImpl for EditorDemo {}
        impl WidgetImpl for EditorDemo {}
        impl BinImpl for EditorDemo {}
    }

    glib::wrapper! {
        // <https://github.com/gtk-rs/gtk4-rs/issues/2118>
        pub struct EditorDemo(ObjectSubclass<imp::EditorDemo>)
            @extends gtk::Widget, adw::Bin,
            @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
    }

    impl EditorDemo {
        #[must_use]
        #[expect(clippy::new_without_default, reason = "gtk-rs convention")]
        pub fn new() -> Self {
            glib::Object::new()
        }

        #[must_use]
        pub fn bevy_content_left(&self) -> adw::Bin {
            self.imp().bevy_content_left.get()
        }

        #[must_use]
        pub fn bevy_content_right(&self) -> adw::Bin {
            self.imp().bevy_content_right.get()
        }
    }
}
