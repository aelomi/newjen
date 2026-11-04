//! Motion for the thread list. A row that changes place does not jump. The one that leaves stays
//! on screen as a ghost, fading, and only then does the gap it leaves close. The one that arrives
//! waits for that, opens room, then fades in.
//!
//! [`Motion`] holds what is in flight. Each render hands it the keys of everything the list
//! shows ([`Sight`]). It compares them with the last render's, starts a [`Block`] for every key
//! that appeared or vanished, and answers with the keys to draw, ghosts spliced in where they
//! were, and how far along each one is ([`Frame`]). Everything is a function of the time it is
//! given, so the tests need no window.
//!
//! Nothing here moves on a keyboard action, a hover, the first render or a project switch:
//! [`Motion::snap`] and a fresh `Motion` make the next render jump to the end.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, ElementId, InteractiveElement as _, IntoElement as _, ParentElement as _, Pixels,
    SharedString, Styled as _, div, px,
};
use newjen_core::steps::Placement;

use super::dock::{self, SheetKind, TrayRow};
use super::{READ_GAP, ROW_HEIGHT, Row, Section};
use crate::ui::Tween;

const fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// How long a count takes to crossfade into its new value.
const COUNT: Duration = ms(120);

/// A panel grows up from its row over this long, everything behind it dims as long, and it
/// lowers a little quicker.
const RISE: Duration = ms(220);
const LOWER: Duration = ms(160);
const DIM: Duration = ms(220);

/// The height of a section header.
pub const HEADER_HEIGHT: Pixels = px(35.);

/// Where a thread shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Zone {
    Draft,
    Pinned,
    Queue,
    Read,
    Tray,
    Snoozed,
    Settled,
}

impl Zone {
    pub fn of(draft: bool, placement: Placement) -> Self {
        if draft {
            return Self::Draft;
        }
        match placement {
            Placement::Pinned => Self::Pinned,
            Placement::Queue => Self::Queue,
            Placement::Read => Self::Read,
            Placement::InFlight => Self::Tray,
            Placement::Snoozed => Self::Snoozed,
            Placement::Settled => Self::Settled,
        }
    }

    fn in_list(self) -> bool {
        matches!(self, Self::Draft | Self::Pinned | Self::Queue | Self::Read)
    }
}

/// Which stack of elements a key belongs to. Ghosts are spliced into their own stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    List,
    Dock,
}

/// One thing the list can show, told apart from its neighbours across renders.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Header(Section),
    Gap,
    /// A thread's row in one of the list's zones. A move changes the zone, so it is a leave and
    /// an arrival.
    Row(Zone, SharedString),
    /// The In flight header.
    Flight,
    /// A thread's row in the In flight tray.
    Tray(SharedString),
    /// The rule between the tray and the Snoozed and Settled rows.
    Sep,
    Disclosure(SheetKind),
}

impl Key {
    pub fn lane(&self) -> Lane {
        match self {
            Self::Header(_) | Self::Gap | Self::Row(..) => Lane::List,
            Self::Flight | Self::Tray(_) | Self::Sep | Self::Disclosure(_) => Lane::Dock,
        }
    }

    /// The height the element has when it is fully there.
    pub fn natural(&self) -> Pixels {
        match self {
            Self::Header(_) => HEADER_HEIGHT,
            Self::Gap => READ_GAP,
            Self::Row(..) => ROW_HEIGHT,
            Self::Flight => dock::TRAY_HEADER,
            Self::Tray(_) | Self::Disclosure(_) => dock::ROW,
            Self::Sep => dock::SEP,
        }
    }

    /// The zone and thread of a key that is a thread's row.
    fn origin(&self) -> Option<(Zone, &SharedString)> {
        match self {
            Self::Row(zone, id) => Some((*zone, id)),
            Self::Tray(id) => Some((Zone::Tray, id)),
            Self::Header(_) | Self::Gap | Self::Flight | Self::Sep | Self::Disclosure(_) => None,
        }
    }

    fn thread(&self) -> Option<&SharedString> {
        match self {
            Self::Row(_, id) | Self::Tray(id) => Some(id),
            Self::Header(_) | Self::Gap | Self::Flight | Self::Sep | Self::Disclosure(_) => None,
        }
    }

    fn element_id(&self) -> ElementId {
        let name = match self {
            Self::Header(section) => format!("slot-header-{section:?}"),
            Self::Gap => "slot-gap".to_string(),
            Self::Row(zone, id) => format!("slot-row-{zone:?}-{id}"),
            Self::Flight => "slot-flight".to_string(),
            Self::Tray(id) => format!("slot-tray-{id}"),
            Self::Sep => "slot-sep".to_string(),
            Self::Disclosure(kind) => format!("slot-disclosure-{kind:?}"),
        };
        ElementId::from(SharedString::from(name))
    }
}

/// The counts the list shows, each of which crossfades when it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Counter {
    Pinned,
    Queue,
    Need,
    Flight,
    Snoozed,
    Settled,
}

/// A count, and the one it replaces while the two crossfade.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Counted {
    pub value: usize,
    pub was: Option<usize>,
    /// 0 to 1: how far the new value has faded in.
    pub mix: f32,
}

impl Counted {
    pub fn steady(value: usize) -> Self {
        Self {
            value,
            was: None,
            mix: 1.,
        }
    }
}

/// How one thing leaves or arrives.
#[derive(Clone, Copy, Debug)]
struct Recipe {
    /// It begins this long after the change.
    wait: Duration,
    fade: Duration,
    /// The height begins this long after `wait`: a leaving row closes its gap after it fades.
    size_wait: Duration,
    size: Duration,
    /// Where the row sits when it is hidden, in pixels right and down from where it rests.
    shift: (f32, f32),
    /// How long the row's status mark takes to grow from 85% to its size.
    pop: Option<Duration>,
}

