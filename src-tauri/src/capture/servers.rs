//! The game servers that exist, to tell a home server the game stated from a
//! misread one.
//!
//! An id is faction, region, slot: `1` Elyos or `2` Asmodian, then the region
//! (`1` NA East, `2` NA West, `3` EU, `4` SA, `5` Asia), then the slot. The
//! Global servers are NC's own list (the notice "New Server and Matchmaking
//! Information", whose page carries it as data, 2026-10-27): 106 of them,
//! the same slots for both factions. The client's string table names more
//! slots (to 60) than are open, and Aion 2 Timers' table had 50 a region: an
//! id past these is a misread, as 50 ids in 1,676 boss reports were
//! (2258 for one player's ten days). When NC opens servers, add them here.
//!
//! Korea and Taiwan number theirs `10xx` / `20xx` (one client string table,
//! 1001-1058 and 2001-2058; see `boss_report::with_host`).

/// Open slots per Global region, by its digit (1 NA East … 5 Asia).
const GLOBAL_SLOTS: [(u16, u16); 5] = [(1, 9), (2, 5), (3, 23), (4, 6), (5, 10)];
/// Korea's and Taiwan's slots.
const KR_TW_SLOTS: u16 = 58;

/// Whether `id` is a game server that exists.
pub fn exists(id: u16) -> bool {
    let faction = id / 1000;
    if faction != 1 && faction != 2 {
        return false;
    }
    let region = id / 100 % 10;
    let slot = id % 100;
    if slot == 0 {
        return false;
    }
    if region == 0 {
        return slot <= KR_TW_SLOTS;
    }
    GLOBAL_SLOTS.iter().any(|&(r, slots)| r == region && slot <= slots)
}

#[cfg(test)]
mod tests {
    use super::exists;

    #[test]
    fn the_open_servers_and_nothing_past_them() {
        // NC's list: the first and last slot of each region, both factions.
        for (first, last) in [(1101, 1109), (1201, 1205), (1301, 1323), (1401, 1406), (1501, 1510)] {
            for id in [first, last, first + 1000, last + 1000] {
                assert!(exists(id), "{id}");
            }
            assert!(!exists(last + 1), "{}", last + 1);
            assert!(!exists(last + 1001), "{}", last + 1001);
        }
        assert_eq!((1000..3000).filter(|&id| exists(id) && id % 1000 >= 100).count(), 106);
        // What Aion 2 Timers could not place (2026-10-27): none exists.
        for id in [2258, 2563, 2551, 2480, 2395, 1575, 1470, 1151, 1560] {
            assert!(!exists(id), "{id}");
        }
        // Korea and Taiwan.
        assert!(exists(1001) && exists(2058) && !exists(1059) && !exists(1000));
        assert!(!exists(0) && !exists(999) && !exists(3101) && !exists(1100));
    }
}
