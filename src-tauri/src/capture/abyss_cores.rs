//! The Abyss's Dimensional Cores (the Artifact Siege's "Dimensional Orbs"),
//! by the NPC code their spawn names: one per artifact, fought for during
//! the siege. The meter shares when one is seen up, its HP and its kill, as
//! for a world boss (`boss_report.rs`), so a2tools.app can show a server's
//! siege as it goes. Only what a player near a core sees: its spawn, its HP
//! while in view, its death.
//!
//! From the game's tables (NpcData `M_AR*_OrcusGem_*`, "Dimensional Core",
//! and the Abyss maps' spawners), 2026-10-18.

/// (NPC code, AbyssArtifact id or 0 when it names none), sorted by code.
pub const CORES: &[(i32, u32)] = &[
    (2600078, 1001), // Lower Reshanta: Reshanta's Root
    (2600079, 0),    // Lower Reshanta (BSZ), no fixed spawn point
    (2600080, 1002), // Lower Reshanta: Sulfur Tree Island
    (2600081, 1002), // Lower Reshanta: Sulfur Tree Island, second
    (2600082, 1003), // Lower Reshanta: Archipelago of Siel's Wings
    (2600083, 1003), // Lower Reshanta: Archipelago of Siel's Wings, second
    (2600151, 0),    // Abyss_Reshanta_B (ACRA)
    (2600152, 0),    // Abyss_Reshanta_B (ACRA), second
    (2600153, 0),    // Abyss_Reshanta_B (ACRB)
    (2600516, 2001), // second layer: Reshanta's Root
    (2600517, 2002), // second layer: Sulfur Tree Island
    (2600518, 2003), // second layer: Archipelago of Siel's Wings
];

fn find(code: i32) -> Option<usize> {
    CORES.binary_search_by_key(&code, |&(npc, _)| npc).ok()
}

/// Whether an NPC code is a Dimensional Core.
pub fn is_core(code: i32) -> bool {
    find(code).is_some()
}

/// The artifact a core stands for (AbyssArtifact id), if it names one.
pub fn artifact_of(code: i32) -> Option<u32> {
    find(code).map(|i| CORES[i].1).filter(|&a| a != 0)
}

/// Who took an orb's corridor when the orb fell: the killing blow's faction,
/// which the game gives the corridor to (Aion 2 Timers, 2026-10-18). A meter
/// doesn't see the killing blow, so it is read off the artifact's holder as
/// next seen: changed since before the kill, or a holding that began at it.
/// The game only says who holds what when a player opens the Abyss map, so a
/// kill waits for that up to `KILLER_WAIT_MS`, then goes as "unknown".
pub const KILLER_WAIT_MS: i64 = 30 * 60_000;

/// A holding that began within this of the kill began with it.
const BEGAN_AT_KILL_MS: i64 = 2 * 60_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrbKill {
    pub npc: i32,
    pub artifact: u32,
    pub at_ms: i64,
    /// "elyos", "asmo" or "unknown".
    pub killer: &'static str,
}

#[derive(Debug, Clone)]
struct Pending {
    npc: i32,
    artifact: u32,
    at_ms: i64,
    holder_before: Option<super::abyss_artifacts::Owner>,
}

/// The kills waiting for their artifact's holder, and the holders last seen.
#[derive(Debug, Default)]
pub struct OrbKillers {
    holders: std::collections::HashMap<u32, super::abyss_artifacts::Owner>,
    pending: Vec<Pending>,
}

fn faction(owner: super::abyss_artifacts::Owner) -> Option<&'static str> {
    use super::abyss_artifacts::Owner;
    match owner {
        Owner::Elyos => Some("elyos"),
        Owner::Asmodian => Some("asmo"),
        _ => None,
    }
}

impl OrbKillers {
    /// A core died: wait for its artifact's holder. Cores that name no
    /// artifact have no corridor to tell by.
    pub fn killed(&mut self, npc: i32, at_ms: i64) {
        let Some(artifact) = artifact_of(npc) else { return };
        if self.pending.iter().any(|p| p.npc == npc && p.at_ms == at_ms) {
            return;
        }
        let holder_before = self.holders.get(&artifact).copied();
        self.pending.push(Pending { npc, artifact, at_ms, holder_before });
    }