/// Switching away from a working thread: the row fades and sinks 6px, then the gap closes.
const FOLD_OUT: Recipe = Recipe {
    wait: ms(0),
    fade: ms(140),
    size_wait: ms(140),
    size: ms(180),
    shift: (0., 6.),
    pop: None,
};

/// Settling or snoozing a row: it fades and slides 8px right, then the gap closes.
const LEAVE: Recipe = Recipe {
    shift: (8., 0.),
    ..FOLD_OUT
};

/// A row that moves to another place in the list fades where it was, then the gap closes.
const MOVE_OUT: Recipe = Recipe {
    shift: (0., 0.),
    ..FOLD_OUT
};

/// A tray row whose thread landed: it fades quickly, then the gap closes.
const LAND_OUT: Recipe = Recipe {
    fade: ms(120),
    size_wait: ms(120),
    ..MOVE_OUT
};

/// A thread arriving in the tray: the row opens room and fades in a moment after the fold begins.
const FOLD_IN: Recipe = Recipe {
    wait: ms(60),
    fade: ms(160),
    size_wait: ms(0),
    size: ms(180),
    shift: (0., 0.),
    pop: None,
};

/// A thread arriving in the list after its tray row faded: room opens, and the row fades in from
/// 4px above while its mark grows.
const LAND_IN: Recipe = Recipe {
    wait: ms(120),
    fade: ms(200),
    size_wait: ms(0),
    size: ms(180),
    shift: (0., -4.),
    pop: Some(ms(160)),
};

/// A row arriving from another place in the list waits for the gap it left to start closing.
const MOVE_IN: Recipe = Recipe {
    wait: ms(140),
    pop: None,
    ..LAND_IN
};

/// A snoozed or settled thread coming back needs no gap to wait for.
const WAKE_IN: Recipe = Recipe {
    wait: ms(0),
    pop: None,
    ..LAND_IN
};

/// A section header, the gap above the read list and a dock row grow and fade together.
const BLOCK: Recipe = Recipe {
    wait: ms(0),
    fade: ms(180),
    size_wait: ms(0),
    size: ms(180),
    shift: (0., 0.),
    pop: None,
};

/// What a leaving row does, given where it goes. `None` when it just disappears.
fn exit_recipe(from: Zone, to: Option<Zone>) -> Option<Recipe> {
    match (from, to) {
        (Zone::Tray, _) => Some(LAND_OUT),
        (Zone::Draft, None) => None,
        (_, Some(Zone::Settled | Zone::Snoozed)) => Some(LEAVE),
        (_, Some(Zone::Tray)) => Some(FOLD_OUT),
        _ => Some(MOVE_OUT),
    }
}

/// What an arriving row does, given where it came from. `None` when it just appears: a thread
/// seen for the first time has nowhere to have come from.
fn enter_recipe(from: Option<Zone>, to: Zone) -> Option<Recipe> {
    match (from?, to) {
        (Zone::Tray, to) if to.in_list() => Some(LAND_IN),
        (from, Zone::Tray) if from.in_list() => Some(FOLD_IN),
        (from, to) if from.in_list() && to.in_list() => Some(Recipe {
            pop: LAND_IN.pop.filter(|_| to == Zone::Queue),
            ..MOVE_IN
        }),
        (Zone::Settled | Zone::Snoozed, to) if to.in_list() => Some(WAKE_IN),
        _ => None,
    }
}

/// One key's progress: its opacity, its height and its mark fade on their own clocks.
#[derive(Clone, Copy, Debug)]
struct Block {
    fade: Tween<f32>,
    fade_len: Duration,
    size: Tween<f32>,
    size_len: Duration,
    pop: Tween<f32>,
    pop_len: Duration,
    shift: (f32, f32),
    leaving: bool,
}

impl Block {
    fn shown(now: Instant) -> Self {
        Self {
            fade: Tween::at(1., now),
            fade_len: BLOCK.fade,
            size: Tween::at(1., now),
            size_len: BLOCK.size,
            pop: Tween::at(1., now),
            pop_len: ms(1),
            shift: (0., 0.),
            leaving: false,
        }
    }

    fn hidden(now: Instant) -> Self {
        Self {
            fade: Tween::at(0., now),
            size: Tween::at(0., now),
            ..Self::shown(now)
        }
    }

    fn leave(&mut self, recipe: Recipe, now: Instant) {
        if self.leaving {
            return;
        }
        let begin = now + recipe.wait;
        self.fade.aim_at(0., begin, now, self.fade_len, true);
        self.size
            .aim_at(0., begin + recipe.size_wait, now, self.size_len, true);
        self.fade_len = recipe.fade;
        self.size_len = recipe.size;
        self.shift = recipe.shift;
        self.leaving = true;
    }

    fn arrive(&mut self, recipe: Recipe, now: Instant) {
        let begin = now + recipe.wait;
        self.fade.aim_at(1., begin, now, self.fade_len, true);
        self.size.aim_at(1., begin, now, self.size_len, true);
        self.fade_len = recipe.fade;
        self.size_len = recipe.size;
        self.shift = recipe.shift;
        self.leaving = false;
        if let Some(pop) = recipe.pop {
            self.pop = Tween::at(0., now);
            self.pop.aim_at(1., begin, now, pop, true);
            self.pop_len = pop;
        }
    }

    fn done(&self, now: Instant) -> bool {
        self.fade.settled(now, self.fade_len)
            && self.size.settled(now, self.size_len)
            && self.pop.settled(now, self.pop_len)
    }

