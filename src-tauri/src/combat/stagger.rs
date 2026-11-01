//! A boss's stagger gauge, and who broke it.
//!
//! The server sends the gauge's state after every hit that takes stagger off
//! it (`StreamProcessor::parse_stagger_gauge_packet`, `05 E0`), and the
//! gauge's size in the boss's spawn (`41 36`, its last 15 bytes). Nothing per
//! skill or per boss is needed: what a hit took off is the gauge before it
//! less the gauge after it.
//!
//! The gauge counts down. A window opens when the gauge becomes active (its
//! first update with status `02`/`03`) or with its first deduction, whichever
//! comes first, and ends with a break (`00 03 02`), or as missed: a reset to
//! full without a break, the boss dying or leaving, or the fight ending.
//!
//! Each deduction goes to the hit that caused it. The server sends the hits
//! on an entity as a batch, the `04 38` records with their values and their
//! effects, and then the gauge's new state, one `05 E0` per hit that took
//! stagger, in the hits' order:
//!
//! - a hit that takes stagger comes with entries of its own skill and actor
//!   that carry no value (switch 0): Punishment two, Pummel's first strike
//!   one. Procs never have them (Punishing Benediction 0 of 78 times,
//!   Theostones, Flash Rampage). These hits, "marked", take the batch's
//!   deductions in turn. Solo the update follows its hit, but a proc can land
//!   in between: Poach, then Punishing Benediction, then the gauge 104 to 94
//!   (Shaman Kumbahum, 2026-10-22). In a party several players' hits come
//!   first: Judgment and a Chanter's hit, then the gauge 947 to 942 and 940
//!   (Enhanced Harcon, the same evening, five players). Over that dungeon's
//!   789 deductions each skill then took one value every time (Punishment
//!   35, Pummel 3, as solo), but for the last of a break, which takes what is
//!   left;
//! - with no marked hit left, the batch's last hit not yet taken does, and
//!   when the batch has hits of more than one actor that is `ambiguous`;
//! - a DoT tick (`05 38`) counts only when the batch has no hit: `05 38` is
//!   also an effect inside a hit's block, and must not take a deduction;
//! - a deduction with no hit (a lost packet, an opcode not read) goes to
//!   actor 0, "unknown", so a window's deductions always add up to what the
//!   gauge lost.
//!
//! The next hit after an update starts a new batch: a marked hit that took
//! nothing (the gauge locked, before a mechanic boss's phase) is dropped.
//!
//! The status byte is logged but decides nothing beyond when a window
//! opens: its meaning is read from three monsters only. See `note_gauge`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A batch whose last hit is older than this when the gauge moves belongs to
/// a fight that has ended: the meter's own idle gap between two fights.
const PENDING_HIT_MAX_AGE_MS: i64 = 30_000;
/// At most this many hits are kept per batch (a party's batch has a dozen).
const MAX_BATCH_HITS: usize = 64;
/// How long a closed window is kept: long enough for any fight to be saved
/// again, short enough that a meter left running all day holds little.
const WINDOW_RETENTION_MS: i64 = 30 * 60 * 1000;

/// What a window ended with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    /// The gauge broke (`05 E0 <entity> 00 03 02`).
    Break,
    /// Reset to full without a break, the entity died or left, or the fight
    /// ended with the window still open.
    Missed,
}

/// Why a window ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum End {
    Break,
    /// Back to full without a break (a wipe).
    Reset,
    /// The entity died (`42 36` flag 3): beaten before the gauge broke.
    Death,
    /// The entity left the world (`42 36` flag 7).
    Despawn,
    /// Still open when the fight ended (or closed only after it).
    FightEnd,
}

/// One deduction from a gauge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub ms: i64,
    /// The entity that hit, as the record names it (a summon is resolved to
    /// its owner when the fight is saved); 0 when the batch had none.
    pub actor: i32,
    /// The skill id as the hit record carries it (12090243 for Punishment),
    /// 0 with no hit.
    pub skill: i32,
    pub value: u32,
    pub ambiguous: bool,
}

