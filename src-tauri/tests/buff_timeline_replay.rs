//! `A2_REPLAY_TIMELINE`: replays captures through the meter and prints, for
//! each boss fight it would save, the buffs and debuffs on the boss, on the
//! players who fought it and on their summons (see `capture::abnormal`).
//!
//! In the manner of Daevalog's `A2_REPLAY_TIMELINE` report (Seralth,
//! GPL-3.0), over this meter's own fights: a fight is a `FightRecord` as the
//! live meter saves it, and its entities are the boss and the record's
//! actors.
//!
//! ```text
//! A2_REPLAY_TIMELINE="<capture>;<capture>..."  packets_*.txt captures (required)
//! A2_REPLAY_TIMELINE_ALL=1                     also print passives and other
//!                                              abnormals that never end
//! cargo test --test buff_timeline_replay -- --nocapture
//! ```
//!
//! Per capture a `records` summary (each abnormal opcode read, and how many
//! records did not read to their last byte), then per fight a `fight` line,
//! one `buff` line per track (entity, abnormal, source, start and end from
//! the fight's start, how it ended, uptime in the fight, average and most
//! stacks), and `summary` lines: tracks per entity, the casters of the
//! boss's debuffs, how the fight's instances ended, records per minute and
//! the size of the fight's timeline. Skips when the variable is unset.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use a2tools_dps_meter_lib::capture::abnormal::{self, End, Instance, Record, Track};
use a2tools_dps_meter_lib::capture::framing::{self, FrameKind};
use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
use a2tools_dps_meter_lib::capture::stream_processor::{read_varint, StreamProcessor};
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::combat::dps_calculator::DpsCalculator;
use a2tools_dps_meter_lib::combat::ping_tracker::PingTracker;
use a2tools_dps_meter_lib::entity::fight_record::FightRecord;
use a2tools_dps_meter_lib::entity::summon_resolver;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};
use a2tools_dps_meter_lib::share::read_capture;
use parking_lot::Mutex;

/// One abnormal record of the capture, read on its own beside the meter.
struct Seen {
    ms: i64,
    /// "add", "change", "remove", "stats", "list"
    kind: &'static str,
    entity: i32,
}

/// Each opcode's records, and those that did not read to their last byte.
#[derive(Default)]
struct Counts {
    read: BTreeMap<&'static str, usize>,
    failed: BTreeMap<&'static str, usize>,
    /// One example of each failure, as hex.
    failed_example: BTreeMap<&'static str, String>,
    /// Spawn and self records with no abnormal list found.
    unlisted: BTreeMap<&'static str, usize>,
}

fn opcode_name(op: [u8; 2]) -> Option<&'static str> {
    Some(match op {
        abnormal::ADDED => "2a38 add",
        abnormal::CHANGED => "2b38 change",
        abnormal::REMOVED => "2c38 remove",
        abnormal::STATS_CHANGED => "4a36 stats",
        abnormal::ALL_STATS => "4936 all stats",
        [0x41, 0x36] => "4136 spawn",
        [0x45, 0x36] => "4536 player",
        [0x33, 0x36] => "3336 self",
        _ => return None,
    })
}

/// The packets in a buffer, bundles opened, as the stream processor walks them.
fn frames_of(buf: &[u8], top: bool, out: &mut Vec<Vec<u8>>, depth: usize) {
    let walk = if top { framing::walk(buf) } else { framing::walk_inner(buf) };
    for f in &walk.frames {
        match f.kind {
            FrameKind::Packet => out.push(f.bytes(buf).to_vec()),
            FrameKind::Bundle if depth < framing::MAX_BUNDLE_DEPTH => {
                if let Some(inner) = framing::decompress_bundle(f.payload(buf)) {
                    frames_of(&inner, false, out, depth + 1);
                }
            }
            FrameKind::Bundle => {}
        }
    }
}

