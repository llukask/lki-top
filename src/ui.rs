//! Pieces every box uses: the bordered box itself, the character grid
//! that columns are measured in, and number formatting.

use std::time::Duration;

use gpui::{Context, Div, Hsla, SharedString, Task, div, prelude::*, px};

use crate::history::History;
use crate::theme::Theme;

pub const ROW_HEIGHT: f32 = 18.0;
/// Approximate advance of one monospace character at `text_xs`. Column
/// widths are given in characters so they line up with btop's grid.
const CH: f32 = 7.4;

pub fn chars(count: f32) -> gpui::Pixels {
    px(count * CH)
}

/// A rounded box with its title cut into the top border, the way btop
/// draws `┌ cpu ┐`.
pub fn panel(
    theme: &Theme,
    title: impl Into<SharedString>,
    accent: Hsla,
    right: Option<SharedString>,
) -> Div {
    // gpui paints a div's border after its children, so a label inside
    // the bordered div can't cover the line. The border lives on a
    // separate frame instead, and the labels come after it as siblings,
    // which paints them on top. The frame starts half a title line down
    // so the line runs through the titles' middle.
    let title_height = ROW_HEIGHT;
    let frame = div()
        .absolute()
        .top(px(title_height / 2.0))
        .bottom_0()
        .left_0()
        .right_0()
        .border_1()
        .border_color(accent)
        .rounded_md();
    let label = |text: SharedString| {
        div()
            .absolute()
            .top_0()
            .h(px(title_height))
            .flex()
            .items_center()
            .px_1()
            .bg(theme.background)
            .text_color(theme.title)
            .child(text)
    };
    div()
        .relative()
        .flex()
        .flex_col()
        .min_h_0()
        .min_w_0()
        .pt(px(title_height + 2.0))
        .px_2()
        .pb_1()
        .child(frame)
        .child(label(title.into()).left(px(10.0)))
        .when_some(right, |panel, right| {
            panel.child(label(right).right(px(10.0)).text_color(theme.muted))
        })
}

/// How long a [`Notice`] stays up.
const NOTICE_DURATION: Duration = Duration::from_secs(5);

/// A message for the user that clears itself after a few seconds. A new
/// notice replaces the old one and drops its timer with it, so an old
/// timer never clears a newer message.
pub struct Notice {
    pub text: SharedString,
    _expiry: Task<()>,
}

impl Notice {
    /// `slot` is the view's field the notice lives in, for the timer to
    /// clear.
    pub fn new<V: 'static>(
        text: impl Into<SharedString>,
        cx: &mut Context<V>,
        slot: fn(&mut V) -> &mut Option<Notice>,
    ) -> Self {
        let expiry = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE_DURATION).await;
            // Fails only when the view is gone, along with its notice.
            this.update(cx, |view, cx| {
                *slot(view) = None;
                cx.notify();
            })
            .ok();
        });
        Self {
            text: text.into(),
            _expiry: expiry,
        }
    }
}

pub fn share(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        0.0
    } else {
        part as f32 / whole as f32
    }
}

/// The last `window` samples divided by their peak, which never drops
/// below `floor`, and that peak.
pub fn scaled(history: &History, window: usize, floor: f32) -> (Vec<f32>, f32) {
    let peak = history.tail(window).fold(floor, f32::max);
    (
        history.tail(window).map(|value| value / peak).collect(),
        peak,
    )
}

pub fn bytes(count: u64) -> String {
    humansize::format_size(count, humansize::BINARY.decimal_places(1))
}

/// Sizes in btop's terse style, e.g. `1.5G` or `16G`, for tight rows.
pub fn short_bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 && unit > 0 {
        format!("{value:.1}{}", UNITS[unit])
    } else {
        format!("{value:.0}{}", UNITS[unit])
    }
}

/// `1d 4h 21m`, or `4h 21m` under a day.
pub fn duration(secs: u64) -> String {
    let (days, hours, minutes) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60);
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else {
        format!("{hours}h {minutes}m")
    }
}
