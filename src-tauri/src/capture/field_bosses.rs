//! The respawning field bosses on open maps and the Abyss's bosses, by the
//! NPC code their spawn names: the ones whose kills and HP milestones the
//! meter shares
//! (`boss_report.rs`). From the game's tables, as a2tools.app's
//! `field_bosses.py` (tools/boss_timers/build_bosses.py) lists them.

/// (NPC code, world-map boss id) of each, sorted by NPC code.
pub const FIELD_BOSSES: &[(i32, u32)] = &[
    (2100003, 101001), // Neikel of the East
    (2100040, 101002), // Rotten Kutar
    (2100050, 101003), // Kernon of the West
    (2100076, 101004), // Kusan the Mad Gladiator
    (2100077, 101005), // Ritualist Garshim
    (2100079, 101006), // Bodyguard Teegant
    (2100141, 101007), // Blooming Korin
    (2100177, 101008), // Furious Saursus
    (2100178, 101009), // Bloodfang Pnyn
    (2100582, 101010), // Heretic Layla
    (2100617, 101011), // Black Tentacle Lawa
    (2100661, 101012), // Phantasm Kasia
    (2100708, 101013), // Centurion Demiros
    (2100718, 101014), // Divine Ansas
    (2100876, 101015), // Harvest Manager Moshav
    (2100877, 101016), // Sentinel K'nash
    (2100988, 101017), // Scholar Aulla
    (2100989, 101018), // Forest Warrior Aullamu
    (2100991, 101019), // Chaser Taulo
    (2101016, 101020), // Researcher Setram
    (2101074, 101021), // Eternal Gartua
    (2101120, 101022), // Silent Dartan
    (2101122, 101023), // Soul Ruler Kashapa
    (2101131, 101024), // High Commander Lagta
    (2400017, 111001), // Melted Danar
    (2400074, 111002), // Black Warrior Aed
    (2400140, 111003), // Faithful Rajit
    (2400141, 111004), // Berserker Vargor
    (2400212, 111005), // Predator Garsan
    (2400223, 111006), // Blood Warrior Lannar
    (2400274, 111007), // Deceiver Trid
    (2400335, 111008), // Blue Wave Kelpina
    (2400353, 111009), // High Overseer Nutah
    (2400358, 111010), // Advisor Resana
    (2400419, 111011), // Special Operations Leader Linx
    (2400424, 111012), // Desecrator Newbold
    (2400425, 111013), // Specter Archon Axios
    (2400474, 111014), // Addicted Hardirun
    (2400504, 111015), // Executioner Barthien
    (2400593, 111016), // Drakan Battalion Weapon Guruta
    (2400607, 111017), // Veteran Shujakan
    (2400608, 111018), // Visionary Karuka
    (2400659, 111019), // Dark Shadow Vishwada
    (2400709, 111020), // Sharp Shylak
    (2400800, 111021), // Immortal Gartua
    (2400853, 111022), // High Commander Lagta
    (2400854, 111023), // Soul Ruler Kashapa
    (2400855, 111024), // Silent Dartan
    // The Abyss's bosses (map 20). All but Argo, the Spirit King keep a
    // fixed schedule (the game's PeriodSpawn), which Event Timers knows; their
    // kills and HP are shared all the same, as the others' (2026-10-28: a
    // player fought Watcher Kaira down and no HP reached anyone).
    (2600068, 2001), // Argo, the Spirit King
    (2600084, 2003), // Guardian Lord Nahma
    (2600089, 2002), // Watcher Kaira
    (2600093, 2004), // Guardian Lord Nahma
    (2600094, 2005), // Guardian Lord Nahma
    (2600096, 2006), // Executor Tamasa
    (2600097, 2007), // Executor Argo
    (2600098, 2008), // Executor Kaira
];

/// The world-map boss id of a field boss's NPC code, the id a2tools.app
/// knows it by.
pub fn world_map_id(code: i32) -> Option<u32> {
    FIELD_BOSSES.binary_search_by_key(&code, |&(npc, _)| npc).ok().map(|i| FIELD_BOSSES[i].1)
}

/// Whether an NPC code is one of them.
pub fn is_field_boss(code: i32) -> bool {
    FIELD_BOSSES.binary_search_by_key(&code, |&(npc, _)| npc).is_ok()
}

/// Whether the meter shares an NPC's kill and HP: a field boss, or one of
/// the Abyss's Dimensional Cores (`abyss_cores`).
pub fn is_reported(code: i32) -> bool {
    is_field_boss(code) || super::abyss_cores::is_core(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_sorted_and_knows_a_verteron_boss() {
        assert!(FIELD_BOSSES.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(is_field_boss(2100079)); // Bodyguard Teegant
        assert!(!is_field_boss(2100080));
        assert_eq!(world_map_id(2600089), Some(2002)); // Watcher Kaira, the Abyss
    }
}
