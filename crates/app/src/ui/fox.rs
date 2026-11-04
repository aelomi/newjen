#![expect(dead_code, reason = "the sidebar mounts the fox in a later slice")]

mod model {
    pub const WIDTH: f32 = 239.0;
    pub const HEIGHT: f32 = 112.0;
    pub const GROUND: f32 = 98.0;
    pub const FOX_X: f32 = 52.0;

    const JUMP_SPEED: f32 = 460.0;
    const GRAVITY: f32 = 2600.0;
    const GRAVITY_HELD: f32 = 1500.0;
    pub const START_SPEED: f32 = 140.0;
    const MAX_SPEED: f32 = 360.0;
    const ACCELERATION: f32 = 4.0;
    pub const LANDING: f32 = 0.09;
    const SUBSTEP: f32 = 1.0 / 120.0;
    const FIRST_GAP: f32 = 170.0;
    const SPAWN_X: f32 = WIDTH + 8.0;
    const BOLT_STRIKES_AT: f32 = WIDTH * 0.74;
    const FOX_LEFT: f32 = FOX_X - 5.0;
    const FOX_RIGHT: f32 = FOX_X + 13.0;
    const GRACE: f32 = 1.5;
    const STORM_FROM_REALM: usize = 2;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Realm {
        pub name: &'static str,
        pub from: u32,
    }

    pub const REALMS: [Realm; 9] = [
        Realm {
            name: "Qi Condensation",
            from: 0,
        },
        Realm {
            name: "Foundation Establishment",
            from: 100,
        },
        Realm {
            name: "Core Formation",
            from: 250,
        },
        Realm {
            name: "Nascent Soul",
            from: 450,
        },
        Realm {
            name: "Soul Formation",
            from: 700,
        },
        Realm {
            name: "Spirit Severing",
            from: 1000,
        },
        Realm {
            name: "Dao Seeking",
            from: 1400,
        },
        Realm {
            name: "Immortal Ascension",
            from: 1900,
        },
        Realm {
            name: "True Immortal",
            from: 2500,
        },
    ];

    pub const MAX_TAILS: usize = REALMS.len();

