//! Who holds each Abyss artifact (the corridors' Artifact Siege), as the game
//! tells the player: with the Abyss map when it is opened (`07 E3`, both
//! layers), and on entering the Abyss (`05 E3`, the layer entered).
//! Read 2026-10-18 against the in-game map of an Elyos character: Reshanta's
//! Root and Siel's Wings blue (1), Sulfur Tree Island purple (2), 2 to 1.
//!
//! ```text
//! 07 E3 <u16 0> <wars u8> wars x <war> <artifacts u8> artifacts x <artifact>
//! 05 E3                   <war>        <artifacts u8> artifacts x <artifact>
//!
//! war:      <u8> <event id u32> <u8> <u8> <since u64 ms> [<until u64 ms>]
//! artifact: <artifact id u32> <u32> <owner u8> <8 bytes>
//! ```
//!
//! The ids are the game's AbyssArtifactEvent (1001 on map 20, 2001 on map
//! 22) and AbyssArtifact (1001-1003: Reshanta's Root, Sulfur Tree Island,
//! the Archipelago of Siel's Wings; 2001-2003 the same three on the second
//! layer). Owner 1 is Elyos and 2 Asmodian, as server ids 1xxx and 2xxx.
//!
//! A war that has a holder carries when it began and when it resets (2026-10-17
//! 19:12:55 and 2026-10-19 19:00 UTC on the 18th); one that has none
//! carries a single 0. Only one capture holds these packets, so a packet is
//! read only when every byte of it falls into place: anything else in its
//! shape is left alone rather than guessed at.

/// One layer's Artifact Siege.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactWar {
    /// AbyssArtifactEvent id: 1001, 2001.
    pub event_id: u32,
    /// Unix ms the current holding began and when it resets; 0 if none.
    pub since_ms: i64,
    pub until_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    None,
    Elyos,
    Asmodian,
    /// A value not seen yet, kept as sent.
    Other(u8),
}

impl Owner {
    fn from_byte(b: u8) -> Self {
        match b {
            0 => Owner::None,
            1 => Owner::Elyos,
            2 => Owner::Asmodian,
            n => Owner::Other(n),
        }
    }

