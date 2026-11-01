//! Replays captures through the meter and reports the stagger gauges
//! (`05 E0`, see `combat::stagger`) of the fights it would save.
//!
//! `A2_REPLAY_STAGGER` (a report, no asserts): per capture, the `41 36`
//! spawns that end in a gauge and whether each of those entities got a
//! `05 E0` later (a spawn read as having a gauge that never moves is a false
//! match), then per saved fight one `window` line per gauge window (entity,
//! result, length, what was missing, `incomplete`) and one `hit` line per
//! deduction (time from the fight's start, actor, skill, value, `ambiguous`),
//! and in the end the deductions that were ambiguous, that went to no actor
//! (actor 0), and that differ from the values measured for the Templar's
//! skills on 2026-10-22 (a break takes what is left, so the last hit of a
//! broken window is not compared).
//!
//! `A2_STAGGER_SOLO_CAPTURE` (asserts): the solo capture of 2026-10-22
//! (Global EU, Story mode, Templar entity 5336): two minibosses (150 each,
//! both broken), then Transcendent Bakarma (2310471, entity 54651), wiped at
//! 0:56.5 with its gauge at 5 of 40 after Punishment took 35. The capture
//! holds character names and is not in the repository.
//!
//! ```text
//! A2_REPLAY_STAGGER="<capture>;<capture>..."  packets_*.txt captures
//! A2_STAGGER_SOLO_CAPTURE=<capture>           the 2026-10-22 solo capture
//! cargo test --test stagger_replay -- --nocapture
//! ```
//!
//! Each skips when its variable is unset.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use a2tools_dps_meter_lib::capture::framing::{self, FrameKind};
use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
use a2tools_dps_meter_lib::capture::stream_processor::{
    read_varint, spawn_stagger_gauge, stagger_gauge_record, StaggerRecord, StreamProcessor,
};
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::combat::dps_calculator::DpsCalculator;
use a2tools_dps_meter_lib::combat::ping_tracker::PingTracker;
use a2tools_dps_meter_lib::combat::stagger::{End, Outcome, StaggerWindow};
use a2tools_dps_meter_lib::entity::fight_record::FightRecord;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};
use a2tools_dps_meter_lib::share::read_capture;
use parking_lot::Mutex;

/// What each Templar skill took off a gauge on every hit, 2026-10-22.
const TEMPLAR_STAGGER: &[(i32, u32)] = &[
    (12090243, 35), // Punishment
    (12120010, 20), // Taunt
    (12070000, 20), // Doom Shield
    (12100340, 10), // Shield Smite
    (12130141, 10), // Poach
    (12270020, 10), // Debilitating Smash
    (12300340, 10), // Annihilate
    (12430240, 10), // Shield Rush
    (12240340, 5),  // Judgment
    (12060140, 5),  // Punishing Strike
    (12350130, 5),  // Warding Strike
    (12040140, 3),  // Pummel, first strike of the chain
    (12010340, 0),  // Vicious Strike
    (12020340, 0),  // Decisive Strike
    (12030340, 0),  // Desperate Strike
    (12340340, 0),  // Flash Rampage
    (12730001, 0),  // Punishing Benediction (a proc)
];

/// The packets in a buffer, bundles opened, as the stream processor walks
/// them, each with whether it came out of a bundle.
fn frames_of(buf: &[u8], top: bool, out: &mut Vec<(bool, Vec<u8>)>, depth: usize) {
    let walk = if top { framing::walk(buf) } else { framing::walk_inner(buf) };
    for f in &walk.frames {
        match f.kind {
            FrameKind::Packet => out.push((depth > 0, f.bytes(buf).to_vec())),
            FrameKind::Bundle if depth < framing::MAX_BUNDLE_DEPTH => {
                if let Some(inner) = framing::decompress_bundle(f.payload(buf)) {
                    frames_of(&inner, false, out, depth + 1);
                }
            }
            FrameKind::Bundle => {}
        }
    }
}