    fn look(&self, now: Instant, reduce: bool) -> Look {
        let opacity = self.fade.value(now, self.fade_len);
        let away = 1. - opacity;
        let (dx, dy) = if reduce {
            (0., 0.)
        } else {
            (self.shift.0 * away, self.shift.1 * away)
        };
        Look {
            opacity,
            size: self.size.value(now, self.size_len),
            dx,
            dy,
            mark: self.pop.value(now, self.pop_len),
        }
    }
}

/// How an element looks at one moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub opacity: f32,
    /// The share of its natural height it takes.
    pub size: f32,
    pub dx: f32,
    pub dy: f32,
    /// How far the row's status mark has grown from 85% of its size to all of it.
    pub mark: f32,
}

impl Look {
    pub const STEADY: Self = Self {
        opacity: 1.,
        size: 1.,
        dx: 0.,
        dy: 0.,
        mark: 1.,
    };

    pub fn is_steady(&self) -> bool {
        *self == Self::STEADY
    }
}

/// One element to draw.
#[derive(Clone, Debug)]
pub struct Item {
    pub key: Key,
    pub look: Look,
    /// It is gone from the list and only stays to finish leaving: draw it from what it last
    /// showed, and let nothing click it.
    pub ghost: bool,
}

/// How far the Snoozed or Settled panel has come, and how far everything behind it has dimmed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Raised {
    /// 0 when it is closed, 1 when it is all the way up.
    pub panel: f32,
    /// 0 for no dimming, 1 for the full dimming.
    pub dim: f32,
}

/// What a render draws.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub items: Vec<Item>,
    pub raised: Raised,
    counts: HashMap<Counter, Counted>,
    last: HashMap<Counter, usize>,
    /// Something is still moving: ask for another frame.
    pub busy: bool,
}

impl Frame {
    pub fn lane(&self, lane: Lane) -> impl Iterator<Item = &Item> {
        self.items
            .iter()
            .filter(move |item| item.key.lane() == lane)
    }

    pub fn item(&self, key: &Key) -> Option<&Item> {
        self.items.iter().find(|item| &item.key == key)
    }

    /// A count as the render shows it. A count whose element is leaving keeps what it last said.
    pub fn counted(&self, counter: Counter) -> Counted {
        self.counts.get(&counter).copied().unwrap_or_else(|| {
            Counted::steady(self.last.get(&counter).copied().unwrap_or_default())
        })
    }
}

/// What the render found on screen, to compare with the render before.
#[derive(Clone, Debug, Default)]
pub struct Sight {
    /// The list's keys, top to bottom.
    pub keys: Vec<Key>,
    /// Where every thread is, including those with no element.
    pub zones: HashMap<SharedString, Zone>,
    pub counts: Vec<(Counter, usize)>,
    pub snoozed: usize,
    pub settled: usize,
    /// A Snoozed or Settled panel is open.
    pub panel_open: bool,
}

struct Lift {
    open: bool,
    panel: Tween<f32>,
    panel_len: Duration,
    dim: Tween<f32>,
}

struct Number {
    value: usize,
    was: Option<usize>,
    mix: Tween<f32>,
}

/// How many times slower debug builds run the motion, from `NEWJEN_MOTION_SLOW`: 5 shows every
/// step at a fifth of its speed, to look at it or to record it. Release builds ignore it.
fn slowdown() -> f32 {
    if !cfg!(debug_assertions) {
        return 1.;
    }
    std::env::var("NEWJEN_MOTION_SLOW")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|factor| factor.is_finite() && *factor > 1.)
        .unwrap_or(1.)
}

#[derive(Default)]
pub struct Motion {
    slow: Option<f32>,
    epoch: Option<Instant>,
    started: bool,
    snap: bool,
    live: Vec<Key>,
    zones: HashMap<SharedString, Zone>,
    blocks: HashMap<Key, Block>,
    shown: Vec<Key>,
    numbers: HashMap<Counter, Number>,
    last: HashMap<Counter, usize>,
    /// A row that left for Snoozed or Settled, and when it gets there: the dock counts it then.
    arrivals: Vec<(Zone, Instant)>,
    lift: Option<Lift>,
    rows: HashMap<SharedString, Row>,
    trays: HashMap<SharedString, TrayRow>,
    headers: HashMap<Section, (usize, usize)>,
}

impl Motion {
    /// The next render shows its changes at once: a key pressed, or a project shown again.
    pub fn snap(&mut self) {
        self.snap = true;
    }

    /// Remembers what a row showed, to draw it while it leaves.
    pub fn remember_row(&mut self, row: &Row) {
        self.rows.insert(row.id.clone(), row.clone());
    }

    pub fn row(&self, id: &str) -> Option<&Row> {
        self.rows.get(id)
    }

    pub fn remember_tray(&mut self, row: &TrayRow) {
        self.trays.insert(row.id.clone(), row.clone());
    }

    pub fn tray(&self, id: &str) -> Option<&TrayRow> {
        self.trays.get(id)
    }

    pub fn remember_header(&mut self, section: Section, count: usize, needs: usize) {
        self.headers.insert(section, (count, needs));
    }

    pub fn header(&self, section: Section) -> (usize, usize) {
        self.headers.get(&section).copied().unwrap_or_default()
    }

    fn pending(&self, zone: Zone, now: Instant) -> usize {
        self.arrivals
            .iter()
            .filter(|(to, at)| *to == zone && *at > now)
            .count()
    }

    /// The time the motion runs on: `now`, stretched by [`slowdown`] from the first call on.
    fn dilate(&mut self, now: Instant) -> Instant {
        let slow = *self.slow.get_or_insert_with(slowdown);
        if slow <= 1. {
            return now;
        }
        let epoch = *self.epoch.get_or_insert(now);
        epoch + now.saturating_duration_since(epoch).div_f32(slow)
    }

