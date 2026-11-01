//! The Supply Requests boards, as the game sends them at login (`56 E2`,
//! inside the login bundle; the window shows them from the client's cache).
//! Read 2026-10-20 on Kaisinel (EU 1304, Elyos).
//!
//! ```text
//! 56 E2 <n u8>
//!   per board:  <board u32> <tab u8: 1 Emergency, 2 Weekly, 3 Craft, 4 Boss>
//!               <start i64 ms> <end i64 ms> <k u8 = requests>
//!               k x ( <board u32> <request u32> <delivered u32> )
//! ```
//!
//! - Emergency and Weekly run from one reset to the next (07:00 UTC daily, and
//!   weekly); Craft and Boss are standing pools whose window ends in 2099.
//! - `delivered` is this character's progress. It is not kept: only the
//!   boards, which every player on the server shares, leave this module.
//! - **A stray byte after the first entry and then after every eighth**,
//!   carried across board headers (16 in the 128 entries captured). An entry
//!   starts with its board's id, so where it doesn't, one byte is skipped
//!   and the entry read from the next. A board that still doesn't read is
//!   where reading stops; the frame's count (6 captured) included 44 bytes
//!   that aren't boards at all, and those are left alone.

/// One board: what it is, its window, and the requests on it (Gather/
/// SupplyRequest ids, the same for every player on the server).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    pub id: u32,
    pub tab: u8,
    pub start_ms: i64,
    pub end_ms: i64,
    pub requests: Vec<u32>,
}

const OPCODE: [u8; 2] = [0x56, 0xE2];
const ENTRY: usize = 12;
const HEADER: usize = 22;
const MAX_BOARDS: usize = 16;
const MAX_REQUESTS: usize = 200;
/// Unix ms: a window starts after 2017 and ends by 2103 (the standing pools
/// end in 2099). Anything else is not a board.
const EARLIEST_MS: i64 = 1_500_000_000_000;
const LATEST_MS: i64 = 4_200_000_000_000;

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

fn i64_at(b: &[u8], i: usize) -> Option<i64> {
    Some(i64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?))
}

/// The board starting at `i`, and where the next begins.
fn board(b: &[u8], i: usize) -> Option<(Board, usize)> {
    let id = u32_at(b, i)?;
    let tab = *b.get(i + 4)?;
    let start_ms = i64_at(b, i + 5)?;
    let end_ms = i64_at(b, i + 13)?;
    let count = *b.get(i + 21)? as usize;
    if !(1..=4).contains(&tab)
        || !(EARLIEST_MS < start_ms && start_ms < end_ms && end_ms < LATEST_MS)
        || count == 0
        || count > MAX_REQUESTS
    {
        return None;
    }
    let mut j = i + HEADER;
    let mut requests = Vec::with_capacity(count);
    for _ in 0..count {
        if u32_at(b, j)? != id {
            // The stray byte: the entry starts one further on, or this isn't
            // a board.
            if u32_at(b, j + 1)? != id {
                return None;
            }
            j += 1;
        }
        requests.push(u32_at(b, j + 4)?);
        j += ENTRY;
    }
    Some((Board { id, tab, start_ms, end_ms, requests }, j))
}

/// The boards in a `56 E2` frame (length varint, opcode, body), or None if
/// this isn't one or no board reads.
pub fn parse(packet: &[u8]) -> Option<Vec<Board>> {
    let (_, i) = varint(packet, 0)?;
    if packet.get(i..i + 2)? != OPCODE {
        return None;
    }
    let count = *packet.get(i + 2)? as usize;
    if count == 0 || count > MAX_BOARDS {
        return None;
    }
    let mut at = i + 3;
    let mut boards = Vec::new();
    for _ in 0..count {
        // A stray byte can sit before a header too.
        let Some((one, next)) = board(packet, at).or_else(|| board(packet, at + 1)) else { break };
        boards.push(one);
        at = next;
    }
    (!boards.is_empty()).then_some(boards)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s = s.trim();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// Kaisinel's boards at login, 2026-10-20: the Emergency (Daily) one with
    /// Pure Sapphire Gemstone (1011020) and Silentium (1011118) among its ten,
    /// as the game showed; Weekly, and the 54-request Craft and Boss pools.
    #[test]
    fn the_four_boards_read_past_the_stray_bytes() {
        let boards = parse(&hex(include_str!("testdata/supply_requests.hex"))).unwrap();
        let summary: Vec<_> = boards.iter().map(|b| (b.id, b.tab, b.requests.len())).collect();
        assert_eq!(summary, vec![(5401417, 1, 10), (5401421, 2, 10), (5403141, 3, 54), (5403131, 4, 54)]);
        let daily = &boards[0];
        assert_eq!(daily.requests[0], 1011020);
        assert!(daily.requests.contains(&1011118));
        // 2026-10-20 07:00:01 to 2026-10-21 07:00:00.999 UTC.
        assert_eq!((daily.start_ms, daily.end_ms), (1_792_479_601_000, 1_792_566_000_999));
    }

    #[test]
    fn other_frames_and_broken_boards_are_left_alone() {
        assert!(parse(&hex("0407e300")).is_none());
        let mut frame = hex(include_str!("testdata/supply_requests.hex"));
        // The first board's tab out of range: nothing reads.
        let at = 2 + 2 + 1 + 4;
        frame[at] = 9;
        assert!(parse(&frame).is_none());
    }
}