/// One window of a gauge, from becoming active to its break or its miss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub entity: i32,
    pub max: u32,
    pub start_ms: i64,
    /// `None` while open.
    pub end_ms: Option<i64>,
    /// Why it ended; `None` while open.
    pub ended: Option<End>,
    /// What was left of the gauge when it was missed; 0 on a break.
    pub missing: u32,
    /// The gauge's starting value was not seen (no spawn, and its first
    /// update already below full): a deduction before it is lost.
    pub incomplete: bool,
    pub hits: Vec<Hit>,
    /// The gauge's value at each change while open, for its value at a
    /// fight's end.
    rests: Vec<(i64, u32)>,
}

impl Window {
    /// Broken or missed; `None` while open.
    pub fn outcome(&self) -> Option<Outcome> {
        self.ended.map(|e| if e == End::Break { Outcome::Break } else { Outcome::Missed })
    }

    /// The gauge's value at `ms`, as last known.
    fn rest_at(&self, ms: i64) -> u32 {
        self.rests.iter().take_while(|r| r.0 <= ms).last().or(self.rests.first()).map_or(self.max, |r| r.1)
    }
}

#[derive(Debug, Clone)]
struct Gauge {
    max: u32,
    rest: u32,
    /// Broken and not yet refilled.
    broken: bool,
    /// The gauge's value has been seen in full (its spawn, or an update at
    /// `max`), so no deduction before the next one was lost.
    baseline: bool,
    open: Option<Window>,
}

#[derive(Debug, Clone, Copy)]
struct PendingHit {
    ms: i64,
    actor: i32,
    skill: i32,
    ambiguous: bool,
}

/// A hit in a batch.
#[derive(Debug, Clone, Copy)]
struct BatchHit {
    hit: PendingHit,
    dot: bool,
    /// Entries of its own with no value followed it: it took stagger.
    marked: bool,
    /// A deduction has gone to it.
    taken: bool,
}

/// The hits on an entity since its gauge last moved (see the module's notes).
#[derive(Debug, Clone, Default)]
struct Pending {
    hits: Vec<BatchHit>,
    /// An update came after the last hit: the next hit starts a new batch.
    updated: bool,
}

impl Pending {
    /// The hit a deduction goes to: the first marked one not yet taken, else
    /// the last one not yet taken.
    fn take(&mut self) -> Option<PendingHit> {
        if let Some(h) = self.hits.iter_mut().find(|h| h.marked && !h.taken) {
            h.taken = true;
            return Some(h.hit);
        }
        let first_actor = self.hits.iter().find(|h| !h.dot).map(|h| h.hit.actor);
        let several = self.hits.iter().any(|h| !h.dot && Some(h.hit.actor) != first_actor);
        let h = self.hits.iter_mut().rev().find(|h| !h.taken)?;
        h.taken = true;
        Some(PendingHit { ambiguous: several, ..h.hit })
    }
}

/// Every gauge the meter knows of, the hits waiting for their update, and
/// the windows.
#[derive(Debug, Default)]
pub struct StaggerLog {
    gauges: HashMap<i32, Gauge>,
    pending: HashMap<i32, Pending>,
    closed: Vec<Window>,
}

impl StaggerLog {
    /// The gauge in an entity's spawn: its starting value. It opens no
    /// window (a mechanic boss's gauge is in its spawn, locked, until its
    /// phase begins) and moves an open one by nothing.
    pub fn note_spawn(&mut self, entity: i32, max: u32, rest: u32) {
        match self.gauges.get_mut(&entity) {
            Some(g) => {
                g.max = max;
                g.rest = rest;
                g.baseline = true;
            }
            None => {
                self.gauges.insert(entity, Gauge { max, rest, broken: false, baseline: true, open: None });
            }
        }
    }

