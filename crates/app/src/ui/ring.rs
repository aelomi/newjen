use gpui::{Hsla, IntoElement, PathBuilder, Pixels, Point, Styled as _, canvas, point, px};

/// A circular gauge: a full track and, from the top, a clockwise arc of `fraction` of it.
pub fn ring(
    fraction: f32,
    size: Pixels,
    width: Pixels,
    color: Hsla,
    track: Hsla,
) -> impl IntoElement {
    let fraction = fraction.clamp(0.0, 1.0);
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let radius = (bounds.size.width.min(bounds.size.height) - width) / 2.0;
            let centre = bounds.center();
            let top = point(centre.x, centre.y - radius);
            let bottom = point(centre.x, centre.y + radius);
            let radii = point(radius, radius);
            let mut stroke = |color: Hsla, build: &dyn Fn(&mut PathBuilder)| {
                let mut path = PathBuilder::stroke(width);
                build(&mut path);
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            stroke(track, &|path| {
                path.move_to(top);
                path.arc_to(radii, px(0.), false, true, bottom);
                path.arc_to(radii, px(0.), false, true, top);
            });
            if fraction >= 0.9995 {
                stroke(color, &|path| {
                    path.move_to(top);
                    path.arc_to(radii, px(0.), false, true, bottom);
                    path.arc_to(radii, px(0.), false, true, top);
                });
            } else if fraction > 0.004 {
                stroke(color, &|path| {
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