/// A capture replayed: its fights as last saved, every window over the whole
/// capture, and what its frames said on their own.
struct Replay {
    fights: Vec<FightRecord>,
    windows: Vec<StaggerWindow>,
    first_ms: i64,
    /// Entities whose spawn ended in a gauge: (max, rest) and when.
    gauge_spawns: BTreeMap<i32, (u32, u32, i64)>,
    /// Entities with a `05 E0` record, and how many.
    updated: BTreeMap<i32, usize>,
    bundled_updates: usize,
    spawns: usize,
    /// The most stagger the meter's rows showed, per target and actor.
    meter_stagger: BTreeMap<(i32, i32), i64>,
}

fn replay(path: &str) -> Replay {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/data");
    let npc = Arc::new(NpcLookup::new());
    npc.load_from_json(&std::fs::read_to_string(data.join("i18n/npcs/en.json")).unwrap());
    let sk = Arc::new(SkillLookup::new());
    sk.load_from_json(&std::fs::read_to_string(data.join("i18n/skills/en.json")).unwrap());
    let dots: Vec<i32> =
        serde_json::from_str(&std::fs::read_to_string(data.join("dot_skill_ids.json")).unwrap()).unwrap();

    let storage = Arc::new(DataStorage::new());
    let mut proc = StreamProcessor::new(storage.clone(), sk.clone(), npc.clone());
    proc.set_dot_skill_ids(dots.into_iter().collect());
    let calc = Arc::new(Mutex::new(DpsCalculator::new(storage.clone(), sk, npc, Arc::new(PingTracker::new()))));
    calc.lock().set_target_selection_mode("bossTargets");
    let fights: Arc<Mutex<BTreeMap<String, FightRecord>>> = Arc::default();
    let save = {
        let (calc, fights) = (calc.clone(), fights.clone());
        Arc::new(move |force: bool| {
            let records = if force { calc.lock().snapshot_boss_fights_force() } else { calc.lock().snapshot_boss_fights() };
            for r in records {
                fights.lock().insert(r.id.clone(), r);
            }
        })
    };
    {
        let save = save.clone();
        storage.set_before_reset(move || save(true));
    }

    let packets = read_capture(Path::new(path)).expect("capture readable");
    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();
    let mut walks: HashMap<String, PacketAccumulator> = HashMap::new();
    let mut out = Replay {
        fights: Vec::new(),
        windows: Vec::new(),
        first_ms: packets.first().map_or(0, |p| p.captured_at_ms),
        gauge_spawns: BTreeMap::new(),
        updated: BTreeMap::new(),
        bundled_updates: 0,
        spawns: 0,
        meter_stagger: BTreeMap::new(),
    };
    let mut next_tick = i64::MIN;
    for p in &packets {
        proc.set_override_timestamp(Some(p.captured_at_ms));
        let acc = streams.entry(p.stream.clone()).or_insert_with(PacketAccumulator::new);
        acc.append(&p.bytes);
        let used = proc.consume_stream(acc.snapshot());
        if used > 0 {
            acc.discard_bytes(used);
        }

        // The same frames again, read on their own.
        let walk = walks.entry(p.stream.clone()).or_insert_with(PacketAccumulator::new);
        walk.append(&p.bytes);
        let consumed = framing::walk(walk.snapshot()).consumed;
        let mut frames = Vec::new();
        frames_of(&walk.snapshot()[..consumed], true, &mut frames, 0);
        walk.discard_bytes(consumed);
        for (bundled, f) in &frames {
            let o = read_varint(f, 0).length.max(0) as usize;
            match f.get(o..o + 2) {
                Some([0x41, 0x36]) => {
                    out.spawns += 1;
                    if let Some((max, rest)) = spawn_stagger_gauge(f) {
                        out.gauge_spawns.insert(read_varint(f, o + 2).value, (max, rest, p.captured_at_ms));
                    }
                }
                Some([0x05, 0xE0]) => {
                    let entity = match stagger_gauge_record(f) {
                        Some(StaggerRecord::Gauge { entity, .. } | StaggerRecord::Break { entity }) => entity,
                        None => read_varint(f, o + 2).value,
                    };
                    *out.updated.entry(entity).or_default() += 1;
                    if *bundled {
                        out.bundled_updates += 1;
                    }
                }
                _ => {}
            }
        }

        if p.captured_at_ms >= next_tick {
            next_tick = p.captured_at_ms + 1_000;
            let dps = calc.lock().get_dps();
            for (&actor, row) in &dps.map {
                if row.stagger > 0 {
                    let seen = out.meter_stagger.entry((dps.target_id, actor)).or_default();
                    *seen = (*seen).max(row.stagger);
                }
            }
            save(false);
        }
    }
    save(true);
    proc.set_override_timestamp(None);
    out.windows = storage.fight_stagger(out.first_ms, i64::MAX);
    out.fights = std::mem::take(&mut *fights.lock()).into_values().collect();
    out.fights.sort_by_key(|f| f.start_time_ms);
    out
}

