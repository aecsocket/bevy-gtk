use {
    crate::GtkApplication,
    bevy_ecs::prelude::*,
    bevy_platform::collections::{HashMap, hash_map::Entry},
    bevy_window::{
        ClosingWindow, Window, WindowCloseRequested, WindowClosed, WindowClosing, WindowCreated,
    },
    gtk::prelude::*,
    log::info,
    std::mem,
};

#[derive(Debug)]
pub struct GtkWindows {
    use_adw: bool,
    entity_to_proxy: HashMap<Entity, WindowProxy>,
}

#[derive(Debug)]
pub struct WindowProxy {
    pub gtk: gtk::ApplicationWindow,
    content: gtk::Widget,
    cache_titlebar_shown: bool,
    cache_titlebar_transparent: bool,
    cache_titlebar_show_title: bool,
    cache_titlebar_show_buttons: bool,
    rx_close_request: async_channel::Receiver<()>,
}

impl WindowProxy {
    pub fn set_content(&mut self, content: impl IsA<gtk::Widget>) {
        let new: gtk::Widget = content.into();
        let old = mem::replace(&mut self.content, new.clone());
        replace_content(&old, Some(&new));
    }
}

impl GtkWindows {
    pub(crate) fn new(use_adw: bool) -> Self {
        Self {
            use_adw,
            entity_to_proxy: HashMap::new(),
        }
    }

    pub fn use_adw(&self) -> bool {
        self.use_adw
    }

    pub fn entity_to_proxy(&self) -> &HashMap<Entity, WindowProxy> {
        &self.entity_to_proxy
    }
}

#[derive(Component)]
pub struct NewWindowContent(pub Option<Box<dyn MakeWindowContent>>);

impl<T: MakeWindowContent> From<T> for NewWindowContent {
    fn from(value: T) -> Self {
        Self(Some(Box::new(value)))
    }
}

pub trait MakeWindowContent: Send + Sync + 'static {
    fn make(self: Box<Self>) -> gtk::Widget;
}

impl<W, F> MakeWindowContent for F
where
    W: IsA<gtk::Widget>,
    F: FnOnce() -> W + Send + Sync + 'static,
{
    fn make(self: Box<Self>) -> gtk::Widget {
        (self)().into()
    }
}

pub fn create_bevy_to_gtk(
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
            // negate cache values to force a widget tree rebuild
            cache_titlebar_shown: !bevy_window.titlebar_shown,
            cache_titlebar_transparent: !bevy_window.titlebar_transparent,
            cache_titlebar_show_title: !bevy_window.titlebar_show_title,
            cache_titlebar_show_buttons: !bevy_window.titlebar_show_buttons,
            rx_close_request,
        };
        sync_one(gtk_windows.use_adw, bevy_window, &mut proxy);
        proxy.gtk.present();

        entry.insert(proxy);
        window_created_events.write(WindowCreated { window: entity });
    }
}

pub fn sync_bevy_to_gtk(
    mut commands: Commands,
    mut changed_windows: Query<(Entity, &Window, Option<&mut NewWindowContent>), Changed<Window>>,
    mut gtk_windows: NonSendMut<GtkWindows>,
) {
    for (entity, bevy_window, mut new_window_content) in &mut changed_windows {
        let gtk_windows = &mut *gtk_windows;
        let Some(proxy) = gtk_windows.entity_to_proxy.get_mut(&entity) else {
            continue;
        };

        if let Some(new_window_content) = &mut new_window_content {
            if let Some(make_content) = new_window_content.0.take() {
                proxy.set_content(make_content.make());
            }
            commands.entity(entity).remove::<NewWindowContent>();
        }

        sync_one(gtk_windows.use_adw, &bevy_window, proxy);
    }
}

fn sync_one(use_adw: bool, bevy_window: &Window, proxy: &mut WindowProxy) {
    fn cmp_ex(dst: &mut bool, src: bool) -> bool {
        if *dst == src {
            false
        } else {
            *dst = src;
            true
        }
    }

    proxy.gtk.set_title(Some(&bevy_window.title));

    let rebuild_widgets = cmp_ex(&mut proxy.cache_titlebar_shown, bevy_window.titlebar_shown)
        || cmp_ex(
            &mut proxy.cache_titlebar_transparent,
            bevy_window.titlebar_transparent,
        )
        || cmp_ex(
            &mut proxy.cache_titlebar_show_title,
            bevy_window.titlebar_show_title,
        )
        || cmp_ex(
            &mut proxy.cache_titlebar_show_buttons,
            bevy_window.titlebar_show_buttons,
        );
    if rebuild_widgets {
        if_adw!(
            use_adw,
            if let Some(adw_window) = proxy.gtk.downcast_ref::<adw::ApplicationWindow>() {
                use adw::prelude::*;

                let content_root = adw_content_root(bevy_window, &proxy.content);
                adw_window.set_content(Some(&content_root));
            },
            proxy.gtk.set_child(Some(&proxy.content)),
        );
    }
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
fn adw_content_root(bevy_window: &Window, content: &gtk::Widget) -> gtk::Widget {
    // ensure `proxy.content` has no parent before we add it to a new parent
    replace_content(content, None);
    if bevy_window.titlebar_shown {
        if bevy_window.titlebar_transparent {
            if bevy_window.titlebar_show_buttons {
                // same margin as `adw::HeaderBar`
                const MARGIN: i32 = 6;

                let window_controls = gtk::WindowControls::builder()
                    .side(gtk::PackType::End)
                    .halign(gtk::Align::End)
                    .valign(gtk::Align::Start)
                    .margin_start(MARGIN)
                    .margin_end(MARGIN)
                    .margin_top(MARGIN)
                    .margin_bottom(MARGIN)
                    .build();

                let overlay = gtk::Overlay::new();
                overlay.add_overlay(&window_controls);
                overlay.set_child(Some(content));
                overlay.upcast()
            } else {
                content.clone().upcast()
            }
        } else {
            let header = adw::HeaderBar::new();
            if !bevy_window.titlebar_show_title {
                header.set_title_widget(Some(&gtk::Label::new(None)));
            }
            if !bevy_window.titlebar_show_buttons {
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