    /// A hit with a value on `entity`, or a DoT tick (`dot`). Only on an
    /// entity whose gauge is known: on any other the first update only sets
    /// the starting value, so nothing would take it.
    pub fn note_hit(&mut self, entity: i32, actor: i32, skill: i32, ms: i64, dot: bool) {
        if !self.gauges.contains_key(&entity) {
            return;
        }
        let p = self.pending.entry(entity).or_default();
        let stale = p.hits.last().is_some_and(|h| ms - h.hit.ms > PENDING_HIT_MAX_AGE_MS);
        if p.updated || stale {
            *p = Pending::default();
        }
        if dot && !p.hits.is_empty() {
            return;
        }
        if p.hits.len() >= MAX_BATCH_HITS {
            p.hits.remove(0);
        }
        let hit = PendingHit { ms, actor, skill, ambiguous: false };
        p.hits.push(BatchHit { hit, dot, marked: false, taken: false });
    }

    /// An entry of a hit with no value (`04 38` switch 0): the batch's last
    /// hit of its actor and skill is one that took stagger.
    pub fn note_hit_entry(&mut self, entity: i32, actor: i32, skill: i32) {
        if let Some(p) = self.pending.get_mut(&entity)
            && let Some(h) = p.hits.iter_mut().rev().find(|h| !h.dot && h.hit.actor == actor && h.hit.skill == skill)
        {
            h.marked = true;
        }
    }

    /// A gauge update, `05 E0 <entity> 03 01 <max> <rest> <status>`.
    ///
    /// | update                            | meaning                              |
    /// |-----------------------------------|--------------------------------------|
    /// | `rest` falls, `max` unchanged     | a deduction, to the batch's next hit |
    /// | `rest` back to `max` after break  | refilled: no stagger, no window yet  |
    /// | `rest` back to `max`, no break    | reset: the open window is missed     |
    /// | `max` changes (60/40 after a wipe)| a new starting value, no deduction   |
    /// | unchanged, status `02`/`03`       | the gauge is active: a window opens  |
    pub fn note_gauge(&mut self, entity: i32, max: u32, rest: u32, status: u8, ms: i64) {
        if let Some(p) = self.pending.get_mut(&entity) {
            p.updated = true;
        }
        let active = matches!(status, 2 | 3);
        let Some(g) = self.gauges.get_mut(&entity) else {
            // No spawn seen: this sets the starting value. Below full, what
            // was taken before it is lost.
            let mut g = Gauge { max, rest, broken: false, baseline: rest >= max, open: None };
            if active || rest < max {
                g.open = Some(open_window(entity, max, rest, ms, rest < max));
            }
            self.gauges.insert(entity, g);
            return;
        };
        if max != g.max {
            g.max = max;
            g.rest = rest;
            g.baseline = rest >= max;
            if let Some(w) = g.open.as_mut() {
                w.max = max;
                w.rests.push((ms, rest));
            }
            return;
        }
        if rest < g.rest {
            let hit = take_hit(&mut self.pending, entity, ms);
            let value = g.rest - rest;
            let incomplete = !g.baseline;
            let w = g.open.get_or_insert_with(|| open_window(entity, max, g.rest, ms, incomplete));
            w.hits.push(hit_of(hit, ms, value));
            w.rests.push((ms, rest));
            g.rest = rest;
            g.broken = false;
            return;
        }
        if rest > g.rest {
            let refilled = g.broken;
            g.rest = rest;
            g.broken = false;
            if rest >= max {
                g.baseline = true;
                if !refilled && let Some(mut w) = g.open.take() {
                    let left = w.rests.last().map_or(w.max, |r| r.1);
                    close(&mut w, ms, End::Reset, left);
                    self.push_closed(w, ms);
                }
            } else if let Some(w) = g.open.as_mut() {
                w.rests.push((ms, rest));
            }
            return;
        }
        if active && g.open.is_none() && !g.broken {
            let incomplete = !g.baseline;
            g.open = Some(open_window(entity, max, rest, ms, incomplete));
        }
    }