fn result_label(r: Outcome) -> &'static str {
    match r {
        Outcome::Break => "break",
        Outcome::Missed => "missed",
    }
}

/// The deductions that differ from `TEMPLAR_STAGGER`, the last hit of a
/// break aside: (skill, expected, value).
fn table_deviations(w: &StaggerWindow) -> Vec<(i32, u32, u32)> {
    let table: HashMap<i32, u32> = TEMPLAR_STAGGER.iter().copied().collect();
    let compared = if w.result == Outcome::Break { w.hits.len().saturating_sub(1) } else { w.hits.len() };
    w.hits[..compared]
        .iter()
        .filter_map(|h| table.get(&h.skill).filter(|&&v| v != h.value).map(|&v| (h.skill, v, h.value)))
        .collect()
}

fn print_window(w: &StaggerWindow, ms: &dyn Fn(i64) -> String) {
    let sum: u32 = w.hits.iter().map(|h| h.value).sum();
    println!(
        "window entity {} (mob {}) max {} {} .. {} s, {:.1} s, {}, missing {}, deducted {}{}",
        w.entity,
        w.mob_code,
        w.max,
        ms(w.start),
        ms(w.end),
        (w.end - w.start) as f64 / 1000.0,
        result_label(w.result),
        w.missing,
        sum,
        if w.incomplete { ", incomplete" } else { "" },
    );
    for h in &w.hits {
        println!(
            "  hit {} s actor {} skill {} value {}{}",
            ms(h.t),
            h.actor,
            h.skill,
            h.value,
            if h.ambiguous { " AMBIGUOUS" } else { "" }
        );
    }
}

