#[cfg(feature = "adwaita")]
use bevy_window::{WindowTheme, WindowThemeChanged};
use {
    crate::GtkWindows,
    bevy_app::prelude::*,
    bevy_ecs::prelude::*,
    bevy_window::{WindowEvent, WindowScaleFactorChanged, prelude::*},
    glib::clone,
    gtk::prelude::*,
};

pub(super) fn plugin(app: &mut App) {
    app.add_systems(
        Last,
        setup_event_forwarding.after(super::create_gtk_windows),
    )
    .add_systems(PreUpdate, forward_events);
}

#[derive(Debug, Component)]
struct RxWindowEvents(async_channel::Receiver<WindowEvent>);

fn setup_event_forwarding(
    new_windows: Query<Entity, Added<Window>>,
    gtk_windows: NonSend<GtkWindows>,
    mut commands: Commands,
) {
    for window in &new_windows {
        let proxy = gtk_windows.get(window).expect(
            "we just added `Window` to this entity; there should be a corresponding `GtkWindows` \
             entry",
        );

        let (tx_event, rx_event) = async_channel::bounded(4);
        commands.entity(window).insert(RxWindowEvents(rx_event));

        let send_event = |tx_event: &async_channel::Sender<WindowEvent>, event| {
            glib::spawn_future(clone!(
                #[strong]
                tx_event,
                async move {
                    _ = tx_event.send(event).await;
                }
            ));
        };

        proxy.gtk_window.connect_scale_factor_notify(clone!(
            #[strong]
            tx_event,
            move |gtk_window| {
                if let Some(scale_factor) = gtk_window
                    .native()
                    .and_then(|native| native.surface())
                    .map(|surface| surface.scale())
                {
                    send_event(
                        &tx_event,
                        WindowScaleFactorChanged {
                            window,
                            scale_factor,
                        }
                        .into(),
                    );
                }
            }
        ));

        adw::StyleManager::default().connect_dark_notify(clone!(
            #[strong]
            tx_event,
            move |style_manager| {
                let theme = if style_manager.is_dark() {
                    WindowTheme::Dark
                } else {
                    WindowTheme::Light
                };
                send_event(&tx_event, WindowThemeChanged { window, theme }.into());
            }
        ));
    }
}

fn forward_events(windows: Query<&RxWindowEvents>, mut window_events: EventWriter<WindowEvent>) {
    let mut to_send = Vec::new();
    for rx_event in &windows {
        while let Ok(event) = rx_event.0.try_recv() {
            to_send.push(event);
        }
    }
    window_events.write_batch(to_send);
}