    pub fn step(&mut self, now: Instant, sight: Sight, reduce: bool) -> Frame {
        let now = self.dilate(now);
        let animate = self.started && !std::mem::take(&mut self.snap);
        self.started = true;
        if !animate {
            self.blocks.clear();
            self.arrivals.clear();
            self.numbers.clear();
        }
        self.arrivals.retain(|(_, at)| *at > now);

        let Sight {
            mut keys,
            zones,
            mut counts,
            snoozed,
            settled,
            panel_open,
        } = sight;
        if animate {
            self.note_arrivals(&keys, &zones, now);
        }
        let folds: Vec<(SheetKind, Counter, usize)> = [
            (SheetKind::Snoozed, Zone::Snoozed, Counter::Snoozed, snoozed),
            (SheetKind::Settled, Zone::Settled, Counter::Settled, settled),
        ]
        .into_iter()
        .map(|(kind, zone, counter, real)| {
            (kind, counter, real.saturating_sub(self.pending(zone, now)))
        })
        .filter(|(_, _, shown)| *shown > 0)
        .collect();
        if keys.contains(&Key::Flight) && !folds.is_empty() {
            keys.push(Key::Sep);
        }
        for (kind, counter, shown) in folds {
            keys.push(Key::Disclosure(kind));
            counts.push((counter, shown));
        }
        if animate {
            self.diff(&keys, &zones, now);
        }
        self.live = keys;
        self.zones = zones;
        let counted = self.count(counts, now);
        let raised = self.raise(panel_open, now);
        let mut frame = self.frame(counted, now, reduce);
        frame.busy |= raised.1;
        frame.raised = raised.0;
        frame
    }

    /// Eases the panel up or down and the dimming in or out, from wherever they are now.
    fn raise(&mut self, open: bool, now: Instant) -> (Raised, bool) {
        let level = if open { 1. } else { 0. };
        let long_ago = now.checked_sub(ms(1000)).unwrap_or(now);
        let lift = self.lift.get_or_insert_with(|| Lift {
            open,
            panel: Tween::at(level, long_ago),
            panel_len: RISE,
            dim: Tween::at(level, long_ago),
        });
        if lift.open != open {
            lift.panel.aim(level, now, lift.panel_len, true);
            lift.panel_len = if open { RISE } else { LOWER };
            lift.dim.aim(level, now, DIM, true);
            lift.open = open;
        }
        let raised = Raised {
            panel: lift.panel.value(now, lift.panel_len),
            dim: lift.dim.value(now, DIM),
        };
        let busy = !lift.panel.settled(now, lift.panel_len) || !lift.dim.settled(now, DIM);
        (raised, busy)
    }

    fn note_arrivals(&mut self, keys: &[Key], zones: &HashMap<SharedString, Zone>, now: Instant) {
        let kept: HashSet<&Key> = keys.iter().collect();
        for key in self.live.iter().filter(|key| !kept.contains(key)) {
            let Some((from, id)) = key.origin() else {
                continue;
            };
            let to = zones.get(id).copied();
            let Some(recipe) = exit_recipe(from, to) else {
                continue;
            };
            if let Some(to @ (Zone::Settled | Zone::Snoozed)) = to {
                let at = now + recipe.wait + recipe.size_wait + recipe.size;
                self.arrivals.push((to, at));
            }
        }
    }

    fn diff(&mut self, keys: &[Key], zones: &HashMap<SharedString, Zone>, now: Instant) {
        let kept: HashSet<&Key> = keys.iter().collect();
        let was: HashSet<Key> = self.live.iter().cloned().collect();

        for key in self.live.clone().iter().filter(|key| !kept.contains(key)) {
            let recipe = match key.origin() {
                Some((from, id)) => exit_recipe(from, zones.get(id).copied()),
                None => Some(BLOCK),
            };
            match recipe {
                Some(recipe) => self
                    .blocks
                    .entry(key.clone())
                    .or_insert_with(|| Block::shown(now))
                    .leave(recipe, now),
                None => {
                    self.blocks.remove(key);
                }
            }
        }

        let mut rows: Vec<(&Key, Option<Recipe>)> = Vec::new();
        for key in keys.iter().filter(|key| !was.contains(key)) {
            if let Some((zone, id)) = key.origin() {
                rows.push((key, enter_recipe(self.zones.get(id).copied(), zone)));
            }
        }
        let row_wait = rows
            .iter()
            .filter_map(|(_, recipe)| recipe.map(|recipe| recipe.wait))
            .min()
            .unwrap_or_default();
        for (key, recipe) in rows {
            self.arrive(key, recipe, now);
        }
        for key in keys.iter().filter(|key| !was.contains(key)) {
            let wait = match key {
                Key::Header(_) | Key::Gap | Key::Flight => row_wait,
                Key::Sep | Key::Disclosure(_) => Duration::ZERO,
                Key::Row(..) | Key::Tray(_) => continue,
            };
            self.arrive(key, Some(Recipe { wait, ..BLOCK }), now);
        }
    }

    fn arrive(&mut self, key: &Key, recipe: Option<Recipe>, now: Instant) {
        match (recipe, self.blocks.get_mut(key)) {
            (Some(recipe), Some(block)) => block.arrive(recipe, now),
            (Some(recipe), None) => {
                let mut block = Block::hidden(now);
                block.arrive(recipe, now);
                self.blocks.insert(key.clone(), block);
            }
            (None, Some(block)) if block.leaving => block.arrive(BLOCK, now),
            (None, _) => {
                self.blocks.remove(key);
            }
        }
    }