    /// The holders the game said, `seen_ms`: the kills they settle.
    pub fn holders_seen(&mut self, state: &super::abyss_artifacts::ArtifactState, seen_ms: i64) -> Vec<OrbKill> {
        let mut settled = Vec::new();
        self.pending.retain(|p| {
            if seen_ms < p.at_ms {
                return true;
            }
            let Some(artifact) = state.artifacts.iter().find(|a| a.id == p.artifact) else { return true };
            let Some(killer) = faction(artifact.owner) else { return true };
            let changed = p.holder_before.is_some_and(|before| before != artifact.owner);
            let began_at_kill = state
                .wars
                .iter()
                .find(|w| w.event_id == artifact.event_id())
                .is_some_and(|w| w.since_ms > 0 && (w.since_ms - p.at_ms).abs() <= BEGAN_AT_KILL_MS);
            if changed || began_at_kill {
                settled.push(OrbKill { npc: p.npc, artifact: p.artifact, at_ms: p.at_ms, killer });
                false
            } else {
                true
            }
        });
        for a in &state.artifacts {
            self.holders.insert(a.id, a.owner);
        }
        settled
    }

    /// The kills that waited too long: "unknown".
    pub fn expired(&mut self, now_ms: i64) -> Vec<OrbKill> {
        let mut out = Vec::new();
        self.pending.retain(|p| {
            if now_ms - p.at_ms < KILLER_WAIT_MS {
                return true;
            }
            out.push(OrbKill { npc: p.npc, artifact: p.artifact, at_ms: p.at_ms, killer: "unknown" });
            false
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::abyss_artifacts::{Artifact, ArtifactState, ArtifactWar, Owner};

    fn state(owner: Owner, since_ms: i64) -> ArtifactState {
        ArtifactState {
            wars: vec![ArtifactWar { event_id: 1001, since_ms, until_ms: since_ms + 1 }],
            artifacts: vec![Artifact { id: 1002, owner }],
        }
    }

    /// The holder changed after the kill: the new one dealt it.
    #[test]
    fn a_changed_holder_after_the_kill_is_the_killer() {
        let mut k = OrbKillers::default();
        assert!(k.holders_seen(&state(Owner::None, 0), 1_000).is_empty());
        k.killed(2600080, 50_000);
        assert!(k.holders_seen(&state(Owner::None, 0), 60_000).is_empty(), "no holder yet: still waiting");
        let got = k.holders_seen(&state(Owner::Asmodian, 1), 400_000);
        assert_eq!(got, vec![OrbKill { npc: 2600080, artifact: 1002, at_ms: 50_000, killer: "asmo" }]);
        assert!(k.expired(10_000_000).is_empty(), "settled once");
    }

    /// No holder known before: a holding that began at the kill tells it.
    #[test]
    fn a_holding_that_began_at_the_kill_tells_the_killer() {
        let mut k = OrbKillers::default();
        k.killed(2600080, 1_000_000);
        let got = k.holders_seen(&state(Owner::Elyos, 1_030_000), 1_500_000);
        assert_eq!(got[0].killer, "elyos");
        // An older holding tells nothing.
        let mut k = OrbKillers::default();
        k.killed(2600080, 9_000_000);
        assert!(k.holders_seen(&state(Owner::Elyos, 1_000_000), 9_500_000).is_empty());
        assert_eq!(k.expired(9_000_000 + KILLER_WAIT_MS)[0].killer, "unknown");
    }

    /// Holders said before the kill settle nothing; cores without an
    /// artifact are not waited for.
    #[test]
    fn only_later_holders_count_and_only_cores_with_artifacts_wait() {
        let mut k = OrbKillers::default();
        k.killed(2600080, 5_000_000);
        assert!(k.holders_seen(&state(Owner::Elyos, 5_000_000), 4_000_000).is_empty());
        k.killed(2600079, 5_000_000);
        assert_eq!(k.expired(5_000_000 + KILLER_WAIT_MS).len(), 1);
    }

    #[test]
    fn the_list_is_sorted_and_names_each_artifact() {
        assert!(CORES.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(artifact_of(2600080), Some(1002));
        assert_eq!(artifact_of(2600518), Some(2003));
        assert_eq!(artifact_of(2600079), None);
        assert!(is_core(2600079) && !is_core(2600068));
    }
}
