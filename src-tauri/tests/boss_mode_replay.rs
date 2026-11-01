//! Replays a player's capture through a Boss-mode meter, asking for it every
//! second of capture time, and checks that only bosses are shown.
//!
//! `packets_20261014_193418.txt` (A2_BOSS_MODE_CAPTURE, a report of
//! 2026-10-14, times in +08:00) starts with the meter opened inside the
//! instance of Grandios (2340057, entity 32321), so no map load placed it and
//! the dungeon id stayed 0. Before the boss was first hit (19:34:57) Boss mode
//! showed its spirit adds (Ancient Water/Earth Spirit 2340054/2340056, elite
//! monsters), and after the player left (map 1010 at 19:36:09) every
//! open-world mob they hit: Mutated Dratonas (2100900/2100902) and mobs whose
//! spawn was never seen. Personal Guard Irima (2101108), a named hero monster
//! of a quest instance, is a boss by the game's own table and stays.
//! Skips when the variable is unset.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::combat::dps_calculator::DpsCalculator;
use a2tools_dps_meter_lib::combat::ping_tracker::PingTracker;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};
use a2tools_dps_meter_lib::share::read_capture;

#[test]
fn boss_mode_shows_bosses_only() {
    let Ok(path) = std::env::var("A2_BOSS_MODE_CAPTURE") else {
        eprintln!("A2_BOSS_MODE_CAPTURE unset; skipping");
        return;
    };
    let packets = read_capture(Path::new(&path)).expect("capture readable");
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
    let mut meter = DpsCalculator::new(storage.clone(), sk.clone(), npc.clone(), Arc::new(PingTracker::new()));
    meter.set_target_selection_mode("bossTargets");
    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();
    // (time, target entity, its NPC code) at every tick.
    let mut shown: Vec<(String, i32, i32)> = Vec::new();
    let mut next_tick = 0;
    for p in &packets {
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
            meter.restart_target_selection(false);
        }
        let at = chrono::DateTime::from_timestamp_millis(p.captured_at_ms + 8 * 3_600_000)
            .unwrap()
            .format("%H:%M:%S")
            .to_string();
        let d = meter.get_dps();
        shown.push((at, d.target_id, storage.mob_code(d.target_id).unwrap_or(0)));
    }
    proc.set_override_timestamp(None);

    // Every target ever shown is a boss by the NPC table.
    for (at, id, code) in &shown {
        assert!(*id == 0 || npc.is_boss(*code), "{at}: entity {id}, code {code} ({})", npc.get_npc_name(*code));
    }
    let code_at = |t: &str| shown.iter().find(|(at, _, _)| at.as_str() >= t).map(|s| s.2).unwrap();
    assert_eq!(code_at("19:34:40"), 0, "the spirit adds before Grandios is hit");
    assert_eq!(code_at("19:35:30"), 2340057, "Grandios");
    assert_eq!(code_at("19:40:30"), 0, "an open-world Dratona");
    assert_eq!(code_at("19:42:40"), 2101108, "Personal Guard Irima");
}