    /// The gauge broke, `05 E0 <entity> 00 03 02`: the batch's next hit
    /// (`Pending::take`) took what was left.
    pub fn note_break(&mut self, entity: i32, ms: i64) {
        let hit = take_hit(&mut self.pending, entity, ms);
        let Some(g) = self.gauges.get_mut(&entity) else {
            return;
        };
        let incomplete = !g.baseline;
        let mut w = g.open.take().unwrap_or_else(|| open_window(entity, g.max, g.rest, ms, incomplete));
        if g.rest > 0 {
            w.hits.push(hit_of(hit, ms, g.rest));
        }
        w.rests.push((ms, 0));
        close(&mut w, ms, End::Break, 0);
        g.rest = 0;
        g.broken = true;
        self.push_closed(w, ms);
    }

    /// The entity died (`died`) or left: an open window is missed, and the
    /// gauge is forgotten with it.
    pub fn note_gone(&mut self, entity: i32, ms: i64, died: bool) {
        self.pending.remove(&entity);
        if let Some(mut g) = self.gauges.remove(&entity)
            && let Some(mut w) = g.open.take()
        {
            close(&mut w, ms, if died { End::Death } else { End::Despawn }, g.rest);
            self.push_closed(w, ms);
        }
    }

    /// The fight ended: no batch carries over into the next.
    pub fn forget_pending(&mut self) {
        self.pending.clear();
    }

    /// Everything, for a capture from another session.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The gauge's (max, rest), if known.
    pub fn gauge(&self, entity: i32) -> Option<(u32, u32)> {
        self.gauges.get(&entity).map(|g| (g.max, g.rest))
    }

    /// The window open on `entity`, if any.
    pub fn open_window(&self, entity: i32) -> Option<&Window> {
        self.gauges.get(&entity).and_then(|g| g.open.as_ref())
    }

    /// Every window, closed and open, with a deduction from `start_ms` to
    /// `end_ms`, as the fight in that span saw them: a window still open at
    /// `end_ms`, or closed after it, is missed at `end_ms` with what was
    /// left of its gauge then, and only its hits up to `end_ms` are kept. The
    /// state here is not changed: a fight is saved again while it goes on.
    pub fn windows_between(&self, start_ms: i64, end_ms: i64) -> Vec<Window> {
        let open = self.gauges.values().filter_map(|g| g.open.as_ref());
        let mut out: Vec<Window> = self
            .closed
            .iter()
            .chain(open)
            .filter(|w| w.hits.iter().any(|h| h.ms >= start_ms && h.ms <= end_ms))
            .map(|w| {
                let mut w = w.clone();
                if w.end_ms.is_none_or(|e| e > end_ms) {
                    w.hits.retain(|h| h.ms <= end_ms);
                    let left = w.rest_at(end_ms);
                    close(&mut w, end_ms, End::FightEnd, left);
                }
                w
            })
            .collect();
        out.sort_by_key(|w| (w.start_ms, w.entity));
        out
    }

    fn push_closed(&mut self, w: Window, ms: i64) {
        self.closed.retain(|c| c.end_ms.is_some_and(|e| e >= ms - WINDOW_RETENTION_MS));
        self.closed.push(w);
    }
}

/// The hit a deduction from `entity`'s gauge goes to (`Pending::take`); none
/// when its batch is older than a fight's idle gap.
fn take_hit(pending: &mut HashMap<i32, Pending>, entity: i32, ms: i64) -> Option<PendingHit> {
    let p = pending.get_mut(&entity)?;
    p.updated = true;
    if p.hits.last().is_some_and(|h| ms - h.hit.ms > PENDING_HIT_MAX_AGE_MS) {
        return None;
    }
    p.take()
}