    fn count(&mut self, counts: Vec<(Counter, usize)>, now: Instant) -> HashMap<Counter, Counted> {
        let present: HashSet<Counter> = counts.iter().map(|(counter, _)| *counter).collect();
        self.numbers.retain(|counter, _| present.contains(counter));
        counts
            .into_iter()
            .map(|(counter, value)| {
                self.last.insert(counter, value);
                let number = self.numbers.entry(counter).or_insert(Number {
                    value,
                    was: None,
                    mix: Tween::at(1., now),
                });
                if number.value != value {
                    number.was = Some(number.value);
                    number.value = value;
                    number.mix = Tween::at(0., now);
                    number.mix.aim(1., now, COUNT, true);
                }
                if number.mix.settled(now, COUNT) {
                    number.was = None;
                }
                let counted = Counted {
                    value,
                    was: number.was,
                    mix: number.mix.value(now, COUNT),
                };
                (counter, counted)
            })
            .collect()
    }

    fn frame(&mut self, counts: HashMap<Counter, Counted>, now: Instant, reduce: bool) -> Frame {
        self.blocks.retain(|_, block| !block.done(now));
        let live: HashSet<&Key> = self.live.iter().collect();
        let mut order: Vec<Key> = self.live.clone();
        for (at, key) in self.shown.iter().enumerate() {
            if live.contains(key) || !self.blocks.get(key).is_some_and(|block| block.leaving) {
                continue;
            }
            let after = self.shown[..at]
                .iter()
                .rev()
                .filter(|before| before.lane() == key.lane())
                .find_map(|before| order.iter().position(|kept| kept == before))
                .map(|place| place + 1);
            let place = after.unwrap_or_else(|| match key.lane() {
                Lane::List => 0,
                Lane::Dock => order
                    .iter()
                    .position(|kept| kept.lane() == Lane::Dock)
                    .unwrap_or(order.len()),
            });
            order.insert(place, key.clone());
        }
        let items = order
            .iter()
            .map(|key| Item {
                look: self
                    .blocks
                    .get(key)
                    .map_or(Look::STEADY, |block| block.look(now, reduce)),
                ghost: !live.contains(key),
                key: key.clone(),
            })
            .collect();
        self.shown = order;
        let referenced: HashSet<&SharedString> = self
            .live
            .iter()
            .chain(self.blocks.keys())
            .filter_map(Key::thread)
            .collect();
        self.rows.retain(|id, _| referenced.contains(id));
        self.trays.retain(|id, _| referenced.contains(id));
        let busy = !self.blocks.is_empty()
            || self.numbers.values().any(|number| number.was.is_some())
            || !self.arrivals.is_empty();
        Frame {
            items,
            raised: Raised::default(),
            counts,
            last: self.last.clone(),
            busy,
        }
    }
}