/// What is known about who is who when a fight is saved.
struct Fight {
    record: FightRecord,
    local: Option<i32>,
    nicknames: HashMap<i32, String>,
    party: HashSet<String>,
    links: HashMap<i32, i32>,
    tracks: Vec<Track>,
    instances: Vec<Instance>,
}

fn snapshot(storage: &DataStorage, record: FightRecord) -> Fight {
    let (start, end) = (record.start_time_ms, record.start_time_ms + record.duration_ms);
    let links = storage.get_summon_data();
    let mut entities: HashSet<i32> = record.actors.iter().map(|a| a.actor_id).collect();
    entities.insert(record.target_id);
    let tracks = storage.fight_abnormal_tracks(start, end, &entities);
    let instances = storage
        .abnormal_instances(start, end)
        .into_iter()
        .filter(|i| entities.contains(&i.entity) || entities.contains(&summon_resolver::resolve(i.entity, &links)))
        .collect();
    Fight {
        local: storage.local_player_id().map(|id| id as i32),
        nicknames: storage.get_nicknames(),
        party: storage.get_party_members().into_keys().collect(),
        links,
        tracks,
        instances,
        record,
    }
}

impl Fight {
    fn start(&self) -> i64 {
        self.record.start_time_ms
    }
    fn end(&self) -> i64 {
        self.record.start_time_ms + self.record.duration_ms
    }
    fn actors(&self) -> HashSet<i32> {
        self.record.actors.iter().map(|a| a.actor_id).collect()
    }

    /// "target", "self", "party", "player" (fought, not on the roster),
    /// "summon:<who of its owner>", "other"; and a name.
    fn who(&self, id: i32) -> (String, String) {
        // The storage's name, else the record's (obscured but for yours).
        let name = |id: i32| {
            self.nicknames
                .get(&id)
                .cloned()
                .or_else(|| self.record.actors.iter().find(|a| a.actor_id == id && !a.nickname.is_empty()).map(|a| a.nickname.clone()))
                .unwrap_or_else(|| format!("#{id}"))
        };
        if id == self.record.target_id {
            return ("target".into(), self.record.boss_name.clone());
        }
        if id == 0 {
            return ("none".into(), "-".into());
        }
        let owner = summon_resolver::resolve(id, &self.links);
        if owner != id {
            let (kind, owner_name) = self.who(owner);
            return (format!("summon:{kind}"), format!("{} of {owner_name}", name(id)));
        }
        let kind = if Some(id) == self.local {
            "self"
        } else if self.nicknames.get(&id).is_some_and(|n| self.party.contains(n)) {
            "party"
        } else if self.actors().contains(&id) {
            "player"
        } else {
            "other"
        };
        (kind.into(), name(id))
    }
}

fn end_label(e: &End) -> String {
    match e {
        End::Expired => "Expired".into(),
        End::TakenOff { .. } => "TakenOff".into(),
        End::Removed(r) => format!("Removed({r})"),
        End::Recast => "Recast".into(),
        End::Gone => "Gone".into(),
        End::MapLoad => "MapLoad".into(),
        End::NotListed => "NotListed".into(),
        End::PushedOut => "PushedOut".into(),
        End::Unseen => "Unseen".into(),
        End::Open => "Open".into(),
    }
}

