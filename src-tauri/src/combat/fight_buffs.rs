//! A fight's buffs and debuffs, compact enough to keep with the fight in
//! History: the abnormal tracks (`capture::abnormal::tracks`) on the fight's
//! actors, on their summons and on its target, cut to the fight's window.
//!
//! One `BuffTrack` per entity, abnormal and caster: every time that caster's
//! abnormal was on that entity in the fight, as segments of constant stacks.
//! Summons are resolved to their owners when the fight is saved, because
//! their entity ids are reused after a map load. A passive (an abnormal with
//! no timer, on from before the fight to past its end) is kept as a flag, with
//! no segments.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::capture::abnormal::{End, Track};
use crate::combat::data_storage::DataStorage;
use crate::entity::summon_resolver;

/// At most this many tracks per fight: the timed ones up longest are kept,
/// then passives. A party's boss fight has 100-250; an open-world boss with
/// twenty players comes past it.
pub const MAX_TRACKS: usize = 400;
/// At most this many segments per track (a DoT reapplied every few seconds
/// over a long fight comes near it).
pub const MAX_SEGMENTS: usize = 400;

/// How a segment ended, in `BuffTrack::segs`.
pub mod end_code {
    /// The stacks changed; the abnormal stayed on.
    pub const STACKS: u8 = 0;
    pub const EXPIRED: u8 = 1;
    /// A skill took it off (a cleanse, a dispel, a consumed charge).
    pub const TAKEN_OFF: u8 = 2;
    /// The server removed it for a reason not decoded.
    pub const REMOVED: u8 = 3;
    /// Another caster applied it again (one shared debuff, now theirs).
    pub const RECAST: u8 = 4;
    /// Its entity died or left.
    pub const GONE: u8 = 5;
    pub const MAP_LOAD: u8 = 6;
    /// Missing from a later list of what the entity has on.
    pub const NOT_LISTED: u8 = 7;
    /// A newer stack went past the stack limit and pushed it out.
    pub const PUSHED_OUT: u8 = 8;
    /// No record ended it: closed at its timer.
    pub const UNSEEN: u8 = 9;
    /// Still on when the fight ended.
    pub const STILL_ON: u8 = 10;
    /// A passive replaced by its new level.
    pub const REPLACED: u8 = 11;
}

fn code_of(end: End) -> u8 {
    match end {
        End::Expired => end_code::EXPIRED,
        End::TakenOff { .. } => end_code::TAKEN_OFF,
        End::Removed(5) => end_code::REPLACED,
        End::Removed(_) => end_code::REMOVED,
        End::Recast => end_code::RECAST,
        End::Gone => end_code::GONE,
        End::MapLoad => end_code::MAP_LOAD,
        End::NotListed => end_code::NOT_LISTED,
        End::PushedOut => end_code::PUSHED_OUT,
        End::Unseen => end_code::UNSEEN,
        End::Open => end_code::STILL_ON,
    }
}

pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// One abnormal from one caster on one of the fight's actors or its target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuffTrack {
    /// Whom it was on: an actor's id (as in `FightRecord::actors`) or the
    /// fight's target.
    pub on: i32,
    /// On one of that actor's summons rather than the actor.
    #[serde(default, skip_serializing_if = "is_false")]
    pub summon: bool,
    /// The `SkillAbnormal` id (names in `i18n/abnormals`).
    pub id: u32,
    /// Who applied it, a summon resolved to its owner; 0 when not named.
    pub by: i32,
    /// The skill that applied it, for its icon; 0 when not named.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skill: u32,
    /// On through the whole fight with no timer: a passive. No segments.
    #[serde(default, skip_serializing_if = "is_false")]
    pub passive: bool,
    /// Its segments, `start,end,stacks,how;...`: times in ms from the fight's
    /// start (a start before 0 was on before the fight began), `how` an
    /// `end_code`. A string, because History pretty-prints its files and an
    /// array would take a line per number.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub segs: String,
    /// Time on within the fight, in ms.
    #[serde(default)]
    pub up: i64,
}

/// One segment of a `BuffTrack`, read back from `segs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub start: i64,
    pub end: i64,
    pub stacks: u32,
    pub how: u8,
}

