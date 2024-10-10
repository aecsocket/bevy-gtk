use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

use adw::prelude::*;
use adw::{gio, glib, gtk};
use atomicbox::AtomicOptionBox;

use crate::render::{self, FrameInfo};
use crate::{AdwaitaHeaderBar, AdwaitaWindowConfig};

#[derive(Debug)]
pub struct WindowOpen {
    pub config: AdwaitaWindowConfig,
    pub recv_command: flume::Receiver<WindowCommand>,
    pub render_target_width: Arc<AtomicI32>,
    pub render_target_height: Arc<AtomicI32>,
    pub scale_factor: Arc<AtomicI32>,
    pub shared_next_frame: Arc<AtomicOptionBox<FrameInfo>>,
    pub closed: Arc<AtomicBool>,
}

#[derive(Debug)]
pub enum WindowCommand {}

pub fn main_thread_loop(recv_window_open: flume::Receiver<WindowOpen>) {
    // when we `init`, this thread is marked as the main thread
    adw::init().expect("failed to initialize Adwaita");
    let main_context = glib::MainContext::default();
    let mut windows = Vec::new();

    loop {
        match recv_window_open.try_recv() {
            Ok(request) => {
                let window_state = WindowState::new(request);
                windows.push(window_state);
            }
            Err(flume::TryRecvError::Disconnected) => return,
            Err(flume::TryRecvError::Empty) => {}
        }

        windows.retain_mut(|window| window.poll().is_ok());

        if main_context.pending() {
            main_context.iteration(true);
        }
    }
}

#[derive(Debug)]
struct WindowState {
    window: adw::Window,
    render_target: gtk::Picture,
    shared_next_frame: Arc<AtomicOptionBox<FrameInfo>>,
    recv_command: flume::Receiver<WindowCommand>,
    closed: Arc<AtomicBool>,
    should_poll: Arc<AtomicBool>,
    current_frame: Option<FrameInfo>,
}

impl WindowState {
    fn new(request: WindowOpen) -> Self {
        let WindowOpen {
            config,
            recv_command,
            render_target_width,
            render_target_height,
            scale_factor,
            shared_next_frame,
            closed,
        } = request;

        let render_target = gtk::Picture::new();
        let render_target_container = {
            let graphics_offload = gtk::GraphicsOffload::builder()
                .black_background(true)
                .child(&render_target)
                .hexpand(true)
                .vexpand(true)
                .build();

            // Use a trick to detect when the actual render target
            // is resized, and send this new frame size to the app.
            // https://stackoverflow.com/questions/70488187/get-calculated-size-of-widget-in-gtk-4-0
            // +-----------------------+
            // |          WL           |  WL: width_listener  (height 0)
            // |-----------------------|  HL: height_listener (width 0)
            // |   |                   |
            // | H |     graphics      |
            // | L |     offload       |
            // |   |                   |
            // +-----------------------+

            let width_listener = gtk::DrawingArea::builder().hexpand(true).build();
            width_listener.set_draw_func({
                let render_target_width = render_target_width.clone();
                move |area, _, width, _| {
                    render_target_width.store(width, Ordering::SeqCst);
                }
            });

            let height_listener = gtk::DrawingArea::builder().vexpand(true).build();
            height_listener.set_draw_func({
                let render_target_height = render_target_height.clone();
                move |area, _, _, height| {
                    render_target_height.store(height, Ordering::SeqCst);
                }
            });

            let frame_content_h = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            frame_content_h.append(&height_listener);
            frame_content_h.append(&graphics_offload);

            let frame_content_v = gtk::Box::new(gtk::Orientation::Vertical, 0);
            frame_content_v.append(&width_listener);
            frame_content_v.append(&frame_content_h);

            frame_content_v
        };

        let content: gtk::Widget = match config.header_bar {
            AdwaitaHeaderBar::Full => {
                let header_bar = adw::HeaderBar::new();

                let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
                content.append(&header_bar);
                content.append(&render_target_container);
                content.upcast()
            }
            AdwaitaHeaderBar::OverContent => {
                // this margin makes the window controls looks exactly like in an `adw::HeaderBar`
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

                let content = gtk::Overlay::new();
                content.set_child(Some(&render_target_container));
                content.add_overlay(&window_controls);
                content.upcast()
            }
            AdwaitaHeaderBar::None => render_target_container.upcast(),
        };

        let window = adw::Window::builder()
            .handle_menubar_accel(false)
            .default_width(assert_i32(config.width, "window request width"))
            .default_height(assert_i32(config.height, "window request height"))
            .title(config.title)
            .resizable(config.resizable)
            .maximized(config.maximized)
            .fullscreened(config.fullscreen)
            .content(&content)
            .build();

        window.connect_close_request({
            let closed = closed.clone();
            move |_| {
                closed.store(true, Ordering::SeqCst);
                glib::Propagation::Proceed
            }
        });

        window.connect_scale_factor_notify({
            let scale_factor = scale_factor.clone();
            move |window| {
                scale_factor.store(window.scale_factor(), Ordering::SeqCst);
            }
        });

        let should_poll = Arc::new(AtomicBool::new(false));
        window.add_tick_callback({
            let should_poll = should_poll.clone();
            move |_, _| {
                should_poll.store(true, Ordering::SeqCst);
                glib::ControlFlow::Continue
            }
        });

        window.present();

        Self {
            window,
            render_target,
            shared_next_frame,
            recv_command,
            closed,
            should_poll,
            current_frame: None,
        }
    }

    fn poll(&mut self) -> Result<(), ()> {
        let Ok(true) =
            self.should_poll
                .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
        else {
            return Ok(());
        };

        if self.closed.load(Ordering::SeqCst) {
            return Err(());
        }

        if let Some(frame_info) = self.shared_next_frame.take(Ordering::SeqCst) {
            self.current_frame = Some(*frame_info);
        }

        if let Some(frame_info) = self.current_frame.as_ref() {
            let frame = render::create_dmabuf_texture(&frame_info.dmabuf);
            self.render_target.set_paintable(Some(&frame));
            self.render_target.queue_draw();
        } else {
            tracing::info!("Don't have a frame yet...");
        }

        loop {
            let command = match self.recv_command.try_recv() {
                Ok(command) => command,
                Err(flume::TryRecvError::Disconnected) => return Err(()),
                Err(flume::TryRecvError::Empty) => break,
            };
        }

        Ok(())
    }
}

fn assert_i32(n: u32, value_name: &str) -> i32 {
    i32::try_from(n).unwrap_or_else(|_| panic!("{value_name} must fit into an `i32`, was {n}"))
}
