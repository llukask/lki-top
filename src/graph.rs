//! Drawing primitives: history graphs and meters, painted straight onto
//! a `canvas`.
//!
//! Colors encode level, as in btop: a graph is green near its baseline
//! and red near the top no matter how tall the current peak is. gpui
//! gradients have two stops and span each shape's own bounds, so every
//! shape here covers a single [`GradientPiece`] and gets its stops
//! placed at the pixel positions of that piece's ends. That reproduces
//! the three-stop [`Gradient`] exactly.

use gpui::{
    Background, Bounds, Hsla, IntoElement, PathBuilder, Pixels, Styled, Window, canvas, fill,
    linear_color_stop, linear_gradient, point, px, size,
};

use crate::theme::{Gradient, GradientPiece};

/// Opacity of a graph's area fill. The line on top is fully opaque.
const FILL_OPACITY: f32 = 0.35;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grow {
    Up,
    Down,
}

/// How far apart samples sit horizontally. Either way the newest is on
/// the right edge, and a short history fills in from the right.
#[derive(Debug, Clone, Copy)]
enum Spacing {
    /// A fixed distance; a wider graph shows more history.
    Pixels(f32),
    /// The width divided among this many samples, whatever it is.
    Window(usize),
}

#[derive(Debug, Clone, Copy)]
pub struct GraphStyle {
    grow: Grow,
    spacing: Spacing,
    line_width: f32,
    /// Color of the 25/50/75% guide lines, if any.
    grid: Option<Hsla>,
    /// Fill behind the whole graph, if any.
    backdrop: Option<Hsla>,
}

impl GraphStyle {
    pub fn large(grow: Grow, grid: Hsla) -> Self {
        Self {
            grow,
            spacing: Spacing::Pixels(4.0),
            line_width: 1.5,
            grid: Some(grid),
            backdrop: None,
        }
    }

    pub fn sparkline(backdrop: Hsla) -> Self {
        Self {
            grow: Grow::Up,
            spacing: Spacing::Pixels(2.0),
            line_width: 1.0,
            grid: None,
            backdrop: Some(backdrop),
        }
    }

    /// Show the last `samples` samples across the full width, however
    /// wide the graph is. For graphs scaled to their own peak, where the
    /// caller has to know which samples are on screen.
    pub fn fitted(self, samples: usize) -> Self {
        Self {
            spacing: Spacing::Window(samples),
            ..self
        }
    }
}

/// History graph of `values` in 0..=1, newest on the right: a
/// translucent area with a line along its top. Only as many samples as
/// fit the width get drawn, so pass the whole history.
pub fn graph(values: Vec<f32>, gradient: Gradient, style: GraphStyle) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window: &mut Window, _| {
            paint_graph(window, bounds, &values, gradient, style);
        },
    )
    .size_full()
}

fn paint_graph(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    values: &[f32],
    gradient: Gradient,
    style: GraphStyle,
) {
    let plot = Plot::new(bounds, style.grow);
    if let Some(backdrop) = style.backdrop {
        window.paint_quad(fill(bounds, backdrop).corner_radii(px(2.0)));
    }
    if let Some(grid) = style.grid {
        for level in [0.25, 0.5, 0.75] {
            let y = plot.y(level).round();
            window.paint_quad(fill(
                Bounds::new(point(px(plot.left), px(y)), size(px(plot.width), px(1.0))),
                grid,
            ));
        }
    }

    // The newest sample sits on the right edge. `+ 1` because n samples
    // span n - 1 steps.
    let (fits, step) = match style.spacing {
        Spacing::Pixels(step) => ((plot.width / step).floor() as usize + 1, step),
        Spacing::Window(samples) => (
            samples,
            plot.width / samples.saturating_sub(1).max(1) as f32,
        ),
    };
    let shown = &values[values.len().saturating_sub(fits)..];
    if shown.len() < 2 {
        return;
    }
    let right = plot.left + plot.width;
    let first_x = right - (shown.len() - 1) as f32 * step;
    let samples: Vec<(f32, f32)> = shown
        .iter()
        .enumerate()
        .map(|(index, value)| (first_x + index as f32 * step, value.clamp(0.0, 1.0)))
        .collect();
    let pieces = gradient.pieces();
    let samples = split_at_levels(&samples, &[pieces[0].to]);

    // Area: per piece, the part of the area between the piece's bottom
    // level and the curve, with the curve clamped into the piece.
    for piece in pieces {
        let clamped: Vec<(f32, f32)> = samples
            .iter()
            .map(|&(x, level)| (x, level.clamp(piece.from, piece.to)))
            .collect();
        let peak = clamped
            .iter()
            .map(|&(_, level)| level)
            .fold(piece.from, f32::max);
        if peak <= piece.from {
            continue;
        }
        let mut path = PathBuilder::fill();
        path.move_to(point(px(clamped[0].0), px(plot.y(piece.from))));
        for &(x, level) in &clamped {
            path.line_to(point(px(x), px(plot.y(level))));
        }
        path.line_to(point(px(right), px(plot.y(piece.from))));
        path.close();
        if let Ok(path) = path.build() {
            let shading = level_gradient(piece, piece.from, peak, plot, 0.0, FILL_OPACITY);
            window.paint_path(path, shading);
        }
    }

    // Line: one stroke per run of samples inside a single piece. After
    // `split_at_levels` every pair of neighbours shares a piece.
    let piece_of = |a: f32, b: f32| {
        let middle = (a + b) / 2.0;
        pieces
            .into_iter()
            .find(|piece| middle <= piece.to)
            .unwrap_or(pieces[1])
    };
    let mut start = 0;
    while start + 1 < samples.len() {
        let piece = piece_of(samples[start].1, samples[start + 1].1);
        let mut end = start + 1;
        while end + 1 < samples.len()
            && piece_of(samples[end].1, samples[end + 1].1).from == piece.from
        {
            end += 1;
        }
        let run = &samples[start..=end];
        let (low, high) = run
            .iter()
            .fold((f32::MAX, f32::MIN), |(low, high), &(_, level)| {
                (low.min(level), high.max(level))
            });
        let mut path = PathBuilder::stroke(px(style.line_width));
        path.move_to(point(px(run[0].0), px(plot.y(run[0].1))));
        for &(x, level) in &run[1..] {
            path.line_to(point(px(x), px(plot.y(level))));
        }
        if let Ok(path) = path.build() {
            window.paint_path(
                path,
                level_gradient(piece, low, high, plot, style.line_width, 1.0),
            );
        }
        start = end;
    }
}