fn replay(path: &str, names: &HashMap<u32, String>, all: bool) {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/data");
    let npc = Arc::new(NpcLookup::new());
    npc.load_from_json(&std::fs::read_to_string(data.join("i18n/npcs/en.json")).unwrap());
    let sk = Arc::new(SkillLookup::new());
    sk.load_from_json(&std::fs::read_to_string(data.join("i18n/skills/en.json")).unwrap());
    let dots: Vec<i32> =
        serde_json::from_str(&std::fs::read_to_string(data.join("dot_skill_ids.json")).unwrap()).unwrap();

    let storage = Arc::new(DataStorage::new());
    storage.set_abnormal_stack_limits(abnormal::stack_limits(
        &std::fs::read_to_string(data.join("abnormals.json")).unwrap(),
    ));
    let mut proc = StreamProcessor::new(storage.clone(), sk.clone(), npc.clone());
    proc.set_dot_skill_ids(dots.into_iter().collect());
    let calc = Arc::new(Mutex::new(DpsCalculator::new(
        storage.clone(),
        sk.clone(),
        npc.clone(),
        Arc::new(PingTracker::new()),
    )));
    calc.lock().set_target_selection_mode("bossTargets");
    // Each fight as last saved, by its id.
    let fights: Arc<Mutex<BTreeMap<String, Fight>>> = Arc::default();
    let save = {
        let (storage, calc, fights) = (storage.clone(), calc.clone(), fights.clone());
        move |force: bool| {
            let records = if force { calc.lock().snapshot_boss_fights_force() } else { calc.lock().snapshot_boss_fights() };
            for r in records {
                let id = r.id.clone();
                let f = snapshot(&storage, r);
                fights.lock().insert(id, f);
            }
        }
    };
    let save = Arc::new(save);
    {
        let save = save.clone();
        storage.set_before_reset(move || save(true));
    }

    let packets = read_capture(Path::new(path)).expect("capture readable");
    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();
    let mut walks: HashMap<String, PacketAccumulator> = HashMap::new();
    let mut counts = Counts::default();
    let mut seen: Vec<Seen> = Vec::new();
    let mut next_tick = i64::MIN;
    let (first_ms, last_ms) = (packets.first().map_or(0, |p| p.captured_at_ms), packets.last().map_or(0, |p| p.captured_at_ms));
    let mut peak_held = 0usize;
    for p in &packets {
        proc.set_override_timestamp(Some(p.captured_at_ms));
        let acc = streams.entry(p.stream.clone()).or_insert_with(PacketAccumulator::new);
        acc.append(&p.bytes);
        let used = proc.consume_stream(acc.snapshot());
        if used > 0 {
            acc.discard_bytes(used);
        }

        // The same packets again, read on their own for the counts.
        let walk = walks.entry(p.stream.clone()).or_insert_with(PacketAccumulator::new);
        walk.append(&p.bytes);
        let consumed = framing::walk(walk.snapshot()).consumed;
        let mut frames = Vec::new();
        frames_of(&walk.snapshot()[..consumed], true, &mut frames, 0);
        walk.discard_bytes(consumed);
        for f in frames {
            let o = read_varint(&f, 0).length.max(0) as usize;
            let Some(op) = f.get(o..o + 2).and_then(|op| opcode_name([op[0], op[1]])) else { continue };
            let record = abnormal::parse(&f);
            let listing = op.starts_with("4136") || op.starts_with("4536") || op.starts_with("3336");
            match &record {
                Some(r) => {
                    *counts.read.entry(op).or_default() += 1;
                    let (kind, entity) = match r {
                        Record::Added(a) => ("add", a.entity),
                        Record::Changed(a) => ("change", a.entity),
                        Record::Removed { entity, .. } => ("remove", *entity),
                        Record::Stats { entity, .. } => ("stats", entity.unwrap_or(-1)),
                        Record::Listed { entity, .. } => ("list", *entity),
                    };
                    seen.push(Seen { ms: p.captured_at_ms, kind, entity });
                }
                // A spawn with nothing on, or whose list is not found, is
                // not a failure of the layout.
                None if listing => *counts.unlisted.entry(op).or_default() += 1,
                None => {
                    *counts.failed.entry(op).or_default() += 1;
                    counts.failed_example.entry(op).or_insert_with(|| f.iter().map(|b| format!("{b:02x}")).collect());
                }
            }
        }

        if p.captured_at_ms >= next_tick {
            next_tick = p.captured_at_ms + 1_000;
            calc.lock().get_dps();
            save(false);
            peak_held = peak_held.max(storage.abnormal_count());
        }
    }
    save(true);
    proc.set_override_timestamp(None);

    println!("\n==== {path}");
    println!(
        "capture {:.1} min, {} lines; timeline holds {} instances at the end, {} at most",
        (last_ms - first_ms) as f64 / 60_000.0,
        packets.len(),
        storage.abnormal_count(),
        peak_held
    );
    for (op, n) in &counts.read {
        println!("records {op}: {n} read, {} did not read to the end", counts.failed.get(op).unwrap_or(&0));
    }
    for (op, n) in &counts.unlisted {
        println!("records {op}: {n} with no abnormal list found");
    }
    for (op, n) in &counts.failed {
        if !counts.read.contains_key(op) {
            println!("records {op}: 0 read, {n} did not read to the end");
        }
        println!("  e.g. {}", &counts.failed_example[op][..counts.failed_example[op].len().min(200)]);
    }

    let fights = std::mem::take(&mut *fights.lock());
    let mut fights: Vec<Fight> = fights.into_values().collect();
    fights.sort_by_key(|f| f.start());
    for f in &fights {
        print_fight(f, &seen, names, all);
    }
}

