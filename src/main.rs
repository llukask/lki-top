//! lki-top: a graphical system monitor in the spirit of btop.

mod boxes;
mod gpu;
mod graph;
mod history;
mod keys;
mod monitor;
mod procs;
mod rates;
mod sample;
mod settings;
mod theme;
mod ui;

use gpui::{
    App, AppContext as _, Application, Bounds, Focusable as _, WindowBounds, WindowOptions, px,
    size,
};

use monitor::Monitor;

fn main() {
    Application::new().run(|cx: &mut App| {
        keys::bind_keys(cx);
        cx.on_window_closed(|cx| cx.quit()).detach();

        let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("lki-top".into()),
                    ..Default::default()
                }),
                app_id: Some("lki-top".into()),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(Monitor::new);
                window.focus(&view.focus_handle(cx));
                view
            },
        );
        if let Err(err) = opened {
            eprintln!("lki-top: could not open a window: {err:#}");
            cx.quit();
        }
    });
}