/// Maps levels (0 at the baseline, 1 at the far edge) to pixels.
#[derive(Debug, Clone, Copy)]
struct Plot {
    left: f32,
    width: f32,
    top: f32,
    height: f32,
    grow: Grow,
}

impl Plot {
    fn new(bounds: Bounds<Pixels>, grow: Grow) -> Self {
        Self {
            left: f32::from(bounds.origin.x),
            width: f32::from(bounds.size.width),
            top: f32::from(bounds.origin.y),
            height: f32::from(bounds.size.height),
            grow,
        }
    }

    fn y(&self, level: f32) -> f32 {
        match self.grow {
            Grow::Up => self.top + self.height * (1.0 - level),
            Grow::Down => self.top + self.height * level,
        }
    }

    /// gpui gradient angle pointing from the baseline to the far edge.
    fn angle(&self) -> f32 {
        match self.grow {
            Grow::Up => 0.0,
            Grow::Down => 180.0,
        }
    }
}

/// Shading for a shape whose vertices span levels `low..=high` within
/// `piece`, grown by `pad` pixels on each side (a stroke's half width
/// on both ends, so `pad` is the full stroke width).
fn level_gradient(
    piece: GradientPiece,
    low: f32,
    high: f32,
    plot: Plot,
    pad: f32,
    opacity: f32,
) -> Background {
    let fade = |color: Hsla| Hsla {
        a: opacity,
        ..color
    };
    let Some((from_stop, to_stop)) = level_stops(piece, low, high, plot.height, pad) else {
        return fade(piece.from_color).into();
    };
    linear_gradient(
        plot.angle(),
        linear_color_stop(fade(piece.from_color), from_stop),
        linear_color_stop(fade(piece.to_color), to_stop),
    )
}

/// Where `piece`'s two ends fall along a shape spanning `low..=high`
/// plus `pad` pixels, as gradient percentages (0 at the baseline side
/// of the shape, 1 at the far side). They usually lie outside 0..=1:
/// the shape shows only the part of the piece it covers. `None` for a
/// shape with no extent along the gradient.
fn level_stops(
    piece: GradientPiece,
    low: f32,
    high: f32,
    height: f32,
    pad: f32,
) -> Option<(f32, f32)> {
    let span = (high - low) * height + pad;
    if span <= f32::EPSILON {
        return None;
    }
    let stop = |level: f32| ((level - low) * height + pad / 2.0) / span;
    Some((stop(piece.from), stop(piece.to)))
}

/// `samples` as `(x, level)`, with a point inserted wherever the line
/// between two neighbours crosses one of `levels`. Clamping a line
/// segment into a level range is only exact at the vertices, so the
/// crossings have to be vertices.
fn split_at_levels(samples: &[(f32, f32)], levels: &[f32]) -> Vec<(f32, f32)> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for pair in samples.windows(2) {
        let [(x0, l0), (x1, l1)] = [pair[0], pair[1]];
        out.push((x0, l0));
        let mut crossings: Vec<f32> = levels
            .iter()
            .filter(|&&level| (l0 < level && level < l1) || (l1 < level && level < l0))
            .map(|&level| (level - l0) / (l1 - l0))
            .collect();
        crossings.sort_by(f32::total_cmp);
        out.extend(
            crossings
                .into_iter()
                .map(|t| (x0 + t * (x1 - x0), l0 + t * (l1 - l0))),
        );
    }
    if let Some(&last) = samples.last() {
        out.push(last);
    }
    out
}

