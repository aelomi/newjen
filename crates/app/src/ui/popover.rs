use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, BoxShadow, Div, Hsla, IntoElement, Styled as _,
    hsla, px,
};
use gpui_kit::component::animation::ease_out_cubic;
use gpui_kit::component::{ActiveTheme as _, ThemeStyled as _};

const ENTER: Duration = Duration::from_millis(150);
const RISE: f32 = 4.;

/// The hairline ring and soft shadow of a Kit popover, at `strength` of their ink.
fn surface_shadow(ring: Hsla, strength: f32) -> Vec<BoxShadow> {
    let ink = hsla(0., 0., 0., 0.1 * strength);
    vec![
        BoxShadow::new(px(0.), px(0.), ring.alpha(ring.a * strength))
            .blur_radius(px(0.))
            .spread_radius(px(1.)),
        BoxShadow::new(px(0.), px(4.), ink)
            .blur_radius(px(3.))
            .spread_radius(px(-1.)),
        BoxShadow::new(px(0.), px(2.), ink)
            .blur_radius(px(2.))
            .spread_radius(px(-2.)),
    ]
}

/// A popover's surface around `body`. Opened with the pointer it fades in over 150 ms while
/// rising 4 px; opened from the keyboard it is there at once. Reduce Motion is GPUI's: the
/// animation adopts its last frame.
///
/// The shadow ramps with the cube of the fade, as Kit's own dropdowns do, so it never shows
/// through the glass while that is still see-through.
pub fn popover_surface(id: &'static str, body: Div, pointer: bool, cx: &App) -> AnyElement {
    let surface = body.relative().popover_style(cx);
    if !pointer {
        return surface.into_any_element();
    }
    let ring = cx.theme().foreground.alpha(0.1);
    surface
        .with_animation(
            id,
            Animation::new(ENTER).with_easing(ease_out_cubic),
            move |surface, t| {
                surface
                    .top(px(RISE * (t - 1.)))
                    .opacity(t)
                    .shadow(surface_shadow(ring, t * t * t))
            },
        )
        .into_any_element()
}
