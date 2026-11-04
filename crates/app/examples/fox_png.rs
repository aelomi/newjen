#[path = "../src/ui/fox.rs"]
mod fox;

mod theme {
    pub struct Material {
        pub glass_muted: gpui::Hsla,
    }

    pub fn material(_: &gpui::App) -> Material {
        Material {
            glass_muted: gpui::rgb(0xd4d4d4).into(),
        }
    }
}

use fox::FoxGame;
use fox::model::{FOX_X, Phase};
use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    VisualTestAppContext, Window, div, px, size,
};
use std::path::PathBuf;

const FRAME: f32 = 1.0 / 60.0;

struct Stage {
    game: Entity<FoxGame>,
}

impl Render for Stage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(255.))
            .h(px(176.))
            .pt(px(12.))
            .bg(gpui::rgb(0x0e0e12))
            .child(self.game.clone())
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend(data);
    out.extend(&body);
    out.extend(crc32(&body).to_be_bytes());
}

fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    for row in rgba.chunks(width as usize * 4) {
        raw.push(0);
        raw.extend(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(i + 1 == blocks.len()));
        zlib.extend((block.len() as u16).to_le_bytes());
        zlib.extend((!(block.len() as u16)).to_le_bytes());
        zlib.extend(*block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    zlib.extend(((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut header = Vec::new();
    header.extend(width.to_be_bytes());
    header.extend(height.to_be_bytes());
    header.extend([8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn tap_when_close(game: &mut FoxGame) {
    let runner = &mut game.runner;
    let centre = FOX_X + 4.0;
    let next = runner
        .obstacles()
        .iter()
        .find(|o| o.x + o.kind.size().0 / 2.0 > centre);
    if let Some(next) = next {
        let (width, height) = next.kind.size();
        let tall = height > 11.0;
        let lead = if tall { 0.27 } else { 0.18 };
        if !runner.airborne() && next.x + width / 2.0 - centre <= runner.speed() * lead {
            runner.press();
            if !tall {
                runner.release();
            }
        }
    }
    if runner.vy() < 120.0 {
        runner.release();
    }
}

fn cruise(game: &mut FoxGame, reduce_motion: bool, until: impl Fn(&FoxGame) -> bool) {
    game.runner.press();
    for _ in 0..60 * 400 {
        if until(game) {
            return;
        }
        tap_when_close(game);
        game.advance(FRAME, reduce_motion);
        if game.runner.phase() == Phase::Crashed {
            game.runner.press();
        }
    }
    panic!("never reached the scene");
}

fn invincible(game: &mut FoxGame, reduce_motion: bool, until: impl Fn(&FoxGame) -> bool) {
    game.runner.press();
    while !until(game) {
        game.advance(FRAME, reduce_motion);
        game.runner.clear_obstacles();
    }
}

fn rest_with_best(game: &mut FoxGame, best: u32, reduce_motion: bool) {
    invincible(game, reduce_motion, |game| game.runner.score() >= best);
    game.runner.pause();
    game.runner.rest();
    game.advance(2.0, reduce_motion);
}

fn hover(game: &mut FoxGame, reduce_motion: bool) {
    game.fx.set_hover(true);
    for _ in 0..70 {
        game.advance(FRAME, reduce_motion);
    }
}

fn mid_jump(game: &FoxGame) -> bool {
    game.runner.score() >= 270 && game.runner.height() > 24.0 && game.runner.vy() > 0.0
}

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("the folder for the PNGs"));
    std::fs::create_dir_all(&dir).unwrap();
    let mut cx = VisualTestAppContext::new(gpui_platform::current_platform(false));
    let mut snap = |name: &str, reduce_motion: bool, prepare: &dyn Fn(&mut FoxGame)| {
        cx.update(|cx| cx.set_reduce_motion(reduce_motion));
        let window = cx
            .open_offscreen_window(size(px(255.), px(176.)), |window, cx| {
                let game = cx.new(|cx| FoxGame::new(window, cx));
                game.update(cx, |game, cx| {
                    game.set_title("Nothing needs you", cx);
                    game.set_shown(true, cx);
                    game.advance(1.0, reduce_motion);
                    prepare(game);
                    game.frame = None;
                });
                cx.new(|_| Stage { game })
            })
            .unwrap();
        cx.run_until_parked();
        let image = cx.capture_screenshot(window.into()).unwrap();
        std::fs::write(
            dir.join(format!("{name}.png")),
            png(image.width(), image.height(), image.as_raw()),
        )
        .unwrap();
        println!("{name}: {}x{}", image.width(), image.height());
    };
    snap("idle-new", false, &|_| {});
    snap("idle-realm-4", false, &|game| {
        rest_with_best(game, 460, false)
    });
    snap("idle-realm-9", false, &|game| {
        rest_with_best(game, 2600, false)
    });
    snap("idle-realm-9-hover", false, &|game| {
        rest_with_best(game, 2600, false);
        hover(game, false);
    });
    snap("idle-realm-9-hover-reduced", true, &|game| {
        rest_with_best(game, 2600, true);
        hover(game, true);
    });
    snap("run-realm-2", false, &|game| cruise(game, false, mid_jump));
    snap("run-realm-2-reduced", true, &|game| {
        cruise(game, true, mid_jump)
    });
    snap("breakthrough", false, &|game| {
        invincible(game, false, |game| game.runner.realm() >= 3);
        for _ in 0..9 {
            game.advance(FRAME, false);
            game.runner.clear_obstacles();
        }
    });
    snap("crash", false, &|game| {
        game.runner.press();
        while game.runner.phase() == Phase::Running {
            game.advance(FRAME, false);
        }
        for _ in 0..14 {
            game.advance(FRAME, false);
        }
    });
    snap("paused", false, &|game| {
        cruise(game, false, |game| game.runner.score() >= 40);
        game.runner.pause();
        game.advance(1.0, false);
    });
}
