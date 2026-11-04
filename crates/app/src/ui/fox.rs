#![expect(dead_code, reason = "the sidebar mounts the fox in a later slice")]

mod model {
    pub const WIDTH: f32 = 239.0;
    pub const HEIGHT: f32 = 112.0;
    pub const GROUND: f32 = 98.0;
    pub const FOX_X: f32 = 52.0;

    const JUMP_SPEED: f32 = 460.0;
    const GRAVITY: f32 = 2600.0;
    const GRAVITY_HELD: f32 = 1500.0;
    const START_SPEED: f32 = 140.0;
    const MAX_SPEED: f32 = 360.0;
    const ACCELERATION: f32 = 4.0;
    const LANDING: f32 = 0.09;
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

        const FRAME: f32 = 1.0 / 60.0;

        fn running(seed: u64) -> Runner {
            let mut runner = Runner::new(seed);
            runner.press();
            runner.release();
            while runner.airborne() {
                runner.step(FRAME);
            }
            runner.clear_obstacles();
            runner
        }

        fn tap_when_close(runner: &mut Runner) {
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