fn open_window(entity: i32, max: u32, rest: u32, ms: i64, incomplete: bool) -> Window {
    Window {
        entity,
        max,
        start_ms: ms,
        end_ms: None,
        ended: None,
        missing: 0,
        incomplete,
        hits: Vec::new(),
        rests: vec![(ms, rest)],
    }
}

fn close(w: &mut Window, ms: i64, ended: End, left: u32) {
    w.end_ms = Some(ms);
    w.ended = Some(ended);
    w.missing = if ended == End::Break { 0 } else { left };
}

fn hit_of(hit: Option<PendingHit>, ms: i64, value: u32) -> Hit {
    match hit {
        Some(p) => Hit { ms: p.ms, actor: p.actor, skill: p.skill, value, ambiguous: p.ambiguous },
        None => Hit { ms, actor: 0, skill: 0, value, ambiguous: false },
    }
}

/// A window as a fight in History keeps it (`FightRecord::stagger`). Times
/// in ms from the fight's start, as `fight_buffs` keeps them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaggerWindow {
    pub entity: i32,
    /// The entity's NPC code, 0 when not known.
    #[serde(default)]
    pub mob_code: i32,
    pub max: u32,
    pub start: i64,
    pub end: i64,
    pub result: Outcome,
    /// Why it ended: broken, or missed as its gauge was reset, its entity
    /// died or left, or the fight ended first.
    pub ended: End,
    /// What was left of the gauge when it was missed; 0 on a break.
    #[serde(default)]
    pub missing: u32,
    /// The gauge's starting value was not seen: a deduction before the
    /// first update is not in `hits`.
    #[serde(default, skip_serializing_if = "crate::combat::fight_buffs::is_false")]
    pub incomplete: bool,
    pub hits: Vec<StaggerHit>,
}

