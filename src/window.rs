use {
    crate::GtkApplication,
    bevy_app::prelude::*,
    bevy_ecs::prelude::*,
    bevy_platform::collections::{HashMap, hash_map::Entry},
    bevy_window::{
        ClosingWindow, Window, WindowCloseRequested, WindowClosed, WindowClosing, WindowCreated,
        WindowMode,
    },
    core::mem,
    gtk::prelude::*,
    log::info,
};

pub(super) fn plugin(app: &mut App) {
    app.add_systems(
        Last,
        (
            create_bevy_to_gtk,
            despawn,
            sync_new_content,
            sync_window_config,
            sync_gtk_to_bevy,
        )
            .chain(),
    );
}

#[derive(Debug)]
pub struct GtkWindows {
    use_adw: bool,
    entity_to_proxy: HashMap<Entity, WindowProxy>,
}

impl GtkWindows {
    #[must_use]
    pub(crate) fn new(use_adw: bool) -> Self {
        Self {
            use_adw,
            entity_to_proxy: HashMap::new(),
        }
    }

    #[must_use]
    pub fn use_adw(&self) -> bool {
        self.use_adw
    }

    #[must_use]
    pub fn entity_to_proxy(&self) -> &HashMap<Entity, WindowProxy> {
        &self.entity_to_proxy
    }
}

#[derive(Debug)]
pub struct WindowProxy {
    pub gtk: gtk::ApplicationWindow,
    content: gtk::Widget,
    cache: Option<Window>,
    rx_close_request: async_channel::Receiver<()>,
}

impl WindowProxy {
    pub fn set_content(&mut self, content: impl IsA<gtk::Widget>) {
        let new: gtk::Widget = content.into();
        let old = mem::replace(&mut self.content, new.clone());
        replace_content(&old, Some(&new));
    }
}

#[derive(Component)]
pub struct GtkWindowContent(pub Option<Box<dyn MakeWidget>>);

impl<T: MakeWidget> From<T> for GtkWindowContent {
    fn from(value: T) -> Self {
        Self(Some(Box::new(value)))
    }
}

pub trait MakeWidget: Send + Sync + 'static {
    fn make(self: Box<Self>) -> gtk::Widget;
}

impl<W, F> MakeWidget for F
where
    W: IsA<gtk::Widget>,
    F: FnOnce() -> W + Send + Sync + 'static,
{
    fn make(self: Box<Self>) -> gtk::Widget {
        (self)().into()
    }
}

pub(super) fn create_bevy_to_gtk(
    new_windows: Query<(Entity, &mut Window), Added<Window>>,
    mut gtk_windows: NonSendMut<GtkWindows>,
    gtk_app: NonSend<GtkApplication>,
    mut window_created_events: EventWriter<WindowCreated>,
) {
    let gtk_windows = &mut *gtk_windows;
    for (entity, bevy_window) in &new_windows {
        let Entry::Vacant(entry) = gtk_windows.entity_to_proxy.entry(entity) else {
            continue;
        };

        info!(
            "Creating new window {} ({})",
            bevy_window.title.as_str(),
            entity
        );

        let gtk_window = if_adw!(
            gtk_windows.use_adw,
            adw::ApplicationWindow::new(&**gtk_app).upcast::<gtk::ApplicationWindow>(),
            gtk::ApplicationWindow::new(&**gtk_app),
        );

        // I think it's fine to drop some close requests if it gets spammed?
        let (tx_close_request, rx_close_request) = async_channel::bounded(8);
        gtk_window.connect_close_request(move |_| {
            _ = tx_close_request.try_send(());
            glib::Propagation::Stop
        });

        let mut proxy = WindowProxy {
            gtk: gtk_window,
            content: gtk::Label::new(None).upcast(),
            cache: None,
            rx_close_request,
        };
        sync_one(gtk_windows.use_adw, bevy_window, &mut proxy);
        proxy.gtk.present();

        entry.insert(proxy);
        window_created_events.write(WindowCreated { window: entity });
    }
}

pub fn sync_new_content(
    mut commands: Commands,
    mut changed_windows: Query<(Entity, Option<&mut GtkWindowContent>), Changed<GtkWindowContent>>,
    mut gtk_windows: NonSendMut<GtkWindows>,
) {
    for (entity, mut new_window_content) in &mut changed_windows {
        let gtk_windows = &mut *gtk_windows;
        let Some(proxy) = gtk_windows.entity_to_proxy.get_mut(&entity) else {
            continue;
        };

        if let Some(new_window_content) = &mut new_window_content {
            if let Some(make_content) = new_window_content.0.take() {
                proxy.set_content(make_content.make());
            }
            commands.entity(entity).remove::<GtkWindowContent>();
        }
    }
}