impl BuffTrack {
    pub fn segments(&self) -> Vec<Segment> {
        self.segs
            .split(';')
            .filter_map(|s| {
                let mut f = s.split(',');
                Some(Segment {
                    start: f.next()?.parse().ok()?,
                    end: f.next()?.parse().ok()?,
                    stacks: f.next()?.parse().ok()?,
                    how: f.next()?.parse().ok()?,
                })
            })
            .collect()
    }
}

#[derive(Default)]
struct Building {
    skill: u32,
    passive: bool,
    segments: Vec<Segment>,
    up: i64,
}

/// The fight's tracks from `start_ms` to `end_ms`, for `actors` (owner ids)
/// and `target_id`. `owner` resolves an entity to the owner of its summon
/// chain; the tracks' casters are expected resolved already.
pub fn build(
    tracks: &[Track],
    start_ms: i64,
    end_ms: i64,
    actors: &HashSet<i32>,
    target_id: i32,
    owner: impl Fn(i32) -> i32,
) -> Vec<BuffTrack> {
    if end_ms <= start_ms {
        return Vec::new();
    }
    let duration = end_ms - start_ms;
    let mut groups: BTreeMap<(i32, bool, u32, i32), Building> = BTreeMap::new();
    for t in tracks {
        let (on, summon) = if t.entity == target_id {
            (target_id, false)
        } else {
            let o = owner(t.entity);
            (o, o != t.entity)
        };
        // The target's adds are not the target.
        if on == target_id && summon {
            continue;
        }
        if on != target_id && !actors.contains(&on) {
            continue;
        }
        let ends_after = t.end_ms.is_none_or(|e| e >= end_ms);
        if t.end_ms.is_some_and(|e| e <= start_ms) || t.start_ms >= end_ms {
            continue;
        }
        let entry = groups.entry((on, summon, t.abnormal, t.owner)).or_default();
        if entry.skill == 0 {
            entry.skill = t.skills.first().copied().unwrap_or(0);
        }
        if t.endless && t.start_ms <= start_ms && ends_after {
            entry.passive = true;
            entry.up = duration;
            continue;
        }
        for (i, &(from, stacks)) in t.stacks.iter().enumerate() {
            if stacks == 0 {
                continue;
            }
            let next = t.stacks.get(i + 1);
            let to = next.map(|s| s.0).or(t.end_ms).unwrap_or(i64::MAX);
            let last = next.is_none_or(|s| s.1 == 0);
            let mut how = if last { code_of(t.end) } else { end_code::STACKS };
            if to <= start_ms || from >= end_ms {
                continue;
            }
            let to = if to > end_ms {
                how = end_code::STILL_ON;
                end_ms
            } else {
                to
            };
            entry.up += to - from.max(start_ms);
            entry.segments.push(Segment { start: from - start_ms, end: to - start_ms, stacks, how });
        }
    }

    let mut out: Vec<(Vec<Segment>, BuffTrack)> = groups
        .into_iter()
        .filter(|(_, b)| b.passive || !b.segments.is_empty())
        .map(|((on, summon, id, by), b)| {
            let mut segments = b.segments;
            segments.sort_by_key(|s| s.start);
            segments.truncate(MAX_SEGMENTS);
            let track = BuffTrack {
                on,
                summon,
                id,
                by,
                skill: b.skill,
                passive: b.passive,
                segs: String::new(),
                up: b.up.min(duration),
            };
            (if b.passive { Vec::new() } else { segments }, track)
        })
        .collect();
    if out.len() > MAX_TRACKS {
        // Passives are the first to go: they say the least.
        out.sort_by_key(|(_, t)| (t.passive, std::cmp::Reverse(t.up)));
        out.truncate(MAX_TRACKS);
    }
    out.sort_by_key(|(s, t)| (t.on, t.summon, t.passive, s.first().map_or(0, |s| s.start), t.id, t.by));
    out.into_iter()
        .map(|(segments, mut t)| {
            t.segs = segments
                .iter()
                .map(|s| format!("{},{},{},{}", s.start, s.end, s.stacks, s.how))
                .collect::<Vec<_>>()
                .join(";");
            t
        })
        .collect()
}

