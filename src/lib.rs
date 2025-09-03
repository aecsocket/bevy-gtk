extern crate alloc;

macro_rules! if_adw {
    ($with_adw:expr, $without_adw:expr $(,)?) => {{
        #[cfg(feature = "adwaita")]
        {
            $with_adw
        }
        #[cfg(not(feature = "adwaita"))]
        {
            $without_adw
        }
    }};
    ($is_adw:expr, $with_adw:expr, $without_adw:expr $(,)?) => {{
        #[cfg(feature = "adwaita")]
        {
            if $is_adw { $with_adw } else { $without_adw }
        }
        #[cfg(not(feature = "adwaita"))]
        {
            $without_adw
        }
    }};
}

use {
    alloc::rc::Rc,
    bevy_app::{PluginsState, prelude::*},
    core::cell::{Cell, RefCell},
    derive_more::Deref,
    glib::clone,
    gtk::prelude::*,
    log::debug,
};

mod window;
#[cfg(feature = "adwaita")]
pub use adw;
pub use {gdk, gio, gtk, window::*};

#[cfg(feature = "viewport")]
pub mod viewport;
#[cfg(feature = "viewport")]
pub use viewport::*;

/// Initialization plugin for [`GtkPlugin`].
///
/// # Plugin ordering
///
/// - **[`GtkInitPlugin`]**
/// - `DefaultPlugins.build().disable::<WinitPlugin>()`
/// - [`GtkPlugin`]
pub struct GtkInitPlugin;

impl Plugin for GtkInitPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(feature = "viewport")]
        viewport::init_plugin(app);
    }
}

/// Runs the Bevy app inside a [`gtk::Application`], allowing you to create
/// GTK windows which interface with the Bevy app.
///
/// This replaces the [app runner](App::set_runner) and windowing backend, so
/// make sure to disable `WinitPlugin` when adding this plugin.
///
/// # Plugin ordering
///
/// - [`GtkInitPlugin`]
/// - `DefaultPlugins.build().disable::<WinitPlugin>()`
/// - **[`GtkPlugin`]**
#[derive(Default)]
pub struct GtkPlugin {
    /// If the `adwaita` feature is enabled, determines whether [Adwaita](adw)
    /// will be used for creating the application and windows, as opposed to raw
    /// GTK.
    ///
    /// If the `adwaita` feature is not enabled, this has no effect, but is
    /// retained in the API for parity.
    pub use_adw: bool,
    /// ID of the GTK application, passed into [`gtk::Application::new`].
    ///
    /// See [Application ID](https://developer.gnome.org/documentation/tutorials/application-id.html)
    /// for an explanation of what this value should be.
    ///
    /// # Examples
    ///
    /// - `org.gnome.TextEditor`
    /// - `org.bevy.DemoApp`
    pub app_id: Option<String>,
    /// Application flags, passed into [`gtk::Application::new`].
    pub app_flags: gio::ApplicationFlags,
}

impl GtkPlugin {
    /// Creates a new plugin with the given application ID.
    ///
    /// See [`GtkPlugin::app_id`] for a description of what this string should
    /// be.
    #[must_use]
    pub fn new(app_id: impl Into<String>) -> Self {
        Self {
            use_adw: if_adw!(true, false),
            app_id: Some(app_id.into()),
            app_flags: gio::ApplicationFlags::empty(),
        }
    }

    /// Enables [`GtkPlugin::use_adw`].
    #[must_use]
    pub fn with_adw(self) -> Self {
        Self {
            use_adw: true,
            ..self
        }
    }

    /// Disables [`GtkPlugin::use_adw`].
    #[must_use]
    pub fn without_adw(self) -> Self {
        Self {
            use_adw: false,
            ..self
        }
    }
}

/// Stores a reference to the [`gtk::Application`] this app is running under.
///
/// If [`GtkPlugin`] uses Adwaita, this will be an [`adw::Application`].
#[derive(Debug, Clone, Deref)]
pub struct GtkApplication(pub gtk::Application);

impl Plugin for GtkPlugin {
    fn build(&self, app: &mut App) {
        assert!(
            app.is_plugin_added::<GtkInitPlugin>(),
            "add `GtkInitPlugin` before `GtkPlugin`"
        );

        #[cfg(feature = "viewport")]
        viewport::plugin(app);

        let gtk_app = if_adw!(
            self.use_adw,
            adw::Application::new(self.app_id.as_deref(), self.app_flags)
                .upcast::<gtk::Application>(),
            gtk::Application::new(self.app_id.as_deref(), self.app_flags),
        );
        // prevent app closing when there are no windows;
        // this becomes `bevy_window`'s responsibility
        let app_hold = gtk_app.hold();

        let (tx_activated, rx_activated) = oneshot::channel::<()>();
        let tx_activated = RefCell::new(Some(tx_activated));
        gtk_app.connect_activate(move |_| {
            if let Some(tx) = tx_activated.take() {
                _ = tx.send(());
            }
        });

        debug!("Registering GTK app");
        gtk_app
            .register(None::<&gio::Cancellable>)
            .expect("failed to register GTK app");
        debug!("Activating GTK app");
        gtk_app.activate();
        rx_activated
            .recv()
            .expect("channel dropped while activating GTK app");
        debug!("App activated");

        app.add_plugins(window::plugin)
            .insert_non_send_resource(app_hold)
            .insert_non_send_resource(GtkApplication(gtk_app.clone()))
            .insert_non_send_resource(GtkWindows::new(self.use_adw))
            .set_runner(|bevy_app| gtk_runner(bevy_app, gtk_app));
    }
}

fn gtk_runner(mut bevy_app: App, gtk_app: gtk::Application) -> AppExit {
    if bevy_app.plugins_state() == PluginsState::Ready {
        bevy_app.finish();
        bevy_app.cleanup();
    }

    debug!("Starting GTK app");

    let bevy_exit = Rc::new(Cell::new(None::<AppExit>));
    glib::idle_add_local(clone!(
        #[strong]
        bevy_exit,
        move || {
            if let Some(exit) = idle_update(&mut bevy_app) {
                bevy_exit.set(Some(exit));
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        }
    ));

    // don't handle CLI args, since that's Bevy's job
    let gtk_exit = gtk_app.run_with_args::<&str>(&[]);
    debug!("GTK app exited with code {gtk_exit:?}");
    bevy_exit
        .take()
        .unwrap_or_else(|| AppExit::from_code(gtk_exit.get()))
}

fn idle_update(bevy_app: &mut App) -> Option<AppExit> {
    if bevy_app.plugins_state() == PluginsState::Cleaned {
        bevy_app.update();
    }

    bevy_app.should_exit()
}