pub fn sync_window_config(
    mut changed_windows: Query<(Entity, &Window), Changed<Window>>,
    mut gtk_windows: NonSendMut<GtkWindows>,
) {
    for (entity, bevy_window) in &mut changed_windows {
        let gtk_windows = &mut *gtk_windows;
        let Some(proxy) = gtk_windows.entity_to_proxy.get_mut(&entity) else {
            continue;
        };

        sync_one(gtk_windows.use_adw, bevy_window, proxy);
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "small numbers; truncation is fine"
)]
fn sync_one(use_adw: bool, new: &Window, proxy: &mut WindowProxy) {
    let cache = proxy.cache.as_ref();
    let gtk_window = &proxy.gtk;

    if cache.is_none_or(|c| c.mode != new.mode) {
        match new.mode {
            WindowMode::Windowed => gtk_window.set_fullscreened(false),
            WindowMode::BorderlessFullscreen(_) => gtk_window.fullscreen(),
            WindowMode::Fullscreen(_, _) => {}
        }
    }

    if cache.is_none_or(|c| c.title != new.title) {
        gtk_window.set_title(Some(&new.title));
    }

    // `set_default_width/height` MUST be called before `set_width/height_request`,
    // or the window size will be wrong on startup
    if cache.is_none_or(|c| c.resolution != new.resolution) {
        gtk_window.set_default_width(new.resolution.width() as i32);
        gtk_window.set_default_height(new.resolution.height() as i32);
    }

    if cache.is_none_or(|c| c.resize_constraints != new.resize_constraints) {
        gtk_window.set_width_request(new.resize_constraints.min_width as i32);
        gtk_window.set_height_request(new.resize_constraints.min_height as i32);
    }

    if cache.is_none_or(|c| c.resizable != new.resizable) {
        gtk_window.set_resizable(new.resizable);
    }

    // TODO: IME

    #[cfg(feature = "adwaita")]
    if cache.is_none_or(|c| c.window_theme != new.window_theme) {
        use bevy_window::WindowTheme;

        adw::StyleManager::default().set_color_scheme(match new.window_theme {
            None => adw::ColorScheme::Default,
            Some(WindowTheme::Light) => adw::ColorScheme::ForceLight,
            Some(WindowTheme::Dark) => adw::ColorScheme::ForceDark,
        });
    }

    let rebuild_widgets = cache.is_none_or(|c| {
        c.titlebar_shown != new.titlebar_shown
            || c.titlebar_transparent != new.titlebar_transparent
            || c.titlebar_show_title != new.titlebar_show_title
            || c.titlebar_show_buttons != new.titlebar_show_buttons
    });
    if rebuild_widgets {
        if_adw!(
            use_adw,
            if let Some(adw_window) = proxy.gtk.downcast_ref::<adw::ApplicationWindow>() {
                use adw::prelude::*;

                let content_root = adw_content_root(new, &proxy.content);
                adw_window.set_content(Some(&content_root));
            },
            proxy.gtk.set_child(Some(&proxy.content)),
        );
    }

    proxy.cache = Some(new.clone());
}

fn replace_content(old: &gtk::Widget, new: Option<&gtk::Widget>) {
    let parent = match (old.parent(), new) {
        (Some(parent), _) => parent,
        (None, None) => return,
        (None, Some(_)) => panic!("if replacing the content, the old content must have a parent"),
    };

    #[cfg(feature = "adwaita")]
    {
        use adw::prelude::*;

        if let Some(parent) = parent.downcast_ref::<adw::ApplicationWindow>() {
            parent.set_content(new);
            return;
        }
        if let Some(parent) = parent.downcast_ref::<adw::ToolbarView>() {
            parent.set_content(new);
            return;
        }
    }
    if let Some(parent) = parent.downcast_ref::<gtk::ApplicationWindow>() {
        parent.set_child(new);
        return;
    }
    if let Some(parent) = parent.downcast_ref::<gtk::Overlay>() {
        parent.set_child(new);
        return;
    }

    unreachable!("invalid parent widget {parent:?}");
}

#[cfg(feature = "adwaita")]
fn adw_content_root(config: &Window, content: &gtk::Widget) -> gtk::Widget {
    // ensure `proxy.content` has no parent before we add it to a new parent
    replace_content(content, None);

    if config.titlebar_shown {
        if config.titlebar_transparent {
            if config.titlebar_show_buttons {
                // same margin as `adw::HeaderBar`
                const MARGIN: i32 = 6;

                let header_box = gtk::Box::builder()
                    .margin_start(MARGIN)
                    .margin_end(MARGIN)
                    .margin_top(MARGIN)
                    .margin_bottom(MARGIN)
                    .valign(gtk::Align::Start)
                    .build();
                header_box.append(&gtk::WindowControls::new(gtk::PackType::Start));
                header_box.append(&gtk::Box::builder().hexpand(true).build());
                header_box.append(&gtk::WindowControls::new(gtk::PackType::End));

                let overlay = gtk::Overlay::new();
                overlay.add_overlay(&header_box);
                overlay.set_child(Some(content));
                overlay.upcast()
            } else {
                content.clone().upcast()
            }
        } else {
            let header = adw::HeaderBar::new();
            if !config.titlebar_show_title {
                header.set_title_widget(Some(&gtk::Label::new(None)));
            }
            if !config.titlebar_show_buttons {
                header.set_show_start_title_buttons(false);
                header.set_show_end_title_buttons(false);
            }

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&header);
            toolbar.set_content(Some(content));
            toolbar.upcast()
        }
    } else {
        content.clone().upcast()
    }
}

pub fn sync_gtk_to_bevy(
    gtk_windows: NonSend<GtkWindows>,
    mut close_requested: EventWriter<WindowCloseRequested>,
) {
    for (entity, proxy) in &gtk_windows.entity_to_proxy {
        if let Ok(()) | Err(async_channel::TryRecvError::Closed) = proxy.rx_close_request.try_recv()
        {
            close_requested.write(WindowCloseRequested { window: *entity });
        }
    }
}

pub fn despawn(
    closing: Query<Entity, With<ClosingWindow>>,
    mut closing_events: EventWriter<WindowClosing>,
    mut closed: RemovedComponents<Window>,
    mut closed_events: EventWriter<WindowClosed>,
    mut gtk_windows: NonSendMut<GtkWindows>,
) {
    for window in &closing {
        closing_events.write(WindowClosing { window });
    }
    for window in closed.read() {
        info!("Closing window {window}");
        if let Some(proxy) = gtk_windows.entity_to_proxy.remove(&window) {
            proxy.gtk.destroy();
        }
        closed_events.write(WindowClosed { window });
    }
}