/// An element in its place in the list, drawn as far along as `look` says. The box has an id of
/// its own, so a row that leaves in one zone and arrives in another is two elements, each with
/// its own state.
pub fn slot(key: &Key, look: Look, content: AnyElement) -> AnyElement {
    let slot = div().id(key.element_id()).flex_none();
    if look.is_steady() {
        return slot.child(content).into_any_element();
    }
    let content = if look.dx != 0. || look.dy != 0. {
        div()
            .relative()
            .left(px(look.dx))
            .top(px(look.dy))
            .child(content)
            .into_any_element()
    } else {
        content
    };
    let slot = if look.size < 1. {
        slot.h(key.natural() * look.size).overflow_hidden()
    } else {
        slot
    };
    let slot = if look.opacity < 1. {
        slot.opacity(look.opacity)
    } else {
        slot
    };
    slot.child(content).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> SharedString {
        SharedString::from(name.to_string())
    }

    fn read(name: &str) -> Key {
        Key::Row(Zone::Read, id(name))
    }

    fn at(start: Instant, millis: u64) -> Instant {
        start + ms(millis)
    }

    fn sight(rows: &[&str], settled: &[&str]) -> Sight {
        let mut zones: HashMap<SharedString, Zone> =
            rows.iter().map(|name| (id(name), Zone::Read)).collect();
        zones.extend(settled.iter().map(|name| (id(name), Zone::Settled)));
        Sight {
            keys: rows.iter().map(|name| read(name)).collect(),
            zones,
            counts: Vec::new(),
            snoozed: 0,
            settled: settled.len(),
            panel_open: false,
        }
    }

    fn keys(frame: &Frame, lane: Lane) -> Vec<Key> {
        frame.lane(lane).map(|item| item.key.clone()).collect()
    }

    fn look(frame: &Frame, key: &Key) -> Look {
        frame.item(key).unwrap().look
    }

    #[test]
    fn the_first_sight_and_a_snap_jump_to_the_end() {
        let start = Instant::now();
        let mut motion = Motion::default();
        let frame = motion.step(start, sight(&["a", "b"], &[]), false);
        assert!(!frame.busy);
        assert!(frame.items.iter().all(|item| item.look.is_steady()));

        motion.snap();
        let frame = motion.step(at(start, 10), sight(&["a"], &["b"]), false);
        assert_eq!(keys(&frame, Lane::List), vec![read("a")]);
        assert!(!frame.busy);
    }

    #[test]
    fn a_settled_row_fades_and_slides_then_its_gap_closes() {
        let start = Instant::now();
        let mut motion = Motion::default();
        motion.step(start, sight(&["a", "b", "c"], &[]), false);

        let now = at(start, 1000);
        let frame = motion.step(now, sight(&["a", "c"], &["b"]), false);
        assert_eq!(
            keys(&frame, Lane::List),
            vec![read("a"), read("b"), read("c")]
        );
        assert!(frame.busy);
        let ghost = frame.item(&read("b")).unwrap();
        assert!(ghost.ghost);
        assert_eq!(ghost.look, Look::STEADY);
        assert!(!frame.item(&read("a")).unwrap().ghost);

        let frame = motion.step(at(now, 70), sight(&["a", "c"], &["b"]), false);
        let half = look(&frame, &read("b"));
        assert!(half.opacity > 0. && half.opacity < 1., "{half:?}");
        assert!(half.dx > 0. && half.dx < 8., "{half:?}");
        assert_eq!((half.dy, half.size), (0., 1.));

        let frame = motion.step(at(now, 140), sight(&["a", "c"], &["b"]), false);
        let faded = look(&frame, &read("b"));
        assert_eq!((faded.opacity, faded.size), (0., 1.));
        assert!((faded.dx - 8.).abs() < 1e-4);

        let frame = motion.step(at(now, 230), sight(&["a", "c"], &["b"]), false);
        let closing = look(&frame, &read("b"));
        assert!(closing.size > 0. && closing.size < 1., "{closing:?}");

        let frame = motion.step(at(now, 320), sight(&["a", "c"], &["b"]), false);
        assert_eq!(keys(&frame, Lane::List), vec![read("a"), read("c")]);
    }

    #[test]
    fn a_leaving_row_keeps_its_place_among_the_rows_that_stay() {
        let start = Instant::now();
        let mut motion = Motion::default();
        motion.step(start, sight(&["a", "b", "c", "d"], &[]), false);
        let now = at(start, 500);
        motion.step(now, sight(&["a", "c"], &["b", "d"]), false);
        let frame = motion.step(at(now, 50), sight(&["a", "c"], &["b", "d"]), false);
        assert_eq!(
            keys(&frame, Lane::List),
            vec![read("a"), read("b"), read("c"), read("d")]
        );
        let frame = motion.step(at(now, 400), sight(&["a", "c"], &["b", "d"]), false);
        assert_eq!(keys(&frame, Lane::List), vec![read("a"), read("c")]);
    }

    #[test]
    fn reduced_motion_keeps_the_fade_and_drops_the_slide() {
        let start = Instant::now();
        let mut motion = Motion::default();
        motion.step(start, sight(&["a", "b"], &[]), true);
        let now = at(start, 1000);
        motion.step(now, sight(&["a"], &["b"]), true);
        let frame = motion.step(at(now, 70), sight(&["a"], &["b"]), true);
        let half = look(&frame, &read("b"));
        assert!(half.opacity < 1. && half.opacity > 0.);
        assert_eq!((half.dx, half.dy), (0., 0.));
    }

    #[test]
    fn a_header_leaves_with_its_last_row_and_a_gap_follows_it() {
        let start = Instant::now();
        let queue = Key::Row(Zone::Queue, id("q"));
        let header = Key::Header(Section::Queue);
        let with_queue = || {
            let mut sight = sight(&["r"], &[]);
            sight
                .keys
                .splice(0..0, [header.clone(), queue.clone(), Key::Gap]);
            sight.zones.insert(id("q"), Zone::Queue);
            sight
        };
        let mut motion = Motion::default();
        motion.step(start, with_queue(), false);

        let now = at(start, 1000);
        let mut after = sight(&["r"], &[]);
        after.zones.insert(id("q"), Zone::Settled);
        after.settled = 1;
        motion.step(now, after.clone(), false);
        let frame = motion.step(at(now, 90), after.clone(), false);
        let head = look(&frame, &header);
        assert!(frame.item(&header).unwrap().ghost);
        assert!(head.opacity < 1. && head.size < 1., "{head:?}");
        assert!(frame.item(&Key::Gap).unwrap().ghost);
        assert_eq!(look(&frame, &queue).size, 1.);

        let frame = motion.step(at(now, 200), after, false);
        assert!(frame.item(&header).is_none());
        assert!(frame.item(&Key::Gap).is_none());
    }

    #[test]
    fn the_dock_counts_a_settled_row_only_when_it_gets_there() {
        let start = Instant::now();
        let settled_row = Key::Disclosure(SheetKind::Settled);
        let mut motion = Motion::default();
        let frame = motion.step(start, sight(&["a", "b"], &[]), false);
        assert!(frame.item(&settled_row).is_none());

        let now = at(start, 1000);
        let frame = motion.step(now, sight(&["a"], &["b"]), false);
        assert!(
            frame.item(&settled_row).is_none(),
            "the row is still leaving"
        );

        let frame = motion.step(at(now, 310), sight(&["a"], &["b"]), false);
        assert!(frame.item(&settled_row).is_none());

        let frame = motion.step(at(now, 330), sight(&["a"], &["b"]), false);
        let arrived = frame.item(&settled_row).unwrap();
        assert!(!arrived.ghost);
        assert_eq!(frame.counted(Counter::Settled), Counted::steady(1));
        assert!(arrived.look.opacity < 1. || arrived.look.size < 1.);
    }

    #[test]
    fn a_count_crossfades_from_the_old_value_to_the_new() {
        let start = Instant::now();
        let mut motion = Motion::default();
        let mut one = sight(&[], &[]);
        one.counts = vec![(Counter::Queue, 3)];
        let frame = motion.step(start, one.clone(), false);
        assert_eq!(frame.counted(Counter::Queue), Counted::steady(3));

        let mut two = one.clone();
        two.counts = vec![(Counter::Queue, 2)];
        let now = at(start, 100);
        motion.step(now, two.clone(), false);
        let frame = motion.step(at(now, 40), two.clone(), false);
        let mid = frame.counted(Counter::Queue);
        assert_eq!((mid.value, mid.was), (2, Some(3)));
        assert!(mid.mix > 0. && mid.mix < 1.);
        assert!(frame.busy);

        let frame = motion.step(at(now, 130), two, false);
        assert_eq!(frame.counted(Counter::Queue), Counted::steady(2));
        assert!(!frame.busy);
    }

    #[test]
    fn a_row_that_comes_back_while_it_leaves_turns_around() {
        let start = Instant::now();
        let mut motion = Motion::default();
        motion.step(start, sight(&["a", "b"], &[]), false);
        let now = at(start, 1000);
        motion.step(now, sight(&["a"], &["b"]), false);
        let frame = motion.step(at(now, 100), sight(&["a"], &["b"]), false);
        let fading = look(&frame, &read("b")).opacity;
        assert!(fading < 1.);

        let back = at(now, 100);
        let frame = motion.step(back, sight(&["a", "b"], &[]), false);
        let item = frame.item(&read("b")).unwrap();
        assert!(!item.ghost);
        assert!((item.look.opacity - fading).abs() < 1e-4);
        let frame = motion.step(at(back, 400), sight(&["a", "b"], &[]), false);
        assert_eq!(look(&frame, &read("b")), Look::STEADY);
        assert!(!frame.busy);
    }

    #[test]
    fn what_a_leaving_row_showed_is_kept_until_it_is_gone() {
        let start = Instant::now();
        let mut motion = Motion::default();
        let row = Row {
            id: id("b"),
            title: "Title".into(),
            dot: crate::sidebar::Dot::None,
            detail: "Done".to_string(),
            attention: false,
            time: None,
            in_copy: false,
            strong: false,
            selected: false,
            archivable: true,
            ghost: false,
        };
        motion.step(start, sight(&["a", "b"], &[]), false);
        motion.remember_row(&row);
        let now = at(start, 1000);
        motion.step(now, sight(&["a"], &["b"]), false);
        assert_eq!(motion.row("b").unwrap().title, "Title");
        motion.step(at(now, 400), sight(&["a"], &["b"]), false);
        assert!(motion.row("b").is_none());
    }

    fn placed(keys: Vec<Key>, zones: &[(&str, Zone)]) -> Sight {
        Sight {
            keys,
            zones: zones.iter().map(|(name, zone)| (id(name), *zone)).collect(),
            ..Sight::default()
        }
    }

    fn tray(name: &str) -> Key {
        Key::Tray(id(name))
    }

    #[test]
    fn switching_away_from_a_working_thread_folds_its_row_into_the_tray() {
        let start = Instant::now();
        let before = || {
            placed(
                vec![read("w"), read("x")],
                &[("w", Zone::Read), ("x", Zone::Read)],
            )
        };
        let after = || {
            let mut sight = placed(
                vec![read("x"), Key::Flight, tray("w")],
                &[("w", Zone::Tray), ("x", Zone::Read)],
            );
            sight.counts = vec![(Counter::Flight, 1)];
            sight
        };
        let mut motion = Motion::default();
        motion.step(start, before(), false);

        let now = at(start, 1000);
        let frame = motion.step(now, after(), false);
        assert!(frame.item(&read("w")).unwrap().ghost);
        assert_eq!(keys(&frame, Lane::Dock), vec![Key::Flight, tray("w")]);
        assert_eq!(look(&frame, &tray("w")).opacity, 0.);

        let frame = motion.step(at(now, 70), after(), false);
        let sinking = look(&frame, &read("w"));
        assert!(sinking.dy > 0. && sinking.dy < 6. && sinking.opacity < 1.);
        let arriving = look(&frame, &tray("w"));
        assert!(arriving.opacity > 0. && arriving.size > 0.);
        assert!(look(&frame, &Key::Flight).opacity > 0.);

        let frame = motion.step(at(now, 400), after(), false);
        assert_eq!(keys(&frame, Lane::List), vec![read("x")]);
        assert_eq!(look(&frame, &tray("w")), Look::STEADY);
        assert!(!frame.busy);
    }

    #[test]
    fn a_landing_fades_the_tray_row_then_opens_room_for_the_row_in_the_queue() {
        let start = Instant::now();
        let queue = Key::Row(Zone::Queue, id("w"));
        let header = Key::Header(Section::Queue);
        let before = || {
            let mut sight = placed(vec![Key::Flight, tray("w")], &[("w", Zone::Tray)]);
            sight.keys = vec![Key::Flight, tray("w")];
            sight
        };
        let after = || placed(vec![header.clone(), queue.clone()], &[("w", Zone::Queue)]);
        let mut motion = Motion::default();
        motion.step(start, before(), false);

        let now = at(start, 1000);
        let frame = motion.step(now, after(), false);
        assert_eq!(look(&frame, &queue).opacity, 0.);
        assert_eq!(look(&frame, &queue).size, 0.);
        assert!(frame.item(&tray("w")).unwrap().ghost);
        assert!(frame.item(&Key::Flight).unwrap().ghost);

        let frame = motion.step(at(now, 100), after(), false);
        assert_eq!(
            look(&frame, &queue).size,
            0.,
            "it waits for the tray row to fade"
        );
        assert_eq!(look(&frame, &header).size, 0.);
        assert!(look(&frame, &tray("w")).opacity < 1.);

        let frame = motion.step(at(now, 200), after(), false);
        let growing = look(&frame, &queue);
        assert!(growing.size > 0. && growing.opacity > 0. && growing.opacity < 1.);
        assert!(growing.dy < 0. && growing.dy > -4.);
        assert!(growing.mark > 0. && growing.mark < 1.);
        assert_eq!(look(&frame, &tray("w")).opacity, 0.);
        assert!(look(&frame, &header).size > 0.);

        let frame = motion.step(at(now, 500), after(), false);
        assert_eq!(look(&frame, &queue), Look::STEADY);
        assert!(frame.item(&tray("w")).is_none());
        assert!(!frame.busy);
    }

    #[test]
    fn reading_a_row_in_the_queue_moves_it_to_the_read_list_after_its_gap_starts_to_close() {
        let start = Instant::now();
        let queue = Key::Row(Zone::Queue, id("q"));
        let mut motion = Motion::default();
        motion.step(
            start,
            placed(vec![queue.clone()], &[("q", Zone::Queue)]),
            false,
        );
        let now = at(start, 1000);
        let after = || placed(vec![read("q")], &[("q", Zone::Read)]);
        let frame = motion.step(now, after(), false);
        assert!(frame.item(&queue).unwrap().ghost);
        assert_eq!(look(&frame, &read("q")).size, 0.);

        let frame = motion.step(at(now, 100), after(), false);
        assert_eq!(look(&frame, &read("q")).size, 0.);
        assert!(look(&frame, &queue).opacity < 1.);
        assert_eq!(look(&frame, &queue).dx, 0.);

        let frame = motion.step(at(now, 200), after(), false);
        let arriving = look(&frame, &read("q"));
        assert!(arriving.size > 0. && arriving.mark == 1.);
    }

    #[test]
    fn a_thread_seen_for_the_first_time_appears_at_once() {
        let start = Instant::now();
        let mut motion = Motion::default();
        motion.step(start, placed(vec![read("a")], &[("a", Zone::Read)]), false);
        let frame = motion.step(
            at(start, 50),
            placed(
                vec![Key::Row(Zone::Draft, id("d")), read("a")],
                &[("a", Zone::Read), ("d", Zone::Draft)],
            ),
            false,
        );
        assert!(frame.items.iter().all(|item| item.look.is_steady()));
        assert!(!frame.busy);
    }

    #[test]
    fn the_rule_shows_with_the_tray_and_the_rows_under_it() {
        let start = Instant::now();
        let mut motion = Motion::default();
        let mut sight = placed(vec![Key::Flight, tray("w")], &[("w", Zone::Tray)]);
        sight.settled = 2;
        let frame = motion.step(start, sight, false);
        assert_eq!(
            keys(&frame, Lane::Dock),
            vec![
                Key::Flight,
                tray("w"),
                Key::Sep,
                Key::Disclosure(SheetKind::Settled)
            ]
        );
        let mut sight = placed(vec![], &[("w", Zone::Read)]);
        sight.settled = 2;
        let frame = motion.step(at(start, 10), sight, false);
        assert_eq!(
            frame.lane(Lane::Dock).filter(|item| !item.ghost).count(),
            1,
            "the rule leaves with the tray"
        );
    }

    #[test]
    fn a_snoozed_thread_that_wakes_fades_in_with_no_wait() {
        let start = Instant::now();
        let mut motion = Motion::default();
        let mut asleep = placed(vec![], &[("s", Zone::Snoozed)]);
        asleep.snoozed = 1;
        motion.step(start, asleep, false);
        let now = at(start, 1000);
        let frame = motion.step(now, placed(vec![read("s")], &[("s", Zone::Read)]), false);
        assert_eq!(look(&frame, &read("s")).size, 0.);
        let frame = motion.step(
            at(now, 100),
            placed(vec![read("s")], &[("s", Zone::Read)]),
            false,
        );
        assert!(look(&frame, &read("s")).size > 0.);
    }

    #[test]
    fn the_panel_rises_in_220ms_and_lowers_in_160_while_everything_behind_dims_in_220_both_ways() {
        let start = Instant::now();
        let mut motion = Motion::default();
        let closed = || placed(vec![], &[]);
        let open = || Sight {
            panel_open: true,
            ..closed()
        };
        let frame = motion.step(start, closed(), false);
        assert_eq!(frame.raised, Raised::default());
        assert!(!frame.busy);

        let now = at(start, 1000);
        let frame = motion.step(now, open(), false);
        assert_eq!(frame.raised, Raised::default());
        assert!(frame.busy);
        let frame = motion.step(at(now, 100), open(), false);
        assert!(frame.raised.panel > 0. && frame.raised.panel < 1.);
        assert!(frame.raised.dim > 0. && frame.raised.dim < 1.);
        let frame = motion.step(at(now, 220), open(), false);
        assert_eq!(frame.raised, Raised { panel: 1., dim: 1. });
        assert!(!frame.busy);

        let now = at(now, 1000);
        motion.step(now, closed(), false);
        let frame = motion.step(at(now, 160), closed(), false);
        assert_eq!(frame.raised.panel, 0.);
        assert!(frame.raised.dim > 0., "the dimming takes 220ms");
        assert!(frame.busy);
        let frame = motion.step(at(now, 220), closed(), false);
        assert_eq!(frame.raised, Raised::default());
        assert!(!frame.busy);
    }

    #[test]
    fn closing_a_panel_half_way_up_lowers_it_from_there() {
        let start = Instant::now();
        let mut motion = Motion::default();
        let sight = |panel_open| Sight {
            panel_open,
            ..Sight::default()
        };
        motion.step(start, sight(false), false);
        let now = at(start, 1000);
        motion.step(now, sight(true), false);
        let seen = motion.step(at(now, 80), sight(true), false).raised.panel;
        let lowered = motion.step(at(now, 80), sight(false), false).raised.panel;
        assert!((lowered - seen).abs() < 1e-4);
        let later = motion.step(at(now, 120), sight(false), false).raised.panel;
        assert!(later < lowered);
    }

    #[test]
    fn a_slowdown_stretches_the_time_the_motion_runs_on() {
        let start = Instant::now();
        let mut motion = Motion {
            slow: Some(5.),
            ..Motion::default()
        };
        assert_eq!(motion.dilate(start), start);
        assert_eq!(motion.dilate(at(start, 500)), at(start, 100));
        let mut normal = Motion {
            slow: Some(1.),
            ..Motion::default()
        };
        assert_eq!(normal.dilate(at(start, 500)), at(start, 500));
    }
}
