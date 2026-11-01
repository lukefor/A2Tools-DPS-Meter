//! Replays a player's capture through the meter as the live app reads it,
//! asking for the meter every second of capture time, and checks what each
//! target mode shows.
//!
//! `packets_20261007_155826.txt` (A2_TARGET_MODE_CAPTURE, a player's report of
//! 2026-10-07, EU) starts with the meter restarted next to the training
//! scarecrows, whose spawns it therefore never saw, and goes on to a Draupnir
//! (Exploration) run: Predator Saraswati, Phantasmal Lakshmi, Transcendent
//! Bakarma. The player (entity 2737 in town, unnamed until 15:59:57) reported
//! Train mode showing nothing and the meter not moving on to the second
//! sub-boss. Skips when the variable is unset.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use a2tools_dps_meter_lib::capture::evidence_slice::CapturedPacket;
use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::combat::dps_calculator::DpsCalculator;
use a2tools_dps_meter_lib::combat::ping_tracker::PingTracker;
use a2tools_dps_meter_lib::entity::dps_data::DpsData;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};
use a2tools_dps_meter_lib::share::read_capture;

const MODES: [&str; 3] = ["bossTargets", "allTargets", "trainTargets"];

/// What each mode showed, by capture time (local `HH:MM:SS`, CEST).
type Shown = HashMap<(String, &'static str), DpsData>;

fn replay(packets: &[CapturedPacket]) -> (Shown, Arc<DataStorage>) {
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
    // One meter per mode. They share the storage, so a zone change's reset
    // request, which the first to ask would take, is passed to each here.
    let mut meters: Vec<DpsCalculator> = MODES
        .iter()
        .map(|m| {
            let mut c = DpsCalculator::new(storage.clone(), sk.clone(), npc.clone(), Arc::new(PingTracker::new()));
            c.set_target_selection_mode(m);
            c
        })
        .collect();
    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();
    let mut shown = Shown::new();
    let mut next_tick = 0;
    for p in packets {
        proc.set_override_timestamp(Some(p.captured_at_ms));
        let acc = streams.entry(p.stream.clone()).or_insert_with(PacketAccumulator::new);
        acc.append(&p.bytes);
        let used = proc.consume_stream(acc.snapshot());
        if used > 0 {
            acc.discard_bytes(used);
        }
        if p.captured_at_ms < next_tick {
            continue;
        }
        next_tick = p.captured_at_ms + 1000;
        if storage.take_combat_reset_requested() {
            for m in &mut meters {
                m.restart_target_selection(false);
            }
        }
        let at = chrono::DateTime::from_timestamp_millis(p.captured_at_ms + 2 * 3_600_000)
            .unwrap()
            .format("%H:%M:%S")
            .to_string();
        for (mode, meter) in MODES.iter().zip(&mut meters) {
            shown.insert((at.clone(), *mode), meter.get_dps());
        }
    }
    proc.set_override_timestamp(None);
    (shown, storage)
}

/// The meter at the first tick at or after `at`.
fn at<'a>(shown: &'a Shown, at: &str, mode: &str) -> &'a DpsData {
    let mut keys: Vec<&(String, &str)> = shown.keys().filter(|(t, m)| t.as_str() >= at && *m == mode).collect();
    keys.sort();
    &shown[keys[0]]
}

#[test]
fn train_and_boss_modes_through_a_restart_and_a_dungeon() {
    let Ok(path) = std::env::var("A2_TARGET_MODE_CAPTURE") else {
        eprintln!("A2_TARGET_MODE_CAPTURE unset; skipping");
        return;
    };
    let packets = read_capture(Path::new(&path)).expect("capture readable");
    let (shown, storage) = replay(&packets);
    let code = |d: &DpsData| storage.mob_code(d.target_id).unwrap_or(0);

    // Train mode, the meter just restarted: scarecrows 19088 and 19523 never
    // spawned in its view and are known by their HP; 25839, the one the
    // player hit, likewise. The player's damage on it is on the meter.
    let train = at(&shown, "15:59:30", "trainTargets");
    for dummy in [19088, 19523, 25839] {
        assert!(train.detail_target_ids.contains(&dummy), "{dummy}: {:?}", train.detail_target_ids);
    }
    assert!(train.map.get(&2737).is_some_and(|r| r.amount > 0.0), "the player's damage on the scarecrow");

    // Boss mode: each boss as it is engaged.
    assert_eq!(code(at(&shown, "16:03:30", "bossTargets")), 2310403, "Predator Saraswati");
    assert_eq!(code(at(&shown, "16:04:38", "bossTargets")), 2310401, "Phantasmal Lakshmi");
    assert_eq!(code(at(&shown, "16:08:20", "bossTargets")), 2310471, "Transcendent Bakarma");

    // All Targets, its default two minutes: Saraswati (34194, last hit at
    // 16:03:23) is in at first and leaves at 16:05:23.
    assert!(at(&shown, "16:04:40", "allTargets").detail_target_ids.contains(&34194));
    let later = at(&shown, "16:05:30", "allTargets");
    assert!(!later.detail_target_ids.contains(&34194), "{:?}", later.detail_target_ids);
    assert!(later.detail_target_ids.contains(&33795));
}