    pub fn name(self) -> String {
        match self {
            Owner::None => "none".into(),
            Owner::Elyos => "elyos".into(),
            Owner::Asmodian => "asmodian".into(),
            Owner::Other(n) => format!("unknown-{n}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Artifact {
    /// AbyssArtifact id: 1001-1003, 2001-2003.
    pub id: u32,
    pub owner: Owner,
}

impl Artifact {
    /// The war (AbyssArtifactEvent id) an artifact belongs to: 1002 -> 1001.
    pub fn event_id(&self) -> u32 {
        self.id / 1000 * 1000 + 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactState {
    pub wars: Vec<ArtifactWar>,
    pub artifacts: Vec<Artifact>,
}

const MAP_OPCODE: [u8; 2] = [0x07, 0xE3];
const ENTRY_OPCODE: [u8; 2] = [0x05, 0xE3];
const ARTIFACT_BYTES: usize = 17;
const MAX_WARS: usize = 8;
const MAX_ARTIFACTS: usize = 32;

fn varint(b: &[u8], mut i: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let x = *b.get(i)?;
        i += 1;
        v |= u64::from(x & 0x7F) << shift;
        if x & 0x80 == 0 {
            return Some((v, i));
        }
        shift += 7;
        if shift > 35 {
            return None;
        }
    }
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

fn time_at(b: &[u8], i: usize) -> Option<i64> {
    let t = i64::try_from(u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?)).ok()?;
    // 0, or a time between 2020 and 2100.
    (t == 0 || (1_577_836_800_000..4_102_444_800_000).contains(&t)).then_some(t)
}

/// The artifacts from `i` to exactly the end of `b`.
fn artifacts(b: &[u8], i: usize) -> Option<Vec<Artifact>> {
    let count = *b.get(i)? as usize;
    if count == 0 || count > MAX_ARTIFACTS || b.len() != i + 1 + count * ARTIFACT_BYTES {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for n in 0..count {
        let at = i + 1 + n * ARTIFACT_BYTES;
        let id = u32_at(b, at)?;
        if !(1..1_000_000).contains(&id) {
            return None;
        }
        out.push(Artifact { id, owner: Owner::from_byte(b[at + 8]) });
    }
    Some(out)
}

/// `wars` wars from `i`, then the artifacts to the end of `b`. A war's
/// second time is there or not; whichever reading accounts for every byte.
fn wars_then_artifacts(b: &[u8], i: usize, wars: usize) -> Option<(Vec<ArtifactWar>, Vec<Artifact>)> {
    if wars == 0 {
        return Some((Vec::new(), artifacts(b, i)?));
    }
    let event_id = u32_at(b, i + 1)?;
    if !(1..1_000_000).contains(&event_id) {
        return None;
    }
    let since_ms = time_at(b, i + 7)?;
    let with_until = || {
        let until_ms = time_at(b, i + 15)?;
        let (mut rest, artifacts) = wars_then_artifacts(b, i + 23, wars - 1)?;
        rest.insert(0, ArtifactWar { event_id, since_ms, until_ms });
        Some((rest, artifacts))
    };
    let without = || {
        let (mut rest, artifacts) = wars_then_artifacts(b, i + 15, wars - 1)?;
        rest.insert(0, ArtifactWar { event_id, since_ms, until_ms: 0 });
        Some((rest, artifacts))
    };
    if since_ms != 0 { with_until().or_else(without) } else { without().or_else(with_until) }
}

/// The artifact owners in a `07 E3` or `05 E3` record, or None when it is
/// neither or doesn't read cleanly.
pub fn parse(packet: &[u8]) -> Option<ArtifactState> {
    let (_, i) = varint(packet, 0)?;
    let opcode = packet.get(i..i + 2)?;
    let i = i + 2;
    let (wars, artifacts) = if opcode == MAP_OPCODE {
        if packet.get(i..i + 2)? != [0, 0] {
            return None;
        }
        let count = *packet.get(i + 2)? as usize;
        if count == 0 || count > MAX_WARS {
            return None;
        }
        wars_then_artifacts(packet, i + 3, count)?
    } else if opcode == ENTRY_OPCODE {
        wars_then_artifacts(packet, i, 1)?
    } else {
        return None;
    };
    Some(ArtifactState { wars, artifacts })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s = s.trim();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn owners(state: &ArtifactState) -> Vec<(u32, Owner)> {
        state.artifacts.iter().map(|a| (a.id, a.owner)).collect()
    }

    #[test]
    fn the_abyss_map_says_who_holds_each_artifact_on_both_layers() {
        let state = parse(&hex(include_str!("testdata/abyss_artifacts_map.hex"))).unwrap();
        assert_eq!(
            state.wars,
            vec![
                ArtifactWar { event_id: 2001, since_ms: 0, until_ms: 0 },
                ArtifactWar { event_id: 1001, since_ms: 1_792_264_375_000, until_ms: 1_792_436_400_000 },
            ]
        );
        // Reshanta's Root and Siel's Wings Elyos, Sulfur Tree Island Asmodian:
        // the in-game map's 2 to 1. Nobody holds the second layer's.
        assert_eq!(
            owners(&state),
            vec![
                (2001, Owner::None),
                (2002, Owner::None),
                (2003, Owner::None),
                (1001, Owner::Elyos),
                (1002, Owner::Asmodian),
                (1003, Owner::Elyos),
            ]
        );
        assert_eq!(state.artifacts[4].event_id(), 1001);
    }

    #[test]
    fn entering_the_abyss_says_the_same_for_the_layer_entered() {
        let state = parse(&hex(include_str!("testdata/abyss_artifacts_entry.hex"))).unwrap();
        assert_eq!(
            state.wars,
            vec![ArtifactWar { event_id: 1001, since_ms: 1_792_264_375_000, until_ms: 1_792_436_400_000 }]
        );
        assert_eq!(owners(&state), vec![(1001, Owner::Elyos), (1002, Owner::Asmodian), (1003, Owner::Elyos)]);
    }

    #[test]
    fn a_record_that_does_not_fall_into_place_is_left_alone() {
        let mut map = hex(include_str!("testdata/abyss_artifacts_map.hex"));
        map.pop();
        assert!(parse(&map).is_none());
        let mut entry = hex(include_str!("testdata/abyss_artifacts_entry.hex"));
        entry.push(0);
        assert!(parse(&entry).is_none());
        // Another opcode.
        assert!(parse(&[0x05, 0x91, 0x01, 0x00]).is_none());
    }
}