/// One deduction in a `StaggerWindow`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaggerHit {
    /// When the hit landed, in ms from the fight's start.
    pub t: i64,
    /// Who hit, a summon resolved to its owner; 0 when not known.
    pub actor: i32,
    /// The skill id as the hit record carries it; 0 when not known.
    pub skill: i32,
    pub value: u32,
    /// No hit of the batch was marked as taking stagger, so the batch's last
    /// went first, and the batch had hits of more than one actor.
    #[serde(default, skip_serializing_if = "crate::combat::fight_buffs::is_false")]
    pub ambiguous: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deduction_goes_to_the_batchs_hit_and_a_reset_misses_the_window() {
        let mut log = StaggerLog::default();
        log.note_spawn(1, 40, 40);
        log.note_gauge(1, 40, 40, 3, 100);
        assert!(log.open_window(1).is_some(), "the gauge became active");
        log.note_hit(1, 7, 12010340, 200, false);
        log.note_hit(1, 7, 12090243, 300, false);
        log.note_gauge(1, 40, 5, 2, 310);
        log.note_gauge(1, 40, 40, 0, 900);
        let w = &log.windows_between(0, 1000)[0];
        assert_eq!((w.outcome(), w.ended), (Some(Outcome::Missed), Some(End::Reset)));
        assert_eq!(w.missing, 5);
        assert_eq!(w.start_ms, 100);
        assert_eq!(w.end_ms, Some(900));
        assert_eq!(w.hits, vec![Hit { ms: 300, actor: 7, skill: 12090243, value: 35, ambiguous: false }]);
    }

    #[test]
    fn a_window_open_at_the_fights_end_is_missed_there() {
        let mut log = StaggerLog::default();
        log.note_spawn(1, 40, 40);
        log.note_hit(1, 7, 1, 300, false);
        log.note_gauge(1, 40, 5, 2, 310);
        // Saved while open, then after the reset that came after the fight.
        for _ in 0..2 {
            let w = &log.windows_between(0, 500)[0];
            assert_eq!((w.outcome(), w.ended, w.missing, w.end_ms), (Some(Outcome::Missed), Some(End::FightEnd), 5, Some(500)));
            log.note_gauge(1, 40, 40, 0, 5_000);
        }
        assert!(log.open_window(1).is_none());
    }

    #[test]
    fn a_break_takes_what_was_left_and_the_refill_is_no_stagger() {
        let mut log = StaggerLog::default();
        log.note_spawn(1, 150, 150);
        log.note_hit(1, 7, 1, 100, false);
        log.note_gauge(1, 150, 9, 2, 110);
        log.note_hit(1, 7, 2, 200, false);
        log.note_break(1, 210);
        log.note_gauge(1, 150, 150, 2, 300);
        assert!(log.open_window(1).is_none(), "a refill opens no window");
        let w = &log.windows_between(0, 1000)[0];
        assert_eq!(w.outcome(), Some(Outcome::Break));
        assert_eq!(w.hits.iter().map(|h| h.value).sum::<u32>(), 150);
        assert_eq!(w.hits[1].value, 9);
    }

    #[test]
    fn a_hit_with_entries_of_its_own_wins_over_a_proc_after_it() {
        let mut log = StaggerLog::default();
        log.note_spawn(1, 150, 104);
        log.note_hit(1, 7, 12040140, 100, false);
        log.note_hit(1, 7, 12130141, 200, false);
        log.note_hit_entry(1, 7, 12130141);
        log.note_hit_entry(1, 8, 12730001);
        log.note_hit(1, 7, 12730001, 200, false);
        log.note_gauge(1, 150, 94, 2, 210);
        // With none marked, the last one; a mark is spent with its update.
        log.note_hit(1, 7, 12090243, 300, false);
        log.note_hit_entry(1, 7, 12040140);
        log.note_gauge(1, 150, 59, 2, 310);
        let hits = &log.windows_between(0, 1000)[0].hits;
        assert_eq!(hits.iter().map(|h| (h.skill, h.value)).collect::<Vec<_>>(), vec![(12130141, 10), (12090243, 35)]);
    }

    /// Enhanced Harcon, 2026-10-22: Judgment (Templar 9348) and a Chanter's
    /// hit (10868), each with an entry of its own, then the gauge 947 to 942
    /// and 940. Judgment takes 5 as it does solo.
    #[test]
    fn a_partys_batch_gives_its_deductions_to_the_marked_hits_in_turn() {
        let mut log = StaggerLog::default();
        log.note_spawn(1, 1200, 947);
        log.note_hit(1, 9348, 12730001, 100, false);
        log.note_hit(1, 9348, 12240340, 250, false);
        log.note_hit_entry(1, 9348, 12240340);
        log.note_hit(1, 10868, 18050230, 250, false);
        log.note_hit_entry(1, 10868, 18050230);
        log.note_gauge(1, 1200, 942, 2, 250);
        log.note_gauge(1, 1200, 940, 2, 250);
        let hits = &log.windows_between(0, 1000)[0].hits;
        assert_eq!(
            hits.iter().map(|h| (h.actor, h.skill, h.value, h.ambiguous)).collect::<Vec<_>>(),
            vec![(9348, 12240340, 5, false), (10868, 18050230, 2, false)]
        );
    }

    #[test]
    fn a_marked_hit_that_took_nothing_is_dropped_with_its_batch() {
        let mut log = StaggerLog::default();
        log.note_spawn(1, 40, 40);
        // Locked: a marked hit, an update that moves nothing.
        log.note_hit(1, 7, 12090243, 100, false);
        log.note_hit_entry(1, 7, 12090243);
        log.note_gauge(1, 40, 40, 3, 150);
        log.note_hit(1, 7, 12010340, 200, false);
        log.note_hit(1, 7, 12100340, 300, false);
        log.note_hit_entry(1, 7, 12100340);
        log.note_gauge(1, 40, 30, 2, 300);
        let hits = &log.windows_between(0, 1000)[0].hits;
        assert_eq!(hits.iter().map(|h| (h.skill, h.value)).collect::<Vec<_>>(), vec![(12100340, 10)]);
    }

    #[test]
    fn a_dot_tick_never_takes_a_remembered_hit() {
        let mut log = StaggerLog::default();
        log.note_spawn(1, 40, 40);
        log.note_hit(1, 7, 1, 100, false);
        log.note_hit(1, 8, 2, 105, true);
        log.note_gauge(1, 40, 30, 2, 110);
        log.note_hit(1, 8, 2, 200, true);
        log.note_gauge(1, 40, 25, 2, 210);
        let hits = &log.windows_between(0, 1000)[0].hits;
        assert_eq!((hits[0].actor, hits[0].skill, hits[0].ambiguous), (7, 1, false));
        assert_eq!((hits[1].actor, hits[1].skill), (8, 2), "with none remembered, the tick is");
    }

    #[test]
    fn another_actors_hit_since_the_last_update_is_ambiguous() {
        let mut log = StaggerLog::default();
        log.note_spawn(1, 40, 40);
        log.note_hit(1, 7, 1, 100, false);
        log.note_hit(1, 8, 2, 105, false);
        log.note_gauge(1, 40, 30, 2, 110);
        log.note_hit(1, 8, 2, 200, false);
        log.note_gauge(1, 40, 20, 2, 210);
        let hits = &log.windows_between(0, 1000)[0].hits;
        assert_eq!((hits[0].actor, hits[0].ambiguous), (8, true));
        assert_eq!((hits[1].actor, hits[1].ambiguous), (8, false));
    }

    #[test]
    fn a_deduction_with_no_hit_is_unknowns_and_an_old_hit_is_not_taken() {
        let mut log = StaggerLog::default();
        log.note_spawn(1, 40, 40);
        log.note_gauge(1, 40, 30, 2, 100);
        log.note_hit(1, 7, 1, 200, false);
        log.note_gauge(1, 40, 20, 2, 200 + PENDING_HIT_MAX_AGE_MS + 1);
        let hits = &log.windows_between(0, i64::MAX)[0].hits;
        assert_eq!(hits.iter().map(|h| (h.actor, h.value)).collect::<Vec<_>>(), vec![(0, 10), (0, 10)]);
    }

    #[test]
    fn a_death_misses_the_window_and_forgets_the_gauge() {
        let mut log = StaggerLog::default();
        log.note_spawn(1, 40, 40);
        log.note_hit(1, 7, 1, 100, false);
        log.note_gauge(1, 40, 30, 2, 110);
        log.note_gone(1, 500, true);
        assert!(log.gauge(1).is_none());
        let w = &log.windows_between(0, 1000)[0];
        assert_eq!((w.outcome(), w.ended, w.missing, w.end_ms), (Some(Outcome::Missed), Some(End::Death), 30, Some(500)));
    }

    #[test]
    fn a_new_max_is_a_new_starting_value() {
        let mut log = StaggerLog::default();
        log.note_spawn(1, 40, 40);
        log.note_gauge(1, 60, 40, 0, 100);
        assert_eq!(log.gauge(1), Some((60, 40)));
        assert!(log.open_window(1).is_none());
        log.note_hit(1, 7, 1, 200, false);
        log.note_gauge(1, 60, 30, 2, 210);
        let w = log.open_window(1).unwrap();
        assert_eq!((w.hits[0].value, w.incomplete), (10, true), "40 of 60 is not a full gauge");
    }

    #[test]
    fn windows_round_trip() {
        let json = serde_json::json!([{
            "entity": 54651, "mobCode": 2310471, "max": 40, "start": 43900, "end": 56454,
            "result": "missed", "ended": "reset", "missing": 5,
            "hits": [{"t": 55751, "actor": 5336, "skill": 12090243, "value": 35}]
        }]);
        let w: Vec<StaggerWindow> = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(w[0].result, Outcome::Missed);
        assert!(!w[0].incomplete && !w[0].hits[0].ambiguous);
        assert_eq!(serde_json::to_value(&w).unwrap(), json);
    }
}