/// Height of a meter's bar. The rest of the element's height is space.
const METER_HEIGHT: f32 = 6.0;

/// A rounded bar filled left to right up to `fraction`. The fill takes
/// the gradient color of each position, so a full meter runs green to
/// red and a short one stays green.
pub fn meter(fraction: f32, gradient: Gradient, track: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window: &mut Window, _| {
            let left = f32::from(bounds.origin.x);
            let width = f32::from(bounds.size.width);
            let height = METER_HEIGHT.min(f32::from(bounds.size.height));
            let top = f32::from(bounds.origin.y) + (f32::from(bounds.size.height) - height) / 2.0;
            let radius = px(height / 2.0);
            window.paint_quad(
                fill(
                    Bounds::new(point(px(left), px(top)), size(px(width), px(height))),
                    track,
                )
                .corner_radii(radius),
            );

            let fraction = fraction.clamp(0.0, 1.0);
            // Neighbouring pieces meet on a whole pixel. Two anti-aliased
            // edges meeting mid-pixel would leave a faint seam.
            let x_at = |level: f32| (left + level * width).round();
            for piece in gradient.pieces() {
                if fraction <= piece.from {
                    break;
                }
                let end = fraction.min(piece.to);
                let (x0, x1) = (x_at(piece.from), x_at(end).max(x_at(piece.from) + 1.0));
                let span = x1 - x0;
                let stop = |level: f32| (x_at(level) - x0) / span;
                let shading = linear_gradient(
                    90.0,
                    linear_color_stop(piece.from_color, stop(piece.from)),
                    linear_color_stop(piece.to_color, stop(piece.to)),
                );
                let mut corners = gpui::Corners::default();
                if piece.from == 0.0 {
                    corners.top_left = radius;
                    corners.bottom_left = radius;
                }
                if end == fraction {
                    corners.top_right = radius;
                    corners.bottom_right = radius;
                }
                window.paint_quad(
                    fill(
                        Bounds::new(point(px(x0), px(top)), size(px(span), px(height))),
                        shading,
                    )
                    .corner_radii(corners),
                );
            }
        },
    )
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_inserts_a_vertex_at_each_crossing() {
        let split = split_at_levels(&[(0.0, 0.2), (10.0, 0.8), (20.0, 0.6)], &[0.5]);
        assert_eq!(split, [(0.0, 0.2), (5.0, 0.5), (10.0, 0.8), (20.0, 0.6)]);
    }

    #[test]
    fn split_leaves_lines_that_touch_a_level_alone() {
        let samples = [(0.0, 0.5), (10.0, 0.9)];
        assert_eq!(split_at_levels(&samples, &[0.5]), samples);
    }

    /// The green channel the shader would produce at `level` on a shape
    /// spanning `low..=high` plus `pad`, in the shader's own arithmetic:
    /// position along the shape, rescaled between the stops, clamped,
    /// then mixed.
    fn shaded(piece: GradientPiece, low: f32, high: f32, height: f32, pad: f32, level: f32) -> f32 {
        let (from_stop, to_stop) =
            level_stops(piece, low, high, height, pad).expect("non-empty span");
        let t = ((level - low) * height + pad / 2.0) / ((high - low) * height + pad);
        let t = ((t - from_stop) / (to_stop - from_stop)).clamp(0.0, 1.0);
        let (from, to) = (
            gpui::Rgba::from(piece.from_color),
            gpui::Rgba::from(piece.to_color),
        );
        from.g + (to.g - from.g) * t
    }

    #[test]
    fn level_stops_reproduce_gradient_at_absolute_levels() {
        let gradient = Gradient::from_hex(0x00_00_00, 0x00_80_00, 0x00_ff_00);
        let piece = gradient.pieces()[1];
        // A stroke whose vertices span 0.6..=0.9 of a 200 px graph.
        for level in [0.6, 0.75, 0.9] {
            let expected = gpui::Rgba::from(gradient.at(level)).g;
            let actual = shaded(piece, 0.6, 0.9, 200.0, 2.0, level);
            assert!(
                (expected - actual).abs() < 1e-3,
                "{level}: {expected} vs {actual}"
            );
        }
    }

    #[test]
    fn flat_shapes_have_no_stops() {
        let piece = Gradient::from_hex(0, 0, 0).pieces()[0];
        assert_eq!(level_stops(piece, 0.3, 0.3, 200.0, 0.0), None);
    }
}