fn print_fight(f: &Fight, seen: &[Seen], names: &HashMap<u32, String>, all: bool) {
    let (start, end) = (f.start(), f.end());
    let length = (end - start).max(1);
    let secs = |ms: i64| format!("{:+.1}", (ms - start) as f64 / 1000.0);
    let tod = chrono::DateTime::from_timestamp_millis(start).map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string());
    println!(
        "\nfight {} {} (target {}, {}) {} {:.1} s, {} actors, local {:?}, party roster {}",
        f.record.id,
        f.record.boss_name,
        f.record.target_id,
        f.record.mob_code,
        tod.unwrap_or_default(),
        length as f64 / 1000.0,
        f.record.actors.len(),
        f.local,
        f.party.len()
    );
    for a in &f.record.actors {
        let (kind, name) = f.who(a.actor_id);
        println!("  actor {} {kind} {name} {}", a.actor_id, a.job);
    }
    let mut per_entity: BTreeMap<(String, i32), (usize, usize)> = BTreeMap::new();
    let mut boss_casters: BTreeMap<String, usize> = BTreeMap::new();
    for t in &f.tracks {
        let (on_kind, on_name) = f.who(t.entity);
        let (from_kind, from_name) = f.who(t.owner);
        let e = per_entity.entry((format!("{on_kind} {on_name}"), t.entity)).or_default();
        if t.endless { e.1 += 1 } else { e.0 += 1 }
        if t.entity == f.record.target_id && !t.endless {
            *boss_casters.entry(from_kind.clone()).or_default() += 1;
        }
        if t.endless && !all {
            continue;
        }
        let (mut on, mut weighted) = (0i64, 0i64);
        for (k, &(at, n)) in t.stacks.iter().enumerate() {
            let next = t.stacks.get(k + 1).map_or(t.end_ms.unwrap_or(end), |s| s.0);
            let span = next.min(end) - at.max(start);
            if n > 0 && span > 0 {
                on += span;
                weighted += span * n as i64;
            }
        }
        println!(
            "buff {on_kind} {} {:?} {} {:?} from {from_kind} {from_name:?} lv {} {} .. {} s, {}, uptime {:.0}%, stacks {:.1} avg {} max{}",
            t.entity,
            on_name,
            t.abnormal,
            names.get(&t.abnormal).map_or("", String::as_str),
            t.level,
            secs(t.start_ms),
            t.end_ms.map_or("-".to_string(), secs),
            t.end.label(),
            100.0 * on as f64 / length as f64,
            if on > 0 { weighted as f64 / on as f64 } else { 0.0 },
            t.stacks.iter().map(|s| s.1).max().unwrap_or(0),
            if t.endless { " (endless)" } else { "" },
        );
    }
    for ((who, id), (timed, endless)) in &per_entity {
        println!("summary tracks on {who} ({id}): {timed} timed, {endless} endless");
    }
    println!("summary casters of the boss's timed debuffs: {boss_casters:?}");
    let on_boss_recast = f.instances.iter().filter(|i| i.entity == f.record.target_id && i.end == End::Recast).count();
    let boss_instances = f.instances.iter().filter(|i| i.entity == f.record.target_id).count();
    println!("summary boss instances {boss_instances}, of them ended by another caster's recast {on_boss_recast}");
    // How the instances that ended in the fight ended (timed ones).
    let mut ends: BTreeMap<String, usize> = BTreeMap::new();
    for i in f.instances.iter().filter(|i| !i.endless) {
        let in_fight = i.end_ms.is_none_or(|e| e <= end + 60_000);
        if in_fight {
            *ends.entry(end_label(&i.end)).or_default() += 1;
        }
    }
    let total: usize = ends.values().sum();
    let shares: Vec<String> = ends
        .iter()
        .map(|(k, n)| format!("{k} {n} ({:.0}%)", 100.0 * *n as f64 / total.max(1) as f64))
        .collect();
    println!("summary ends of {total} timed instances: {}", shares.join(", "));
    let before = f.instances.iter().filter(|i| i.start_ms < start).count();
    let before_timed = f.instances.iter().filter(|i| i.start_ms < start && !i.endless).count();
    println!(
        "summary instances on before the fight: {before} ({before_timed} timed); endless {}",
        f.instances.iter().filter(|i| i.endless).count()
    );
    // Records in the fight, by kind and by whom they are about.
    let mut by: BTreeMap<(String, &str), usize> = BTreeMap::new();
    let mut n = 0usize;
    for s in seen.iter().filter(|s| s.ms >= start && s.ms <= end) {
        n += 1;
        let kind = if s.entity < 0 { "self".to_string() } else { f.who(s.entity).0 };
        // A summon's records count under "summon", whoever its owner.
        let kind = kind.split(':').next().unwrap_or("").to_string();
        *by.entry((kind, s.kind)).or_default() += 1;
    }
    println!(
        "summary {n} abnormal/stat records in the fight, {:.0} a minute; by entity and kind: {by:?}",
        n as f64 * 60_000.0 / length as f64
    );
    let instance_bytes = f.instances.len() * std::mem::size_of::<Instance>();
    let track_bytes: usize = f
        .tracks
        .iter()
        .map(|t| std::mem::size_of::<Track>() + t.stacks.len() * 16 + t.skills.len() * 4)
        .sum();
    println!(
        "summary size: {} instances ({} B each, {} KB), {} tracks ({} KB with their steps)",
        f.instances.len(),
        std::mem::size_of::<Instance>(),
        instance_bytes / 1024,
        f.tracks.len(),
        track_bytes / 1024
    );
    // What History keeps of them (`FightRecord::buffs`, pretty-printed as saved).
    let kept = f.record.buffs.as_deref().unwrap_or(&[]);
    let segments: usize = kept.iter().map(|b| b.segments().len()).sum();
    let buffs_bytes = serde_json::to_string_pretty(&f.record.buffs).map_or(0, |s| s.len());
    let record_bytes = serde_json::to_string_pretty(&f.record).map_or(0, |s| s.len());
    println!(
        "summary history: {} buff tracks ({} passives, {segments} segments), {:.1} KB of a {:.1} KB saved fight",
        kept.len(),
        kept.iter().filter(|b| b.passive).count(),
        buffs_bytes as f64 / 1024.0,
        record_bytes as f64 / 1024.0
    );
}

#[test]
fn buff_timeline_report() {
    let Ok(paths) = std::env::var("A2_REPLAY_TIMELINE") else {
        eprintln!("A2_REPLAY_TIMELINE unset; skipping");
        return;
    };
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/data");
    let names: HashMap<u32, String> = serde_json::from_str::<HashMap<String, String>>(
        &std::fs::read_to_string(data.join("i18n/abnormals/en.json")).unwrap(),
    )
    .unwrap()
    .into_iter()
    .filter_map(|(id, n)| Some((id.parse().ok()?, n)))
    .collect();
    let all = std::env::var("A2_REPLAY_TIMELINE_ALL").is_ok();
    for path in paths.split(';').filter(|p| !p.is_empty()) {
        replay(path, &names, all);
    }
}