#[test]
fn stagger_report() {
    let Ok(paths) = std::env::var("A2_REPLAY_STAGGER") else {
        eprintln!("A2_REPLAY_STAGGER unset; skipping");
        return;
    };
    for path in paths.split(';').filter(|p| !p.trim().is_empty()) {
        let r = replay(path.trim());
        println!("\n==== {path}");
        println!(
            "spawns {}, ending in a gauge {}; 05 E0 records {} on {} entities, {} of them in a bundle",
            r.spawns,
            r.gauge_spawns.len(),
            r.updated.values().sum::<usize>(),
            r.updated.len(),
            r.bundled_updates
        );
        for (entity, (max, rest, at)) in &r.gauge_spawns {
            println!(
                "spawn entity {entity} gauge {rest}/{max} at {:.1} s, 05 E0 records later: {}",
                (at - r.first_ms) as f64 / 1000.0,
                r.updated.get(entity).copied().unwrap_or(0)
            );
        }
        let unspawned: Vec<&i32> = r.updated.keys().filter(|e| !r.gauge_spawns.contains_key(e)).collect();
        println!("05 E0 on entities whose gauge spawn was not seen: {unspawned:?}");

        for f in &r.fights {
            println!(
                "\nfight {} {} (target {}, mob {}) {:.1} s, {} actors",
                f.id,
                f.boss_name,
                f.target_id,
                f.mob_code,
                f.duration_ms as f64 / 1000.0,
                f.actors.len()
            );
            for w in f.stagger.as_deref().unwrap_or_default() {
                print_window(w, &|t| format!("{:+.1}", t as f64 / 1000.0));
            }
        }

        println!("\nall windows over the capture (times from its start):");
        let (mut ambiguous, mut unknown) = (0, 0);
        let mut deviations = Vec::new();
        for w in &r.windows {
            print_window(w, &|t| format!("{:.1}", t as f64 / 1000.0));
            ambiguous += w.hits.iter().filter(|h| h.ambiguous).count();
            unknown += w.hits.iter().filter(|h| h.actor == 0).count();
            deviations.extend(table_deviations(w));
        }
        let hits: usize = r.windows.iter().map(|w| w.hits.len()).sum();
        println!("summary deductions {hits}, ambiguous {ambiguous}, to actor 0 {unknown}");
        println!("summary differing from the Templar table: {deviations:?}");
        println!("summary the meter's Stagger mode at most (target, actor): {:?}", r.meter_stagger);
        let silent: Vec<&i32> = r.gauge_spawns.keys().filter(|e| !r.updated.contains_key(e)).collect();
        println!("summary gauge spawns with no 05 E0 later (false matches?): {silent:?}");
    }
}

#[test]
fn the_solo_capture_of_2026_10_22() {
    let Ok(path) = std::env::var("A2_STAGGER_SOLO_CAPTURE") else {
        eprintln!("A2_STAGGER_SOLO_CAPTURE unset; skipping");
        return;
    };
    let r = replay(&path);

    // Bakarma's wipe: missed by 5, Punishment took 35.
    let bakarma = r.fights.iter().find(|f| f.mob_code == 2310471).expect("the Bakarma fight");
    let windows = bakarma.stagger.as_deref().expect("stagger recorded");
    let on_boss: Vec<&StaggerWindow> = windows.iter().filter(|w| w.entity == 54651).collect();
    assert_eq!(on_boss.len(), 1, "{windows:?}");
    let w = on_boss[0];
    assert_eq!((w.result, w.missing, w.max), (Outcome::Missed, 5, 40), "{w:?}");
    // The reset came 5 s after the fight's last hit: the fight saw it open.
    assert_eq!(w.ended, End::FightEnd, "{w:?}");
    assert!(w.hits.iter().any(|h| h.skill == 12090243 && h.value == 35 && h.actor == 5336), "{w:?}");
    assert!(!w.incomplete);

    // The minibosses: every break adds up to the gauge.
    let breaks: Vec<&StaggerWindow> = r.windows.iter().filter(|w| w.result == Outcome::Break).collect();
    assert!(!breaks.is_empty());
    for w in &breaks {
        assert_eq!(w.hits.iter().map(|h| h.value).sum::<u32>(), w.max, "{w:?}");
        assert!(!w.incomplete, "{w:?}");
    }
    // A miniboss broken once and killed during its next window.
    assert!(r.windows.iter().any(|w| w.ended == End::Death && w.entity != 54651));
    // The meter's Stagger mode: Punishment's 35 on Bakarma, and each
    // miniboss's broken gauge and more.
    assert_eq!(r.meter_stagger.get(&(54651, 5336)), Some(&35));
    assert!(r.meter_stagger.get(&(24627, 5336)).is_some_and(|&v| v >= 150), "{:?}", r.meter_stagger);
    for w in &r.windows {
        assert!(w.hits.iter().all(|h| !h.ambiguous && h.actor != 0), "{w:?}");
        assert!(table_deviations(w).is_empty(), "{w:?}");
    }
    // Each spawn that ends in a gauge got updates later.
    assert_eq!(r.gauge_spawns.len(), 3);
    assert!(r.gauge_spawns.keys().all(|e| r.updated.contains_key(e)));
}
