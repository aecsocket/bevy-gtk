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
    rx_close_request: async_channel::Receiver<()>,
}

impl WindowProxy {
    pub fn set_content(&mut self, content: impl IsA<gtk::Widget>) {
        let new: gtk::Widget = content.into();
        let old = mem::replace(&mut self.content, new.clone());
        replace_content(&old, Some(&new));
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

pub fn create_bevy_to_gtk(
    mut commands: Commands,
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
            rx_close_request,
        };
        sync_one(gtk_windows.use_adw, bevy_window, &mut proxy);
        proxy.gtk.present();

        entry.insert(proxy);
        window_created_events.write(WindowCreated { window: entity });
    }
}

pub fn sync_bevy_to_gtk(
    changed_windows: Query<(Entity, &Window), Changed<Window>>,
    mut gtk_windows: NonSendMut<GtkWindows>,
) {
    for (entity, bevy_window) in &changed_windows {
        let gtk_windows = &mut *gtk_windows;
        let Some(proxy) = gtk_windows.entity_to_proxy.get_mut(&entity) else {
            continue;
        };

        sync_one(gtk_windows.use_adw, &bevy_window, proxy);
    }
}

fn sync_one(use_adw: bool, bevy_window: &Window, proxy: &mut WindowProxy) {
    proxy.gtk.set_title(Some(&bevy_window.title));

    if_adw!(
        use_adw,
        if let Some(window) = proxy.gtk.downcast_ref::<adw::ApplicationWindow>() {
            use adw::prelude::*;

            // ensure `proxy.content` has no parent before we add it to a new parent
            replace_content(&proxy.content, None);
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
                        overlay.set_child(Some(&proxy.content));
                        window.set_content(Some(&overlay));
                    } else {
                        window.set_content(Some(&proxy.content));
                    }
                } else {
                    let header = adw::HeaderBar::new();
                    if !bevy_window.titlebar_show_title {
                        // TODO generic empty widget
                        header.set_title_widget(Some(&gtk::Box::new(
                            gtk::Orientation::Horizontal,
                            0,
                        )));
                    }
                    if !bevy_window.titlebar_show_buttons {
                        header.set_show_start_title_buttons(false);
                        header.set_show_end_title_buttons(false);
                    }

                    let toolbar = adw::ToolbarView::new();
                    toolbar.add_top_bar(&header);
                    toolbar.set_content(Some(&proxy.content));
                    window.set_content(Some(&toolbar));
                }
            } else {
                window.set_content(Some(&proxy.content));
            };
        },
        proxy.gtk.set_child(Some(&proxy.content)),
    );
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
