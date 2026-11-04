use gpui::{Hsla, IntoElement, PathBuilder, Pixels, Point, Styled as _, canvas, point, px};

/// A short mark across the ring at `at` of the way round it, from the top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tick {
    pub at: f32,
    pub color: Hsla,
    pub width: Pixels,
    pub reach: Pixels,
}

/// A circular gauge: a full track and, from the top, a clockwise arc of `fraction` of it.
pub fn ring(
    fraction: f32,
    size: Pixels,
    width: Pixels,
    color: Hsla,
    track: Hsla,
) -> impl IntoElement {
    ring_with_tick(fraction, size, width, px(0.), color, track, None)
}

/// A ring drawn `inset` inside its box, with an optional tick across it.
pub fn ring_with_tick(
    fraction: f32,
    size: Pixels,
    width: Pixels,
    inset: Pixels,
    color: Hsla,
    track: Hsla,
    tick: Option<Tick>,
) -> impl IntoElement {
    let fraction = fraction.clamp(0.0, 1.0);
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let radius = (bounds.size.width.min(bounds.size.height) - width) / 2.0 - inset;
            let centre = bounds.center();
            let top = point(centre.x, centre.y - radius);
            let bottom = point(centre.x, centre.y + radius);
            let radii = point(radius, radius);
            let mut stroke = |width: Pixels, color: Hsla, build: &dyn Fn(&mut PathBuilder)| {
                let mut path = PathBuilder::stroke(width);
                build(&mut path);
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            stroke(width, track, &|path| {
                path.move_to(top);
                path.arc_to(radii, px(0.), false, true, bottom);
                path.arc_to(radii, px(0.), false, true, top);
            });
            if fraction >= 0.9995 {
                stroke(width, color, &|path| {
                    path.move_to(top);
                    path.arc_to(radii, px(0.), false, true, bottom);
                    path.arc_to(radii, px(0.), false, true, top);
                });
            } else if fraction > 0.004 {
                stroke(width, color, &|path| {
                    path.move_to(top);
                    path.arc_to(
                        radii,
                        px(0.),
                        fraction > 0.5,
                        true,
                        arc_end(centre, radius, fraction),
                    );
                });
            }
            if let Some(tick) = tick {
                let at = tick.at.clamp(0.0, 1.0);
                stroke(tick.width, tick.color, &|path| {
                    path.move_to(arc_end(centre, radius - tick.reach, at));
                    path.line_to(arc_end(centre, radius + tick.reach, at));
                });
            }
        },
    )
    .size(size)
}

fn arc_end(centre: Point<Pixels>, radius: Pixels, fraction: f32) -> Point<Pixels> {
    let angle = std::f32::consts::TAU * fraction - std::f32::consts::FRAC_PI_2;
    point(
        centre.x + radius * angle.cos(),
        centre.y + radius * angle.sin(),
    )
}