    pub fn realm_for(score: u32) -> usize {
        REALMS
            .iter()
            .rposition(|realm| score >= realm.from)
            .unwrap_or(0)
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Rng(u64);

    impl Rng {
        pub fn new(seed: u64) -> Self {
            Self(seed)
        }

        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        pub fn unit(&mut self) -> f32 {
            (self.next() >> 40) as f32 / (1u64 << 24) as f32
        }

        pub fn range(&mut self, low: f32, high: f32) -> f32 {
            low + self.unit() * (high - low)
        }

        pub fn below(&mut self, len: usize) -> usize {
            ((self.unit() * len as f32) as usize).min(len.saturating_sub(1))
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Hazard {
        Rock,
        Rocks,
        Talisman,
        Step,
        Bolt,
    }

    impl Hazard {
        pub const fn size(self) -> (f32, f32) {
            match self {
                Hazard::Rock => (10.0, 8.0),
                Hazard::Rocks => (18.0, 11.0),
                Hazard::Talisman => (8.0, 17.0),
                Hazard::Step => (14.0, 9.0),
                Hazard::Bolt => (7.0, 12.0),
            }
        }
    }

    const CALM: [Hazard; 4] = [Hazard::Rock, Hazard::Rocks, Hazard::Talisman, Hazard::Step];
    const STORMY: [Hazard; 6] = [
        Hazard::Rock,
        Hazard::Rocks,
        Hazard::Talisman,
        Hazard::Step,
        Hazard::Bolt,
        Hazard::Bolt,
    ];

    #[derive(Clone, Debug, PartialEq)]
    pub struct Obstacle {
        pub kind: Hazard,
        pub x: f32,
        pub strike: f32,
        pub zig: [f32; 6],
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Phase {
        Idle,
        Running,
        Paused,
        Crashed,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Runner {
        phase: Phase,
        height: f32,
        vy: f32,
        held: bool,
        speed: f32,
        distance: f32,
        obstacles: Vec<Obstacle>,
        gap: f32,
        landing: f32,
        best: u32,
        epoch: u32,
        rng: Rng,
    }

    impl Runner {
        pub fn new(seed: u64) -> Self {
            Self {
                phase: Phase::Idle,
                height: 0.0,
                vy: 0.0,
                held: false,
                speed: START_SPEED,
                distance: 0.0,
                obstacles: Vec::new(),
                gap: FIRST_GAP,
                landing: 0.0,
                best: 0,
                epoch: 0,
                rng: Rng::new(seed),
            }
        }

        pub fn phase(&self) -> Phase {
            self.phase
        }

        pub fn height(&self) -> f32 {
            self.height
        }

        pub fn vy(&self) -> f32 {
            self.vy
        }

        pub fn speed(&self) -> f32 {
            self.speed
        }

        pub fn distance(&self) -> f32 {
            self.distance
        }

        pub fn landing(&self) -> f32 {
            self.landing
        }

        pub fn best(&self) -> u32 {
            self.best
        }

        pub fn epoch(&self) -> u32 {
            self.epoch
        }

        pub fn obstacles(&self) -> &[Obstacle] {
            &self.obstacles
        }

        pub fn score(&self) -> u32 {
            (self.distance / 10.0) as u32
        }

        pub fn realm(&self) -> usize {
            realm_for(self.score())
        }

        pub fn tail_count(&self) -> usize {
            self.realm() + 1
        }

        pub fn best_tail_count(&self) -> usize {
            realm_for(self.best) + 1
        }

        pub fn airborne(&self) -> bool {
            self.height > 0.0 || self.vy > 0.0
        }

        pub fn press(&mut self) {
            match self.phase {
                Phase::Running => self.jump(),
                Phase::Paused => self.phase = Phase::Running,
                Phase::Idle | Phase::Crashed => {
                    self.reset();
                    self.phase = Phase::Running;
                    self.jump();
                }
            }
        }

        pub fn release(&mut self) {
            self.held = false;
        }

        pub fn pause(&mut self) {
            if self.phase == Phase::Running {
                self.phase = Phase::Paused;
                self.held = false;
            }
        }

        pub fn rest(&mut self) {
            self.best = self.best.max(self.score());
            self.reset();
            self.phase = Phase::Idle;
        }

        pub fn step(&mut self, dt: f32) {
            if self.phase != Phase::Running || dt <= 0.0 {
                return;
            }
            let slices = (dt / SUBSTEP).ceil().max(1.0);
            let slice = dt / slices;
            for _ in 0..slices as usize {
                if !self.tick(slice) {
                    break;
                }
            }
        }

        fn jump(&mut self) {
            if !self.airborne() {
                self.vy = JUMP_SPEED;
                self.held = true;
            }
        }

        fn reset(&mut self) {
            self.height = 0.0;
            self.vy = 0.0;
            self.held = false;
            self.speed = START_SPEED;
            self.distance = 0.0;
            self.obstacles.clear();
            self.gap = FIRST_GAP;
            self.landing = 0.0;
            self.epoch += 1;
        }

        fn tick(&mut self, dt: f32) -> bool {
            self.distance += self.speed * dt;
            self.speed = (self.speed + ACCELERATION * dt).min(MAX_SPEED);
            if self.airborne() {
                let gravity = if self.held && self.vy > 0.0 {
                    GRAVITY_HELD
                } else {
                    GRAVITY
                };
                self.vy -= gravity * dt;
                self.height += self.vy * dt;
                if self.height <= 0.0 {
                    self.height = 0.0;
                    self.vy = 0.0;
                    self.landing = LANDING;
                }
            }
            self.landing = (self.landing - dt).max(0.0);
            let speed = self.speed;
            for obstacle in &mut self.obstacles {
                obstacle.x -= speed * dt;
                if obstacle.kind == Hazard::Bolt
                    && obstacle.strike == 0.0
                    && obstacle.x < BOLT_STRIKES_AT
                {
                    obstacle.strike = 1e-6;
                }
                if obstacle.strike > 0.0 {
                    obstacle.strike += dt;
                }
            }
            self.obstacles
                .retain(|obstacle| obstacle.x + obstacle.kind.size().0 > -6.0);
            if self
                .obstacles
                .last()
                .is_none_or(|last| last.x < WIDTH - self.gap)
            {
                self.spawn();
            }
            let height = self.height;
            let hit = self.obstacles.iter().any(|obstacle| {
                let (width, tall) = obstacle.kind.size();
                obstacle.x + GRACE < FOX_RIGHT
                    && obstacle.x + width - GRACE > FOX_LEFT
                    && height < tall - GRACE
            });
            if hit {
                self.phase = Phase::Crashed;
                self.held = false;
                self.best = self.best.max(self.score());
            }
            !hit
        }

        fn spawn(&mut self) {
            let pool: &[Hazard] = if self.realm() >= STORM_FROM_REALM {
                &STORMY
            } else {
                &CALM
            };
            let kind = pool[self.rng.below(pool.len())];
            let zig = std::array::from_fn(|_| self.rng.range(-3.0, 3.0));
            self.obstacles.push(Obstacle {
                kind,
                x: SPAWN_X,
                strike: 0.0,
                zig,
            });
            self.gap = self.rng.range(130.0, 220.0) + self.speed * 0.35;
        }

        #[cfg(test)]
        pub fn place(&mut self, kind: Hazard, x: f32) {
            self.obstacles.push(Obstacle {
                kind,
                x,
                strike: 0.0,
                zig: [0.0; 6],
            });
        }

        #[cfg(test)]
        pub fn clear_obstacles(&mut self) {
            self.obstacles.clear();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::ui::fox::play::{FRAME, running, tap_when_close};

        fn peak_of_jump(hold: bool) -> f32 {
            let mut runner = running(1);
            runner.press();
            if !hold {
                runner.release();
            }
            let mut peak = 0.0f32;
            while runner.airborne() {
                runner.step(FRAME);
                peak = peak.max(runner.height());
            }
            peak
        }

        #[test]
        fn realm_thresholds_pick_the_last_realm_reached() {
            let table = [
                (0, 0),
                (99, 0),
                (100, 1),
                (249, 1),
                (250, 2),
                (449, 2),
                (450, 3),
                (700, 4),
                (1000, 5),
                (1400, 6),
                (1900, 7),
                (2499, 7),
                (2500, 8),
                (u32::MAX, 8),
            ];
            for (score, realm) in table {
                assert_eq!(realm_for(score), realm, "score {score}");
            }
        }

        #[test]
        fn pressing_in_the_idle_state_starts_a_run_and_jumps() {
            let mut runner = Runner::new(7);
            assert_eq!(runner.phase(), Phase::Idle);
            runner.step(1.0);
            assert_eq!(runner.distance(), 0.0, "idle never advances");
            runner.press();
            assert_eq!(runner.phase(), Phase::Running);
            assert!(runner.airborne());
        }

        #[test]
        fn a_tap_clears_a_small_rock() {
            let mut runner = running(3);
            runner.place(Hazard::Rock, FOX_X + 110.0);
            for _ in 0..240 {
                tap_when_close(&mut runner);
                runner.step(FRAME);
                assert_eq!(runner.phase(), Phase::Running);
                if runner.obstacles().iter().all(|o| o.x < FOX_LEFT - 20.0) {
                    return;
                }
            }
            panic!("the rock never passed");
        }

        #[test]
        fn the_same_rock_crashes_a_fox_that_never_jumps() {
            let mut runner = running(3);
            runner.place(Hazard::Rock, FOX_X + 110.0);
            for _ in 0..240 {
                runner.step(FRAME);
            }
            assert_eq!(runner.phase(), Phase::Crashed);
        }

        #[test]
        fn never_jumping_crashes_whatever_the_seed() {
            for seed in 0..40 {
                let mut runner = Runner::new(seed);
                runner.press();
                runner.release();
                for _ in 0..60 * 8 {
                    runner.step(FRAME);
                }
                assert_eq!(runner.phase(), Phase::Crashed, "seed {seed}");
                assert!(runner.best() < 100);
            }
        }

        #[test]
        fn holding_the_press_jumps_higher_than_a_tap() {
            let tap = peak_of_jump(false);
            let hold = peak_of_jump(true);
            assert!((38.0..44.0).contains(&tap), "tap peaked at {tap}");
            assert!(hold > tap + 20.0, "hold {hold} against tap {tap}");
        }

        #[test]
        fn a_tap_cannot_clear_a_tall_talisman() {
            let mut runner = running(5);
            runner.place(Hazard::Talisman, FOX_X + 110.0);
            for _ in 0..240 {
                let centre = FOX_X + 4.0;
                let near = runner.obstacles()[0].x + 4.0 - centre;
                if !runner.airborne() && near <= runner.speed() * 0.27 {
                    runner.press();
                    runner.release();
                }
                runner.step(FRAME);
            }
            assert_eq!(runner.phase(), Phase::Crashed, "a tap tops out below 17");
        }

        #[test]
        fn each_realm_threshold_adds_one_tail_up_to_nine() {
            let mut runner = running(11);
            let mut seen = vec![(runner.score(), runner.tail_count())];
            for _ in 0..60 * 240 {
                runner.step(FRAME);
                runner.clear_obstacles();
                let tails = runner.tail_count();
                if tails != seen.last().unwrap().1 {
                    seen.push((runner.score(), tails));
                }
            }
            let expected: Vec<(u32, usize)> = REALMS
                .iter()
                .enumerate()
                .map(|(i, realm)| (realm.from, i + 1))
                .skip(1)
                .collect();
            assert_eq!(&seen[1..], &expected[..]);
            assert_eq!(runner.tail_count(), MAX_TAILS);
        }

        #[test]
        fn speed_climbs_to_a_ceiling() {
            let mut runner = running(2);
            for _ in 0..60 * 120 {
                runner.step(FRAME);
                runner.clear_obstacles();
            }
            assert_eq!(runner.speed(), MAX_SPEED);
        }

        #[test]
        fn a_jump_and_the_distance_run_do_not_depend_on_the_frame_rate() {
            let play = |frame: f32| {
                let mut runner = running(9);
                runner.press();
                runner.release();
                let mut peak = 0.0f32;
                for _ in 0..(2.0 / frame).round() as usize {
                    runner.step(frame);
                    runner.clear_obstacles();
                    peak = peak.max(runner.height());
                }
                (peak, runner.distance())
            };
            let (peak_30, run_30) = play(1.0 / 30.0);
            let (peak_120, run_120) = play(1.0 / 120.0);
            assert!(
                (peak_30 - peak_120).abs() < 0.5,
                "{peak_30} against {peak_120}"
            );
            assert!((run_30 - run_120).abs() < 1.0, "{run_30} against {run_120}");
        }

        #[test]
        fn the_same_seed_spawns_the_same_course() {
            let course = |seed: u64| {
                let mut runner = running(seed);
                let mut kinds = Vec::new();
                for _ in 0..60 * 20 {
                    tap_when_close(&mut runner);
                    runner.step(FRAME);
                    if let Some(newest) = runner.obstacles().last()
                        && newest.x > WIDTH
                    {
                        kinds.push(newest.kind);
                    }
                }
                kinds
            };
            assert_eq!(course(21), course(21));
            assert_ne!(course(21), course(22));
        }

        #[test]
        fn pausing_freezes_the_run_and_only_a_press_resumes_it() {
            let mut runner = running(4);
            runner.step(0.5);
            let before = runner.clone();
            runner.pause();
            assert_eq!(runner.phase(), Phase::Paused);
            runner.step(3.0);
            assert_eq!(runner.distance(), before.distance());
            runner.press();
            assert_eq!(runner.phase(), Phase::Running);
            assert!(!runner.airborne(), "resuming does not jump");
        }

        #[test]
        fn pause_leaves_idle_and_crashed_alone() {
            let mut runner = Runner::new(1);
            runner.pause();
            assert_eq!(runner.phase(), Phase::Idle);
            runner.press();
            for _ in 0..60 * 8 {
                runner.step(FRAME);
            }
            assert_eq!(runner.phase(), Phase::Crashed);
            runner.pause();
            assert_eq!(runner.phase(), Phase::Crashed);
        }

        #[test]
        fn a_crash_keeps_the_best_and_a_press_retries_from_zero() {
            let mut runner = running(8);
            for _ in 0..60 * 12 {
                tap_when_close(&mut runner);
                runner.step(FRAME);
                if runner.phase() == Phase::Crashed {
                    break;
                }
            }
            runner.clear_obstacles();
            runner.place(Hazard::Step, FOX_X + 20.0);
            for _ in 0..30 {
                runner.step(FRAME);
            }
            assert_eq!(runner.phase(), Phase::Crashed);
            let best = runner.best();
            assert!(best > 0);
            let epoch = runner.epoch();
            runner.press();
            assert_eq!(runner.phase(), Phase::Running);
            assert_eq!(runner.score(), 0);
            assert_eq!(runner.best(), best);
            assert_eq!(runner.epoch(), epoch + 1);
        }

        #[test]
        fn resting_mid_run_banks_the_score_and_returns_to_idle() {
            let mut runner = running(6);
            for _ in 0..60 * 20 {
                runner.step(FRAME);
                runner.clear_obstacles();
            }
            let score = runner.score();
            assert!(score > 100);
            runner.pause();
            runner.rest();
            assert_eq!(runner.phase(), Phase::Idle);
            assert_eq!(runner.best(), score);
            assert_eq!(runner.score(), 0);
            runner.step(5.0);
            assert_eq!(runner.phase(), Phase::Idle, "never starts by itself");
        }

        #[test]
        fn the_idle_fox_shows_the_realm_of_the_best_run() {
            let mut runner = running(12);
            for _ in 0..60 * 40 {
                runner.step(FRAME);
                runner.clear_obstacles();
            }
            let tails = runner.tail_count();
            assert!(tails >= 3);
            runner.rest();
            assert_eq!(runner.best_tail_count(), tails);
            assert_eq!(runner.tail_count(), 1);
        }
    }
}

mod art {
    use super::model::{
        FOX_X, GROUND, Hazard, LANDING, MAX_TAILS, Obstacle, Phase, REALMS, Rng, Runner,
        START_SPEED, WIDTH,
    };
    use std::f32::consts::{FRAC_PI_2, PI, TAU};

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Pt {
        pub x: f32,
        pub y: f32,
    }

    const fn pt(x: f32, y: f32) -> Pt {
        Pt { x, y }
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Color {
        pub r: u8,
        pub g: u8,
        pub b: u8,
        pub a: f32,
    }

    impl Color {
        const fn hex(rgb: u32) -> Self {
            Self {
                r: (rgb >> 16) as u8,
                g: (rgb >> 8) as u8,
                b: rgb as u8,
                a: 1.0,
            }
        }

        const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Self {
            Self { r, g, b, a }
        }

        const fn alpha(self, a: f32) -> Self {
            Self { a, ..self }
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Shape {
        Fill(Vec<Pt>),
        Line {
            pts: Vec<Pt>,
            width: f32,
            closed: bool,
            round: bool,
        },
        Dot {
            at: Pt,
            radius: f32,
        },
        Rect {
            at: Pt,
            width: f32,
            height: f32,
        },
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Prim {
        pub shape: Shape,
        pub color: Color,
    }

    const FAR_RIDGE: Color = Color::hex(0x15151b);
    const NEAR_RIDGE: Color = Color::hex(0x1a1a21);
    const CLOUD: Color = Color::hex(0x2d2d37);
    const GROUND_LINE: Color = Color::rgba(255, 255, 255, 0.10);
    const ROCK: Color = Color::hex(0x383843);
    const ROCK_VEIN: Color = Color::hex(0x4e4e5a);
    const POLE: Color = Color::hex(0x5c4d3c);
    const PAPER: Color = Color::hex(0xd6b459);
    const INK: Color = Color::hex(0xc0262d);
    const STEP: Color = Color::hex(0x33333d);
    const STEP_TOP: Color = Color::hex(0x44444f);
    const STEP_CRACK: Color = Color::hex(0x25252d);
    const STORM: Color = Color::hex(0x262632);
    const BODY: Color = Color::hex(0xf2f2f4);
    const DARK: Color = Color::hex(0x0b0b10);
    const PINK: Color = Color::hex(0xf4a3b8);
    const MARK: Color = Color::hex(0xe11d48);
    const CHEEK: Color = Color::rgba(244, 163, 184, 0.6);
    const REAR_LEG: Color = Color::hex(0xcfcfd8);
    const BODY_RGB: [f32; 3] = [242.0, 242.0, 244.0];
    const FLASH_RGB: [f32; 3] = [253.0, 230.0, 138.0];
    pub const FIRE: [(f32, Color); 4] = [
        (1.9, Color::rgba(59, 130, 246, 0.20)),
        (1.3, Color::rgba(59, 130, 246, 0.42)),
        (0.85, Color::rgba(125, 180, 255, 0.78)),
        (0.4, Color::rgba(232, 243, 255, 0.96)),
    ];
    const BOLT_GLOW: Color = Color::rgba(139, 92, 246, 0.22);
    const BOLT_CORE: Color = Color::rgba(221, 214, 254, 0.92);
    const BOLT_HALO: Color = Color::rgba(167, 139, 250, 0.35);
    const BOLT_FLASH: Color = Color::rgba(237, 233, 254, 1.0);
    const RING: Color = Color::rgba(252, 211, 77, 0.9);

    const RIDGE_PERIOD: f32 = 420.0;
    const FLAME_SEGMENTS: usize = 12;
    const MAX_PARTICLES: usize = 90;

    fn ease(t: f32) -> f32 {
        1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
    }

    #[derive(Clone, Copy)]
    struct Xf {
        a: f32,
        b: f32,
        c: f32,
        d: f32,
        e: f32,
        f: f32,
    }

    impl Xf {
        const IDENTITY: Xf = Xf {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        };

        fn fox(at: Pt, rotation: f32, sx: f32, sy: f32) -> Self {
            let (sin, cos) = rotation.sin_cos();
            Self {
                a: cos * sx,
                b: sin * sx,
                c: -sin * sy,
                d: cos * sy,
                e: at.x,
                f: at.y,
            }
        }

        fn apply(self, p: Pt) -> Pt {
            pt(
                self.a * p.x + self.c * p.y + self.e,
                self.b * p.x + self.d * p.y + self.f,
            )
        }
    }

    struct Canvas {
        prims: Vec<Prim>,
        xf: Xf,
    }

    impl Canvas {
        fn map(&self, points: &[Pt]) -> Vec<Pt> {
            points.iter().map(|&p| self.xf.apply(p)).collect()
        }

        fn fill(&mut self, points: &[(f32, f32)], color: Color) {
            let points: Vec<Pt> = points.iter().map(|&(x, y)| pt(x, y)).collect();
            self.fill_pts(&points, color);
        }

        fn fill_pts(&mut self, points: &[Pt], color: Color) {
            let shape = Shape::Fill(self.map(points));
            self.prims.push(Prim { shape, color });
        }

        fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, color: Color) {
            self.fill_pts(&ellipse_points(cx, cy, rx, ry), color);
        }

        fn stroke(&mut self, points: &[Pt], width: f32, color: Color, closed: bool, round: bool) {
            let shape = Shape::Line {
                pts: self.map(points),
                width,
                closed,
                round,
            };
            self.prims.push(Prim { shape, color });
        }

        fn line(&mut self, points: &[(f32, f32)], width: f32, color: Color, round: bool) {
            let points: Vec<Pt> = points.iter().map(|&(x, y)| pt(x, y)).collect();
            self.stroke(&points, width, color, false, round);
        }

        fn rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
            let shape = Shape::Rect {
                at: pt(x, y),
                width,
                height,
            };
            self.prims.push(Prim { shape, color });
        }

        fn dot(&mut self, at: Pt, radius: f32, color: Color) {
            let shape = Shape::Dot { at, radius };
            self.prims.push(Prim { shape, color });
        }
    }

    fn ellipse_points(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<Pt> {
        let segments = (8.0 + 2.5 * rx.max(ry)).ceil().clamp(12.0, 32.0) as usize;
        (0..segments)
            .map(|i| {
                let angle = TAU * i as f32 / segments as f32;
                pt(cx + rx * angle.cos(), cy + ry * angle.sin())
            })
            .collect()
    }

    fn arc_points(cx: f32, cy: f32, radius: f32, from: f32, to: f32, steps: usize) -> Vec<Pt> {
        (0..=steps)
            .map(|i| {
                let angle = from + (to - from) * i as f32 / steps as f32;
                pt(cx + radius * angle.cos(), cy + radius * angle.sin())
            })
            .collect()
    }

    fn quad_points(from: Pt, control: Pt, to: Pt, steps: usize) -> Vec<Pt> {
        (1..=steps)
            .map(|i| {
                let t = i as f32 / steps as f32;
                let u = 1.0 - t;
                pt(
                    u * u * from.x + 2.0 * u * t * control.x + t * t * to.x,
                    u * u * from.y + 2.0 * u * t * control.y + t * t * to.y,
                )
            })
            .collect()
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Spark {
        Smoke,
        Gold,
        Blue,
    }

    #[derive(Clone, Debug)]
    pub struct Particle {
        pub at: Pt,
        vx: f32,
        vy: f32,
        life: f32,
        age: f32,
        radius: f32,
        pub kind: Spark,
    }

    type Ridge = Vec<(f32, f32)>;

    fn ridge(rng: &mut Rng, low: f32, high: f32) -> Ridge {
        let mut points = Vec::new();
        let mut x = 0.0;
        while x < RIDGE_PERIOD {
            points.push((x, rng.range(low, high)));
            x += rng.range(16.0, 34.0);
        }
        points.push((RIDGE_PERIOD, points[0].1));
        points
    }

    #[derive(Clone, Debug)]
    pub struct Fx {
        rng: Rng,
        reduce_motion: bool,
        hover: bool,
        clock: f32,
        lag: f32,
        crash: f32,
        flash: f32,
        label: f32,
        wake: f32,
        wisps: f32,
        growth: Vec<f32>,
        particles: Vec<Particle>,
        rings: Vec<f32>,
        epoch: u32,
        realm: usize,
        phase: Phase,
        far: Ridge,
        near: Ridge,
    }

    impl Fx {
        pub fn new(seed: u64) -> Self {
            let mut rng = Rng::new(seed ^ 0xF0F0_5EED_C0DE_0001);
            let far = ridge(&mut rng, 16.0, 38.0);
            let near = ridge(&mut rng, 6.0, 20.0);
            Self {
                rng,
                reduce_motion: false,
                hover: false,
                clock: 0.0,
                lag: 0.0,
                crash: 0.0,
                flash: 0.0,
                label: 1.0,
                wake: 0.0,
                wisps: 0.0,
                growth: vec![1.0],
                particles: Vec::new(),
                rings: Vec::new(),
                epoch: 0,
                realm: 0,
                phase: Phase::Idle,
                far,
                near,
            }
        }

        pub fn set_reduce_motion(&mut self, reduce: bool) {
            self.reduce_motion = reduce;
        }

        pub fn set_hover(&mut self, hover: bool) {
            self.hover = hover;
        }

        pub fn particles(&self) -> &[Particle] {
            &self.particles
        }

        pub fn rings(&self) -> usize {
            self.rings.len()
        }

        pub fn glow(&self) -> f32 {
            self.label
        }

        pub fn busy(&self, runner: &Runner) -> bool {
            let phase = runner.phase();
            let target = self.wake_target(phase);
            phase == Phase::Running
                || (phase == Phase::Crashed && self.crash < 0.8)
                || !self.particles.is_empty()
                || !self.rings.is_empty()
                || self.flash > 0.0
                || self.label < 1.0
                || self.growth.iter().any(|g| *g < 1.0)
                || (!self.reduce_motion && (self.hover || (self.wake - target).abs() > 0.01))
        }

        pub fn step(&mut self, dt: f32, runner: &Runner) {
            if runner.epoch() != self.epoch {
                self.restart(runner.epoch());
            }
            if runner.realm() > self.realm {
                self.breakthrough(runner);
            }
            let phase = runner.phase();
            if phase == Phase::Crashed && self.phase != Phase::Crashed {
                self.crashed(runner);
            }
            self.phase = phase;
            let moving = !self.reduce_motion;
            self.clock += dt;
            let target = self.wake_target(phase);
            if moving {
                let rate = if target > self.wake { 10.0 } else { 6.0 };
                self.wake += (target - self.wake) * (dt * rate).min(1.0);
            } else {
                self.wake = target;
            }
            if phase == Phase::Running {
                self.lag = if moving {
                    let aim = (runner.vy() * 0.03).clamp(-7.0, 7.0);
                    self.lag + (aim - self.lag) * (dt * 12.0).min(1.0)
                } else {
                    0.0
                };
            }
            if phase == Phase::Crashed {
                self.crash += dt;
            }
            self.flash = (self.flash - dt / 0.25).max(0.0);
            self.label = (self.label + dt / 0.3).min(1.0);
            for growth in &mut self.growth {
                *growth = if moving {
                    (*growth + dt / 0.4).min(1.0)
                } else {
                    1.0
                };
            }
            for ring in &mut self.rings {
                *ring += dt;
            }
            self.rings.retain(|ring| *ring < 0.5);
            let drag = 0.92f32.powf(dt * 60.0);
            for spark in &mut self.particles {
                spark.age += dt;
                spark.at.x += spark.vx * dt;
                spark.at.y += spark.vy * dt;
                if spark.kind == Spark::Gold {
                    spark.vx *= drag;
                    spark.vy *= drag;
                }
            }
            self.particles.retain(|spark| spark.age < spark.life);
            if moving
                && self.wake > 0.4
                && phase != Phase::Crashed
                && self.particles.len() < MAX_PARTICLES
            {
                self.wisps += dt * self.growth.len() as f32 * (6.0 + 6.0 * self.wake);
                if self.wisps > 1.0 {
                    self.emit_wisps(runner);
                }
            }
        }

        fn wake_target(&self, phase: Phase) -> f32 {
            if phase == Phase::Running || self.hover {
                1.0
            } else {
                0.0
            }
        }

        fn restart(&mut self, epoch: u32) {
            self.growth = vec![1.0];
            self.particles.clear();
            self.rings.clear();
            self.lag = 0.0;
            self.crash = 0.0;
            self.flash = 0.0;
            self.label = 1.0;
            self.realm = 0;
            self.epoch = epoch;
        }

        fn breakthrough(&mut self, runner: &Runner) {
            self.realm = runner.realm();
            while self.growth.len() < (self.realm + 1).min(MAX_TAILS) {
                self.growth.push(0.0);
            }
            self.flash = 1.0;
            self.label = 0.0;
            if self.reduce_motion {
                return;
            }
            self.rings.push(0.0);
            for _ in 0..12 {
                let angle = self.rng.range(0.0, TAU);
                let speed = self.rng.range(30.0, 60.0);
                let spark = Particle {
                    at: pt(FOX_X + 3.0, GROUND - runner.height() - 12.0),
                    vx: angle.cos() * speed,
                    vy: angle.sin() * speed,
                    life: self.rng.range(0.35, 0.6),
                    age: 0.0,
                    radius: self.rng.range(0.9, 1.5),
                    kind: Spark::Gold,
                };
                self.particles.push(spark);
            }
        }

        fn crashed(&mut self, runner: &Runner) {
            self.crash = 0.0;
            for _ in 0..7 {
                let spark = Particle {
                    at: pt(
                        FOX_X - 14.0 + self.rng.range(-6.0, 4.0),
                        GROUND - runner.height() - 18.0 + self.rng.range(-6.0, 6.0),
                    ),
                    vx: self.rng.range(-14.0, -4.0),
                    vy: self.rng.range(-22.0, -10.0),
                    life: self.rng.range(0.6, 0.9),
                    age: 0.0,
                    radius: self.rng.range(1.8, 3.0),
                    kind: Spark::Smoke,
                };
                self.particles.push(spark);
            }
        }

        fn emit_wisps(&mut self, runner: &Runner) {
            let tips = flame_tips(runner, self);
            if tips.is_empty() {
                self.wisps = 0.0;
                return;
            }
            let drift = if runner.phase() == Phase::Running {
                runner.speed() * 0.35
            } else {
                0.0
            };
            while self.wisps > 1.0 {
                self.wisps -= 1.0;
                let tip = tips[self.rng.below(tips.len())];
                let spark = Particle {
                    at: tip,
                    vx: -drift - self.rng.range(4.0, 14.0),
                    vy: -self.rng.range(12.0, 28.0),
                    life: self.rng.range(0.4, 0.75),
                    age: 0.0,
                    radius: self.rng.range(0.9, 1.7),
                    kind: Spark::Blue,
                };
                self.particles.push(spark);
            }
        }
    }

    struct Tail {
        index: usize,
        base: Pt,
        angle: f32,
        length: f32,
        amp: f32,
        curl: f32,
    }

    struct Pose {
        xf: Xf,
        sitting: bool,
        crashed: bool,
        bob: f32,
        body: Color,
        tails: Vec<Tail>,
    }

    fn body_color(flash: f32) -> Color {
        if flash <= 0.0 {
            return BODY;
        }
        let [r, g, b] = std::array::from_fn(|i| {
            (BODY_RGB[i] + (FLASH_RGB[i] - BODY_RGB[i]) * flash).round() as u8
        });
        Color::rgba(r, g, b, 1.0)
    }

    fn pose(runner: &Runner, fx: &Fx) -> Pose {
        let phase = runner.phase();
        let sitting = phase == Phase::Idle;
        let crashed = phase == Phase::Crashed;
        let moving = !fx.reduce_motion;
        let crash = if crashed { ease(fx.crash / 0.3) } else { 0.0 };
        let intensity = (0.6 + 0.4 * fx.wake) * (1.0 - 0.8 * crash);
        let bob = if phase == Phase::Running && runner.height() == 0.0 {
            -(runner.distance() / 7.0).sin().abs() * 1.2
        } else {
            0.0
        };
        let rotation = if sitting {
            0.0
        } else if crashed {
            -0.3 * ease(fx.crash / 0.16)
        } else if runner.height() > 0.0 {
            (-runner.vy() / 460.0 * 0.22).clamp(-0.22, 0.22)
        } else {
            0.0
        };
        let (mut sx, mut sy) = (1.0, 1.0);
        if runner.landing() > 0.0 && moving {
            let squash = runner.landing() / LANDING;
            sx = 1.0 + 0.14 * squash;
            sy = 1.0 - 0.16 * squash;
        }
        let growth: Vec<f32> = if sitting {
            vec![1.0; runner.best_tail_count()]
        } else {
            fx.growth.clone()
        };
        let n = growth.len();
        let spread = (n as f32 - 1.0).max(0.0);
        let fan = if sitting {
            (0.3 * spread).min(2.2)
        } else {
            (0.2 * spread).min(1.3)
        };
        let speed_share = ((runner.speed() - START_SPEED) / 220.0).clamp(0.0, 1.0);
        let tails = growth
            .iter()
            .enumerate()
            .map(|(i, grown)| {
                let f = if n > 1 { i as f32 / spread - 0.5 } else { 0.0 };
                let sway = if moving {
                    0.07 * (fx.clock * 11.0 + i as f32 * 3.0).sin() * fx.wake
                } else {
                    0.0
                };
                let reach = if sitting {
                    22.0
                } else {
                    16.0 + 5.0 * speed_share
                };
                let side = if f > 0.0 { 1.0 } else { -1.0 };
                let fold = if n > 1 { side } else { -side };
                Tail {
                    index: i,
                    base: if sitting {
                        pt(-4.0, -6.0)
                    } else {
                        pt(-8.0, -10.0 + bob)
                    },
                    angle: if sitting {
                        -FRAC_PI_2 - if n > 1 { 0.55 } else { 0.45 } + fan * f
                    } else {
                        PI + 0.35 - 0.15 * speed_share + fan * f
                    },
                    length: reach
                        * ease(*grown)
                        * (1.0 + sway)
                        * (1.0 - 0.55 * crash)
                        * (0.75 + 0.25 * intensity),
                    amp: fx.wake * intensity,
                    curl: if sitting { 4.0 * fold } else { -1.5 },
                }
            })
            .collect();
        Pose {
            xf: Xf::fox(pt(FOX_X, GROUND - runner.height()), rotation, sx, sy),
            sitting,
            crashed,
            bob,
            body: body_color(fx.flash),
            tails,
        }
    }

    fn spine(tail: &Tail, fx: &Fx) -> [Pt; FLAME_SEGMENTS + 1] {
        let moving = !fx.reduce_motion;
        let (dy, dx) = tail.angle.sin_cos();
        let (nx, ny) = (-dy, dx);
        std::array::from_fn(|k| {
            let t = k as f32 / FLAME_SEGMENTS as f32;
            let wave = if moving {
                (fx.clock * 6.0 - t * 4.0 + tail.index as f32 * 1.7).sin() * 2.2
            } else {
                0.0
            };
            let sway = wave * t * t * tail.amp + tail.curl * t * t;
            pt(
                tail.base.x + dx * tail.length * t + nx * sway,
                tail.base.y + dy * tail.length * t + ny * sway + fx.lag * t * t,
            )
        })
    }

    fn flame_width(t: f32, tail: &Tail, fx: &Fx) -> f32 {
        let base = if t < 0.12 {
            1.6 + t / 0.12 * 1.8
        } else {
            3.4 * (1.0 - (t - 0.12) / 0.88).max(0.0).powf(1.25)
        };
        let flicker = if fx.reduce_motion {
            0.0
        } else {
            let i = tail.index as f32;
            (0.22 * (fx.clock * 17.0 + t * 11.0 + i * 2.1).sin()
                + 0.12 * (fx.clock * 29.0 - t * 17.0 + i).sin())
                * tail.amp
        };
        (base * (1.0 + flicker)).max(0.0)
    }

    fn flame_layers(tail: &Tail, fx: &Fx) -> Vec<(Color, Vec<Pt>)> {
        let spine = spine(tail, fx);
        FIRE.iter()
            .enumerate()
            .map(|(layer, &(scale, color))| {
                let (mut left, mut right) = (Vec::new(), Vec::new());
                for (k, p) in spine.iter().enumerate() {
                    let t = k as f32 / FLAME_SEGMENTS as f32;
                    let before = spine[k.saturating_sub(1)];
                    let after = spine[(k + 1).min(FLAME_SEGMENTS)];
                    let (ex, ey) = (after.x - before.x, after.y - before.y);
                    let len = ex.hypot(ey);
                    let len = if len == 0.0 { 1.0 } else { len };
                    let mut w = flame_width(t, tail, fx) * scale;
                    if layer == 3 {
                        w *= if t < 0.7 {
                            1.0
                        } else {
                            ((1.0 - t) / 0.3).max(0.0)
                        };
                    }
                    left.push(pt(p.x - ey / len * w, p.y + ex / len * w));
                    right.push(pt(p.x + ey / len * w, p.y - ex / len * w));
                }
                right.reverse();
                left.extend(right);
                (color, left)
            })
            .collect()
    }

    fn flame_tips(runner: &Runner, fx: &Fx) -> Vec<Pt> {
        let pose = pose(runner, fx);
        pose.tails
            .iter()
            .map(|tail| pose.xf.apply(spine(tail, fx)[FLAME_SEGMENTS]))
            .collect()
    }

    pub fn scene(runner: &Runner, fx: &Fx) -> Vec<Prim> {
        let mut canvas = Canvas {
            prims: Vec::new(),
            xf: Xf::IDENTITY,
        };
        let offset = if fx.reduce_motion {
            0.0
        } else {
            runner.distance()
        };
        mountains(&mut canvas, &fx.far, offset * 0.06, FAR_RIDGE);
        mountains(&mut canvas, &fx.near, offset * 0.16, NEAR_RIDGE);
        for (i, (x, y)) in [(150.0f32, 22.0f32), (40.0, 34.0)].into_iter().enumerate() {
            let span = WIDTH + 40.0;
            let drift = offset * (0.03 + i as f32 * 0.015);
            cloud(&mut canvas, ((x - drift) % span + span) % span - 20.0, y);
        }
        canvas.rect(0.0, GROUND, WIDTH, 1.0, GROUND_LINE);
        let still = fx.reduce_motion || runner.phase() != Phase::Running;
        for obstacle in runner.obstacles() {
            hazard(&mut canvas, obstacle, fx, still);
        }
        fox(&mut canvas, runner, fx);
        for age in &fx.rings {
            let progress = age / 0.5;
            let around = arc_points(
                FOX_X + 3.0,
                GROUND - runner.height() - 12.0,
                6.0 + 22.0 * ease(progress),
                0.0,
                TAU,
                32,
            );
            canvas.stroke(
                &around,
                1.5,
                RING.alpha(0.9 * (1.0 - progress)),
                true,
                false,
            );
        }
        for spark in &fx.particles {
            let k = 1.0 - spark.age / spark.life;
            let (color, radius) = match spark.kind {
                Spark::Gold => (Color::rgba(253, 224, 71, k), spark.radius * (0.4 + 0.6 * k)),
                Spark::Smoke => (
                    Color::rgba(150, 150, 165, 0.45 * k),
                    spark.radius * (2.0 - k),
                ),
                Spark::Blue => (
                    Color::rgba(147, 197, 253, 0.85 * k),
                    spark.radius * (0.4 + 0.6 * k),
                ),
            };
            canvas.dot(spark.at, radius, color);
        }
        canvas.prims
    }

    fn mountains(canvas: &mut Canvas, ridge: &Ridge, offset: f32, color: Color) {
        let shift = offset % RIDGE_PERIOD;
        let mut points = vec![pt(-shift, GROUND)];
        for lap in [0.0, RIDGE_PERIOD] {
            for &(x, h) in ridge {
                points.push(pt(x + lap - shift, GROUND - h));
            }
        }
        points.push(pt(2.0 * RIDGE_PERIOD - shift, GROUND));
        canvas.fill_pts(&points, color);
    }

    fn cloud(canvas: &mut Canvas, x: f32, y: f32) {
        let start = pt(x, y + 6.0);
        let mut outline = vec![start];
        outline.extend(quad_points(
            start,
            pt(x + 1.5, y + 1.0),
            pt(x + 6.0, y + 2.5),
            6,
        ));
        outline.extend(quad_points(
            pt(x + 6.0, y + 2.5),
            pt(x + 9.0, y - 2.5),
            pt(x + 14.0, y + 2.0),
            6,
        ));
        outline.extend(quad_points(
            pt(x + 14.0, y + 2.0),
            pt(x + 19.5, y + 1.0),
            pt(x + 18.5, y + 6.0),
            6,
        ));
        canvas.stroke(&outline, 1.0, CLOUD, true, false);
        let curl = arc_points(x + 9.5, y + 4.0, 1.6, PI, PI * 2.3, 8);
        canvas.stroke(&curl, 1.0, CLOUD, false, false);
    }

    fn rock(canvas: &mut Canvas, x: f32, w: f32, h: f32) {
        canvas.fill(
            &[
                (x, GROUND),
                (x + w * 0.18, GROUND - h * 0.7),
                (x + w * 0.45, GROUND - h),
                (x + w * 0.85, GROUND - h * 0.6),
                (x + w, GROUND),
            ],
            ROCK,
        );
        canvas.line(
            &[
                (x + w * 0.2, GROUND - h * 0.68),
                (x + w * 0.45, GROUND - h + 0.4),
            ],
            0.8,
            ROCK_VEIN,
            false,
        );
    }

    fn hazard(canvas: &mut Canvas, obstacle: &Obstacle, fx: &Fx, still: bool) {
        let x = obstacle.x;
        match obstacle.kind {
            Hazard::Rock => rock(canvas, x, 10.0, 8.0),
            Hazard::Rocks => {
                rock(canvas, x, 11.0, 11.0);
                rock(canvas, x + 9.0, 9.0, 7.0);
            }
            Hazard::Talisman => {
                let flutter = if still {
                    0.0
                } else {
                    (fx.clock * 9.0 + x).sin() * 0.8
                };
                canvas.rect(x + 3.4, GROUND - 17.0, 1.3, 17.0, POLE);
                canvas.fill(
                    &[
                        (x + 0.5, GROUND - 16.0),
                        (x + 7.5, GROUND - 16.0),
                        (x + 7.5 + flutter, GROUND - 5.0),
                        (x + 0.5 + flutter, GROUND - 5.0),
                    ],
                    PAPER,
                );
                canvas.line(
                    &[
                        (x + 4.0, GROUND - 14.5),
                        (x + 2.5, GROUND - 12.0),
                        (x + 5.5, GROUND - 10.0),
                        (x + 3.5 + flutter, GROUND - 7.0),
                    ],
                    0.9,
                    INK,
                    false,
                );
            }
            Hazard::Step => {
                canvas.rect(x, GROUND - 9.0, 14.0, 9.0, STEP);
                canvas.rect(x, GROUND - 9.0, 14.0, 1.2, STEP_TOP);
                canvas.line(
                    &[
                        (x + 8.0, GROUND - 9.0),
                        (x + 6.5, GROUND - 5.0),
                        (x + 8.5, GROUND - 2.0),
                    ],
                    0.8,
                    STEP_CRACK,
                    false,
                );
            }
            Hazard::Bolt => storm(canvas, obstacle, fx, still),
        }
    }

    fn storm(canvas: &mut Canvas, obstacle: &Obstacle, fx: &Fx, still: bool) {
        let x = obstacle.x;
        for (dx, cy, r) in [(0.0, 12.0, 6.0), (6.0, 9.0, 7.0), (12.0, 12.0, 5.0)] {
            canvas.dot(pt(x - 4.0 + dx, cy), r, STORM);
        }
        if obstacle.strike <= 0.0 {
            return;
        }
        if obstacle.strike < 0.28 && !fx.reduce_motion {
            let fade = 1.0 - obstacle.strike / 0.28;
            let mut path = vec![(x + 3.0, 17.0)];
            for (i, z) in obstacle.zig.iter().enumerate() {
                path.push((x + 3.0 + z, 17.0 + (GROUND - 17.0) * (i + 1) as f32 / 7.0));
            }
            path.push((x + 3.0, GROUND));
            canvas.line(&path, 4.0, BOLT_HALO.alpha(0.35 * fade), true);
            canvas.line(&path, 1.6, BOLT_FLASH.alpha(fade), true);
        }
        let flicker = if still {
            1.0
        } else {
            0.8 + 0.2 * (fx.clock * 23.0 + x).sin()
        };
        canvas.ellipse(
            x + 3.5,
            GROUND - 4.0,
            7.0,
            6.0,
            BOLT_GLOW.alpha(0.22 * flicker),
        );
        canvas.fill(
            &[
                (x, GROUND),
                (x + 2.0, GROUND - 8.0),
                (x + 3.5, GROUND - 5.0),
                (x + 4.5, GROUND - 12.0),
                (x + 7.0, GROUND),
            ],
            BOLT_CORE.alpha(0.92 * flicker),
        );
    }

    fn leg(
        canvas: &mut Canvas,
        runner: &Runner,
        hip: (f32, f32),
        turn: f32,
        front: bool,
        color: Color,
    ) {
        let (hx, hy) = hip;
        let stride = runner.distance() / 7.0 + turn;
        let (foot_x, foot_y) = if runner.height() > 0.0 || runner.vy() > 0.0 {
            (hx + if front { 4.0 } else { -4.0 }, hy + 4.6)
        } else if runner.phase() == Phase::Running {
            (hx + stride.sin() * 3.2, -(-stride.cos()).max(0.0) * 2.2)
        } else {
            (hx, 0.0)
        };
        canvas.line(&[(hx, hy), (foot_x, foot_y.min(0.0))], 2.2, color, true);
    }

    fn fox(canvas: &mut Canvas, runner: &Runner, fx: &Fx) {
        let pose = pose(runner, fx);
        canvas.xf = pose.xf;
        for tail in pose.tails.iter().filter(|tail| tail.length >= 0.05) {
            for (color, polygon) in flame_layers(tail, fx) {
                canvas.fill_pts(&polygon, color);
            }
        }
        let (body, bob) = (pose.body, pose.bob);
        if pose.sitting {
            canvas.ellipse(-3.0, -4.0, 6.0, 4.5, body);
            canvas.ellipse(0.0, -9.0, 6.5, 8.0, body);
            canvas.ellipse(3.0, -1.0, 2.0, 1.3, body);
            canvas.ellipse(6.0, -1.0, 2.0, 1.3, body);
            canvas.ellipse(3.0, -19.0, 5.2, 5.2, body);
            canvas.fill(&[(6.0, -21.0), (11.6, -18.5), (6.0, -16.4)], body);
            canvas.fill(&[(-1.0, -22.0), (0.0, -29.5), (3.0, -23.5)], body);
            canvas.fill(&[(3.0, -23.5), (6.5, -29.5), (7.4, -22.0)], body);
            canvas.fill(&[(4.0, -24.0), (6.2, -28.0), (6.6, -23.0)], PINK);
            canvas.ellipse(11.1, -18.6, 0.9, 0.9, DARK);
            let eye = arc_points(5.6, -20.2, 1.2, 0.15, PI - 0.15, 6);
            canvas.stroke(&eye, 0.9, DARK, false, false);
            canvas.fill(
                &[(2.4, -25.4), (3.4, -24.2), (2.4, -23.0), (1.4, -24.2)],
                MARK,
            );
            canvas.ellipse(6.4, -17.0, 1.3, 0.7, CHEEK);
            return;
        }
        leg(canvas, runner, (-3.0, -6.0 + bob), 0.6, false, REAR_LEG);
        leg(canvas, runner, (7.0, -6.0 + bob), PI + 0.6, true, REAR_LEG);
        canvas.ellipse(0.0, -9.0 + bob, 9.0, 5.2, body);
        canvas.ellipse(6.0, -9.5 + bob, 3.5, 4.0, body);
        leg(canvas, runner, (-5.0, -6.0 + bob), 0.0, false, body);
        leg(canvas, runner, (5.0, -6.0 + bob), PI, true, body);
        canvas.ellipse(10.0, -14.0 + bob, 5.0, 5.0, body);
        canvas.fill(
            &[
                (13.0, -16.0 + bob),
                (19.5, -13.0 + bob),
                (13.0, -11.0 + bob),
            ],
            body,
        );
        canvas.fill(
            &[(6.5, -17.0 + bob), (7.5, -24.5 + bob), (10.5, -18.5 + bob)],
            body,
        );
        canvas.fill(
            &[
                (10.0, -18.5 + bob),
                (13.0, -24.5 + bob),
                (14.0, -17.0 + bob),
            ],
            body,
        );
        canvas.fill(
            &[
                (11.0, -18.8 + bob),
                (12.8, -23.0 + bob),
                (13.3, -18.0 + bob),
            ],
            PINK,
        );
        canvas.ellipse(19.0, -13.1 + bob, 0.9, 0.9, DARK);
        if pose.crashed {
            canvas.line(&[(11.6, -16.4), (13.6, -14.0)], 0.9, DARK, false);
            canvas.line(&[(13.6, -16.4), (11.6, -14.0)], 0.9, DARK, false);
        } else {
            canvas.ellipse(12.6, -15.0 + bob, 0.9, 1.35, DARK);
        }
        canvas.fill(
            &[
                (9.0, -20.4 + bob),
                (10.0, -19.2 + bob),
                (9.0, -18.0 + bob),
                (8.0, -19.2 + bob),
            ],
            MARK,
        );
        canvas.ellipse(12.5, -12.3 + bob, 1.3, 0.7, CHEEK);
    }

    pub enum Hud {
        Blank,
        Best(u32),
        Run {
            score: u32,
            realm: &'static str,
            glow: f32,
        },
    }

    pub fn hud(runner: &Runner, fx: &Fx) -> Hud {
        match (runner.phase(), runner.best()) {
            (Phase::Idle, 0) => Hud::Blank,
            (Phase::Idle, best) => Hud::Best(best),
            _ => Hud::Run {
                score: runner.score(),
                realm: REALMS[runner.realm()].name,
                glow: fx.glow(),
            },
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::ui::fox::play::{FRAME, running};

        fn fire_fills(prims: &[Prim]) -> usize {
            prims
                .iter()
                .filter(|prim| {
                    matches!(prim.shape, Shape::Fill(_))
                        && FIRE.iter().any(|(_, color)| *color == prim.color)
                })
                .count()
        }

        fn world(seed: u64) -> (Runner, Fx) {
            (running(seed), Fx::new(seed))
        }

        fn tick(runner: &mut Runner, fx: &mut Fx, reduce_motion: bool) {
            fx.set_reduce_motion(reduce_motion);
            runner.step(FRAME);
            fx.step(FRAME, runner);
        }

        fn calm(runner: &Runner, fx: &mut Fx, seconds: f32) {
            for _ in 0..(seconds * 60.0) as usize {
                fx.step(FRAME, runner);
            }
        }

        fn cultivate(until: u32) -> Runner {
            let (mut runner, mut fx) = world(5);
            while runner.score() < until {
                tick(&mut runner, &mut fx, false);
                runner.clear_obstacles();
            }
            runner.pause();
            runner.rest();
            runner
        }

        #[test]
        fn the_resting_fox_burns_one_tail_per_realm_of_its_best_run() {
            for (score, tails) in [(0, 1), (150, 2), (460, 4), (1010, 6)] {
                let runner = cultivate(score);
                let fx = Fx::new(5);
                assert_eq!(fire_fills(&scene(&runner, &fx)), 4 * tails, "best {score}");
            }
        }

        #[test]
        fn the_resting_fire_is_still_until_the_pointer_is_over_it() {
            let runner = cultivate(150);
            let shape_after = |hover: bool, reduce_motion: bool| {
                let mut fx = Fx::new(5);
                fx.set_reduce_motion(reduce_motion);
                fx.set_hover(hover);
                calm(&runner, &mut fx, 1.0);
                let before = scene(&runner, &fx);
                calm(&runner, &mut fx, 0.37);
                (before, scene(&runner, &fx))
            };
            let (before, after) = shape_after(false, false);
            assert_eq!(before, after, "an idle fox is a still picture");
            let (before, after) = shape_after(true, false);
            assert_ne!(before, after, "hover wakes the fire");
            let (before, after) = shape_after(true, true);
            assert_eq!(before, after, "reduce motion holds the shape");
        }

        #[test]
        fn hovering_the_resting_fox_lets_wisps_rise_unless_motion_is_reduced() {
            let runner = cultivate(150);
            let wisps = |reduce_motion: bool| {
                let mut fx = Fx::new(5);
                fx.set_reduce_motion(reduce_motion);
                fx.set_hover(true);
                calm(&runner, &mut fx, 1.5);
                fx.particles()
                    .iter()
                    .filter(|spark| spark.kind == Spark::Blue)
                    .count()
            };
            assert!(wisps(false) > 0);
            assert_eq!(wisps(true), 0);
        }

        #[test]
        fn the_resting_scene_asks_for_frames_only_while_something_moves() {
            let runner = cultivate(150);
            let mut fx = Fx::new(5);
            assert!(!fx.busy(&runner), "a fresh idle scene is drawn once");
            fx.set_hover(true);
            assert!(fx.busy(&runner), "a hovered fire flickers");
            calm(&runner, &mut fx, 1.0);
            fx.set_hover(false);
            assert!(fx.busy(&runner), "the fire is still settling");
            calm(&runner, &mut fx, 3.0);
            assert!(!fx.busy(&runner), "settled again");
            fx.set_reduce_motion(true);
            fx.set_hover(true);
            fx.step(FRAME, &runner);
            calm(&runner, &mut fx, 1.0);
            assert!(!fx.busy(&runner), "a reduced-motion hover needs no frames");
        }

        #[test]
        fn a_run_wants_frames_and_a_crash_settles_into_embers() {
            let (mut runner, mut fx) = world(3);
            runner.place(Hazard::Step, FOX_X + 40.0);
            runner.press();
            assert!(fx.busy(&runner));
            let mut frames = 0;
            while runner.phase() == Phase::Running {
                tick(&mut runner, &mut fx, false);
                frames += 1;
                assert!(frames < 600);
            }
            assert_eq!(runner.phase(), Phase::Crashed);
            assert!(fx.busy(&runner));
            assert!(
                fx.particles()
                    .iter()
                    .any(|spark| spark.kind == Spark::Smoke),
                "a crash gutters into smoke"
            );
            for _ in 0..120 {
                tick(&mut runner, &mut fx, false);
            }
            assert!(!fx.busy(&runner));
        }

        #[test]
        fn a_breakthrough_adds_a_tail_a_ring_and_a_gold_burst() {
            let (mut runner, mut fx) = world(5);
            runner.press();
            while runner.realm() < 1 {
                tick(&mut runner, &mut fx, false);
                runner.clear_obstacles();
            }
            assert_eq!(fx.rings(), 1);
            assert_eq!(
                fx.particles()
                    .iter()
                    .filter(|spark| spark.kind == Spark::Gold)
                    .count(),
                12
            );
            assert!(fx.glow() < 0.1, "the realm name relights");
            for _ in 0..45 {
                tick(&mut runner, &mut fx, false);
                runner.clear_obstacles();
            }
            assert_eq!(fire_fills(&scene(&runner, &fx)), 8);
        }

        #[test]
        fn reduce_motion_never_changes_the_physics() {
            let (mut calm_runner, mut calm_fx) = world(8);
            let (mut still_runner, mut still_fx) = world(8);
            calm_runner.press();
            still_runner.press();
            let (mut rings, mut gold) = (0, 0);
            for frame in 0..60 * 20 {
                if frame % 53 == 0 {
                    calm_runner.press();
                    still_runner.press();
                }
                if frame % 53 == 20 {
                    calm_runner.release();
                    still_runner.release();
                }
                tick(&mut calm_runner, &mut calm_fx, false);
                tick(&mut still_runner, &mut still_fx, true);
                calm_runner.clear_obstacles();
                still_runner.clear_obstacles();
                assert_eq!(calm_runner, still_runner, "frame {frame}");
                rings += calm_fx.rings();
                gold += calm_fx
                    .particles()
                    .iter()
                    .filter(|spark| spark.kind == Spark::Gold)
                    .count();
                assert_eq!(still_fx.rings(), 0);
                assert!(
                    still_fx
                        .particles()
                        .iter()
                        .all(|spark| spark.kind == Spark::Smoke)
                );
            }
            assert!(rings > 0 && gold > 0, "the moving run did break through");
            assert!(calm_runner.realm() >= 2);
        }

        #[test]
        fn reduce_motion_drops_parallax_and_squash_but_not_the_game() {
            let ridge_start = |reduce_motion: bool, run_for: usize| {
                let (mut runner, mut fx) = world(4);
                runner.press();
                for _ in 0..run_for {
                    tick(&mut runner, &mut fx, reduce_motion);
                    runner.clear_obstacles();
                }
                let Shape::Fill(points) = &scene(&runner, &fx)[0].shape else {
                    panic!("the far ridge comes first");
                };
                points[0].x
            };
            assert_eq!(ridge_start(true, 30), ridge_start(true, 200));
            assert_ne!(ridge_start(false, 30), ridge_start(false, 200));

            let squash = |reduce_motion: bool| {
                let (mut runner, mut fx) = world(4);
                for _ in 0..30 {
                    tick(&mut runner, &mut fx, reduce_motion);
                    runner.clear_obstacles();
                }
                runner.press();
                runner.release();
                while runner.airborne() || runner.landing() == 0.0 {
                    tick(&mut runner, &mut fx, reduce_motion);
                    runner.clear_obstacles();
                }
                pose(&runner, &fx).xf.a
            };
            assert_eq!(squash(true), 1.0);
            assert!(squash(false) > 1.05);
        }

        #[test]
        fn the_hud_shows_best_when_resting_and_the_realm_while_playing() {
            let runner = cultivate(150);
            let mut fx = Fx::new(5);
            let Hud::Best(best) = hud(&runner, &fx) else {
                panic!("a rested fox with a best shows it");
            };
            assert!(best >= 150);
            assert!(matches!(hud(&Runner::new(1), &Fx::new(1)), Hud::Blank));
            let mut runner = runner;
            runner.press();
            fx.step(FRAME, &runner);
            let Hud::Run { realm, .. } = hud(&runner, &fx) else {
                panic!("a run shows its realm");
            };
            assert_eq!(realm, "Qi Condensation");
        }

        #[test]
        fn the_scene_never_emits_a_point_that_is_not_a_number() {
            let (mut runner, mut fx) = world(6);
            runner.press();
            for _ in 0..60 * 5 {
                tick(&mut runner, &mut fx, false);
                runner.clear_obstacles();
                for prim in scene(&runner, &fx) {
                    if let Shape::Fill(points) | Shape::Line { pts: points, .. } = &prim.shape {
                        assert!(points.iter().all(|p| p.x.is_finite() && p.y.is_finite()));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod play {
    use super::model::{FOX_X, Runner};

    pub const FRAME: f32 = 1.0 / 60.0;

    pub fn running(seed: u64) -> Runner {
        let mut runner = Runner::new(seed);
        runner.press();
        runner.release();
        while runner.airborne() {
            runner.step(FRAME);
        }
        runner.clear_obstacles();
        runner
    }

    pub fn tap_when_close(runner: &mut Runner) {
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
}
