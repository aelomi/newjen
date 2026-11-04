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
use newjen_core::steps::Placement as ThreadPlacement;

use super::dock::{self, SheetKind};
use super::{READ_GAP, ROW_HEIGHT, Row, Section};
use crate::ui::Tween;

const fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// How long a count takes to crossfade into its new value.
const COUNT: Duration = ms(120);

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
    pub fn of(draft: bool, placement: ThreadPlacement) -> Self {
        if draft {
            return Self::Draft;
        }
        match placement {
            ThreadPlacement::Pinned => Self::Pinned,
            ThreadPlacement::Queue => Self::Queue,
            ThreadPlacement::Read => Self::Read,
            ThreadPlacement::InFlight => Self::Tray,
            ThreadPlacement::Snoozed => Self::Snoozed,
            ThreadPlacement::Settled => Self::Settled,
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
    Disclosure(SheetKind),
}

impl Key {
    pub fn lane(&self) -> Lane {
        match self {
            Self::Header(_) | Self::Gap | Self::Row(..) => Lane::List,
            Self::Disclosure(_) => Lane::Dock,
        }
    }

    /// The height the element has when it is fully there.
    pub fn natural(&self) -> Pixels {
        match self {
            Self::Header(_) => HEADER_HEIGHT,
            Self::Gap => READ_GAP,
            Self::Row(..) => ROW_HEIGHT,
            Self::Disclosure(_) => dock::ROW,
        }
    }

    fn thread(&self) -> Option<&SharedString> {
        match self {
            Self::Row(_, id) => Some(id),
            Self::Header(_) | Self::Gap | Self::Disclosure(_) => None,
        }
    }

    fn element_id(&self) -> ElementId {
        let name = match self {
            Self::Header(section) => format!("slot-header-{section:?}"),
            Self::Gap => "slot-gap".to_string(),
            Self::Row(zone, id) => format!("slot-row-{zone:?}-{id}"),
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
}

/// Settling or snoozing a row: it fades and slides 8px right, then the gap closes.
const LEAVE: Recipe = Recipe {
    wait: ms(0),
    fade: ms(140),
    size_wait: ms(140),
    size: ms(180),
    shift: (8., 0.),
};

/// A section header, the gap above the read list and a dock row grow and fade together.
const BLOCK: Recipe = Recipe {
    wait: ms(0),
    fade: ms(180),
    size_wait: ms(0),
    size: ms(180),
    shift: (0., 0.),
};

/// What a leaving row does, given where it goes. `None` when it just disappears.
fn exit_recipe(from: Zone, to: Option<Zone>) -> Option<Recipe> {
    match (from.in_list(), to) {
        (true, Some(Zone::Settled | Zone::Snoozed)) => Some(LEAVE),
        _ => None,
    }
}

/// What an arriving row does, given where it came from. `None` when it just appears.
fn enter_recipe(_from: Option<Zone>, _to: Zone) -> Option<Recipe> {
    None
}

/// One key's progress: its opacity and its height fade on their own clocks.
#[derive(Clone, Copy, Debug)]
struct Block {
    fade: Tween<f32>,
    fade_len: Duration,
    size: Tween<f32>,
    size_len: Duration,
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
    }

    fn done(&self, now: Instant) -> bool {
        self.fade.settled(now, self.fade_len) && self.size.settled(now, self.size_len)
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
}

impl Look {
    pub const STEADY: Self = Self {
        opacity: 1.,
        size: 1.,
        dx: 0.,
        dy: 0.,
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

/// What a render draws.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub items: Vec<Item>,
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
}

struct Number {
    value: usize,
    was: Option<usize>,
    mix: Tween<f32>,
}

#[derive(Default)]
pub struct Motion {
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
    rows: HashMap<SharedString, Row>,
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

    pub fn step(&mut self, now: Instant, sight: Sight, reduce: bool) -> Frame {
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
        } = sight;
        if animate {
            self.note_arrivals(&keys, &zones, now);
        }
        for (kind, zone, counter, real) in [
            (SheetKind::Snoozed, Zone::Snoozed, Counter::Snoozed, snoozed),
            (SheetKind::Settled, Zone::Settled, Counter::Settled, settled),
        ] {
            let shown = real.saturating_sub(self.pending(zone, now));
            if shown > 0 {
                keys.push(Key::Disclosure(kind));
                counts.push((counter, shown));
            }
        }
        if animate {
            self.diff(&keys, &zones, now);
        }
        self.live = keys;
        self.zones = zones;
        let counted = self.count(counts, now);
        self.frame(counted, now, reduce)
    }

    fn note_arrivals(&mut self, keys: &[Key], zones: &HashMap<SharedString, Zone>, now: Instant) {
        let kept: HashSet<&Key> = keys.iter().collect();
        for key in self.live.iter().filter(|key| !kept.contains(key)) {
            let Key::Row(from, id) = key else { continue };
            let to = zones.get(id).copied();
            let Some(recipe) = exit_recipe(*from, to) else {
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
            let recipe = match key {
                Key::Row(from, id) => exit_recipe(*from, zones.get(id).copied()),
                Key::Header(_) | Key::Gap | Key::Disclosure(_) => Some(BLOCK),
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
            if let Key::Row(zone, id) = key {
                rows.push((key, enter_recipe(self.zones.get(id).copied(), *zone)));
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
                Key::Header(_) | Key::Gap => row_wait,
                Key::Disclosure(_) => Duration::ZERO,
                Key::Row(..) => continue,
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
        let busy = !self.blocks.is_empty()
            || self.numbers.values().any(|number| number.was.is_some())
            || !self.arrivals.is_empty();
        Frame {
            items,
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
        };
        motion.step(start, sight(&["a", "b"], &[]), false);
        motion.remember_row(&row);
        let now = at(start, 1000);
        motion.step(now, sight(&["a"], &["b"]), false);
        assert_eq!(motion.row("b").unwrap().title, "Title");
        motion.step(at(now, 400), sight(&["a"], &["b"]), false);
        assert!(motion.row("b").is_none());
    }
}