/// The buffs and debuffs of the fight on `target_id` from `start_ms` to
/// `end_ms`, fought by `actors`, from what the meter has recorded.
pub fn for_fight(
    storage: &DataStorage,
    target_id: i32,
    start_ms: i64,
    end_ms: i64,
    actors: impl IntoIterator<Item = i32>,
) -> Vec<BuffTrack> {
    let actors: HashSet<i32> = actors.into_iter().filter(|&a| a > 0).collect();
    let mut entities = actors.clone();
    entities.insert(target_id);
    let tracks = storage.fight_abnormal_tracks(start_ms, end_ms, &entities);
    let links = storage.get_summon_data();
    build(&tracks, start_ms, end_ms, &actors, target_id, |e| summon_resolver::resolve(e, &links))
}

/// The live fight on a target, for Details while it is still going.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveFightBuffs {
    pub target_id: i32,
    pub start_time_ms: i64,
    pub duration_ms: i64,
    pub buffs: Vec<BuffTrack>,
    /// The fight's stagger windows so far (`combat::stagger`), fetched with
    /// the buffs: Details shows both on the same timeline.
    #[serde(default)]
    pub stagger: Vec<crate::combat::stagger::StaggerWindow>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const START: i64 = 1_000_000;
    const END: i64 = START + 60_000;
    const PLAYER: i32 = 100;
    const HEALER: i32 = 200;
    const SPIRIT: i32 = 900;
    const BOSS: i32 = 5000;

    fn track(entity: i32, abnormal: u32, owner: i32, stacks: &[(i64, u32)], end: End) -> Track {
        let open = stacks.last().is_none_or(|s| s.1 != 0);
        Track {
            entity,
            abnormal,
            owner,
            skills: vec![abnormal / 10],
            level: 1,
            endless: false,
            start_ms: stacks[0].0,
            end_ms: if open { None } else { stacks.last().map(|s| s.0) },
            end: if open { End::Open } else { end },
            stacks: stacks.to_vec(),
        }
    }

    fn links() -> HashMap<i32, i32> {
        HashMap::from([(SPIRIT, PLAYER)])
    }

    fn run(tracks: &[Track]) -> Vec<BuffTrack> {
        let links = links();
        build(tracks, START, END, &HashSet::from([PLAYER, HEALER]), BOSS, |e| summon_resolver::resolve(e, &links))
    }

    #[test]
    fn a_buff_reapplied_is_one_track_with_a_segment_each_time() {
        let tracks = [
            track(PLAYER, 161_900_001, HEALER, &[(START + 1_000, 1), (START + 11_000, 0)], End::Expired),
            track(PLAYER, 161_900_001, HEALER, &[(START + 20_000, 1), (START + 25_000, 0)], End::TakenOff { skill: 7 }),
        ];
        let out = run(&tracks);
        assert_eq!(out.len(), 1);
        let t = &out[0];
        assert_eq!((t.on, t.id, t.by, t.skill, t.summon, t.passive), (PLAYER, 161_900_001, HEALER, 16_190_000, false, false));
        assert_eq!(t.segs, "1000,11000,1,1;20000,25000,1,2");
        assert_eq!(t.up, 15_000);
    }

    #[test]
    fn stacks_split_a_segment_and_only_the_last_piece_says_how_it_ended() {
        let tracks = [track(
            PLAYER,
            163_000_003,
            PLAYER,
            &[(START + 5_000, 1), (START + 6_000, 2), (START + 7_000, 3), (START + 9_000, 0)],
            End::PushedOut,
        )];
        let t = &run(&tracks)[0];
        assert_eq!(
            t.segments(),
            [
                Segment { start: 5_000, end: 6_000, stacks: 1, how: end_code::STACKS },
                Segment { start: 6_000, end: 7_000, stacks: 2, how: end_code::STACKS },
                Segment { start: 7_000, end: 9_000, stacks: 3, how: end_code::PUSHED_OUT },
            ]
        );
        assert_eq!(t.up, 4_000);
    }

    #[test]
    fn segments_are_cut_to_the_fight_and_uptime_counts_only_inside_it() {
        let tracks = [
            // On from 10 s before the pull to 5 s in.
            track(PLAYER, 1, HEALER, &[(START - 10_000, 1), (START + 5_000, 0)], End::Expired),
            // On past the end of the fight, though it ended later.
            track(PLAYER, 2, HEALER, &[(END - 3_000, 1), (END + 7_000, 0)], End::Expired),
            // Still on when the capture was read.
            track(PLAYER, 3, HEALER, &[(END - 1_000, 2)], End::Open),
            // Over before the fight.
            track(PLAYER, 4, HEALER, &[(START - 9_000, 1), (START - 1_000, 0)], End::Expired),
        ];
        let out = run(&tracks);
        let by_id = |id: u32| out.iter().find(|t| t.id == id).cloned();
        assert_eq!(by_id(1).unwrap().segs, "-10000,5000,1,1");
        assert_eq!(by_id(1).unwrap().up, 5_000);
        assert_eq!(by_id(2).unwrap().segs, "57000,60000,1,10");
        assert_eq!(by_id(3).unwrap().segs, "59000,60000,2,10");
        assert_eq!(by_id(3).unwrap().up, 1_000);
        assert!(by_id(4).is_none());
    }

    #[test]
    fn summons_are_their_owners_and_strangers_are_left_out() {
        let tracks = [
            track(SPIRIT, 10, PLAYER, &[(START, 1), (START + 1_000, 0)], End::Expired),
            track(7777, 11, PLAYER, &[(START, 1), (START + 1_000, 0)], End::Expired),
            track(BOSS, 12, SPIRIT, &[(START, 1), (START + 2_000, 0)], End::Recast),
        ];
        // Casters come resolved from the timeline (the spirit's debuff is its summoner's).
        let mut tracks = tracks.to_vec();
        tracks[2].owner = PLAYER;
        let out = run(&tracks);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].on, out[0].summon, out[0].id), (PLAYER, true, 10));
        assert_eq!((out[1].on, out[1].summon, out[1].id, out[1].by), (BOSS, false, 12, PLAYER));
        assert_eq!(out[1].segs, "0,2000,1,4");
    }

    #[test]
    fn a_passive_on_through_the_fight_is_a_flag_without_segments() {
        let mut passive = track(PLAYER, 20, PLAYER, &[(START - 100_000, 1)], End::Open);
        passive.endless = true;
        // A toggle without a timer, switched on mid-fight, keeps its segments.
        let mut toggle = track(PLAYER, 21, PLAYER, &[(START + 30_000, 1), (START + 40_000, 0)], End::Removed(9));
        toggle.endless = true;
        let out = run(&[passive, toggle]);
        assert_eq!(out.len(), 2);
        let p = out.iter().find(|t| t.id == 20).unwrap();
        assert!(p.passive);
        assert_eq!((p.segs.as_str(), p.up), ("", 60_000));
        let t = out.iter().find(|t| t.id == 21).unwrap();
        assert!(!t.passive);
        assert_eq!(t.segs, "30000,40000,1,3");
        let json = serde_json::to_string(p).unwrap();
        assert_eq!(json, r#"{"on":100,"id":20,"by":100,"skill":2,"passive":true,"up":60000}"#);
    }

    #[test]
    fn the_cap_keeps_the_tracks_up_longest() {
        let tracks: Vec<Track> = (0..(MAX_TRACKS as u32 + 10))
            .map(|i| track(PLAYER, 1_000 + i, HEALER, &[(START, 1), (START + 1 + i64::from(i), 0)], End::Expired))
            .collect();
        let out = run(&tracks);
        assert_eq!(out.len(), MAX_TRACKS);
        assert!(out.iter().all(|t| t.id >= 1_010));
        // A passive is up all fight, and still goes before any timed track.
        let mut with_passive = tracks.clone();
        let mut passive = track(PLAYER, 99, PLAYER, &[(START - 1, 1)], End::Open);
        passive.endless = true;
        with_passive.push(passive);
        assert!(run(&with_passive).iter().all(|t| !t.passive));
    }

    #[test]
    fn nothing_for_an_empty_window() {
        let tracks = [track(PLAYER, 1, HEALER, &[(START, 1), (START + 1_000, 0)], End::Expired)];
        let links = links();
        assert!(build(&tracks, END, END, &HashSet::from([PLAYER]), BOSS, |e| summon_resolver::resolve(e, &links)).is_empty());
    }
}
