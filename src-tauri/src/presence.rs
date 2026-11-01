//! Discord activity: "Playing AION2 — Naicha · Cleric · Lv 29 — Kaisinel (Elyos) · Europe".
//!
//! Opt-in (`dpsMeter.discordActivity`), and shown only while AION2 is open.
//! It talks to the Discord apps on this computer over their local IPC sockets,
//! so it needs no a2tools.app account and sends nothing anywhere else: Discord
//! already knows who its own user is. Every Discord running is told (Discord
//! and Vesktop side by side each have a socket, and Discord shows the activity
//! of whichever one you use), not only the first one found.
//!
//! Everything shown is what the game has told the meter about the local
//! player (see `DataStorage::local_profile`): class and level from the self
//! record, the server from it or from loot. A field nobody has stated yet is
//! left out rather than guessed.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use discord_rich_presence::activity::{Activity, Assets, Timestamps};
use discord_rich_presence::error::Error as IpcError;
use discord_rich_presence::DiscordIpc;
use tauri::Manager;

use crate::app::AppState;
use crate::combat::data_storage::{DataStorage, LocalProfile};
use crate::entity::job_class::JobClass;
use crate::platform::discord_ipc::{self, Stream as IpcStream};
use crate::platform::window_detector;

type IpcResult<T> = Result<T, IpcError>;

/// The Discord application whose name ("AION2") the activity shows under, and
/// whose art assets hold the class icons (keys as `class_key` returns them).
const DISCORD_APPLICATION_ID: &str = "1556021373401432214";

pub const SETTING_KEY: &str = "dpsMeter.discordActivity";
/// The number after the level: "combatPower", "gearScore" or "off" (the
/// default: a public status shows no number the player didn't choose).
pub const STAT_KEY: &str = "dpsMeter.discordStat";

const TICK: Duration = Duration::from_secs(10);
/// Discord not running: try again this often, not every tick.
const RECONNECT_EVERY: Duration = Duration::from_secs(30);
/// A boss counts as being fought while it took damage this recently.
const FIGHTING_WITHIN_MS: i64 = 20_000;

/// Server names in every language the game ships, and region names.
static SERVERS: std::sync::LazyLock<ServerTable> = std::sync::LazyLock::new(|| {
    serde_json::from_str(include_str!("../../src/data/servers.json")).unwrap_or_default()
});

#[derive(Default, serde::Deserialize)]
struct ServerTable {
    servers: HashMap<String, HashMap<String, String>>,
    regions: HashMap<String, HashMap<String, String>>,
}

/// What the activity says. Compared between ticks so Discord is only told
/// when something changed.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    details: String,
    /// Class and level alone, for the class image's hover text.
    class_line: String,
    state: String,
    class_key: Option<&'static str>,
    class_name: Option<String>,
}

/// Whether a Discord application is configured into this build at all.
pub fn available() -> bool {
    !DISCORD_APPLICATION_ID.is_empty()
}

pub fn spawn(app: tauri::AppHandle) {
    if !available() {
        return;
    }
    std::thread::Builder::new()
        .name("discord-activity".into())
        .spawn(move || run(app))
        .ok();
}

fn run(app: tauri::AppHandle) {
    let mut discords = Discords::default();
    let mut shown: Option<Shown> = None;
    let mut game_since: Option<i64> = None;
    let mut last_scan: Option<Instant> = None;
    // Kept for the same character when nothing states them for a while.
    let mut last_stats = LastStats::default();
    loop {
        std::thread::sleep(TICK);
        let Some(state) = app.try_state::<AppState>() else { continue };
        let enabled = state.settings.get(SETTING_KEY).as_deref() == Some("true");
        let playing = window_detector::find_aion2_window();
        if !playing {
            game_since = None;
        }
        if !enabled || !playing {
            if !discords.is_empty() {
                discords.clear_all();
                tracing::info!("Discord activity cleared");
            }
            shown = None;
            continue;
        }
        let since = *game_since.get_or_insert_with(crate::clock::now_ms);

        let lang = state.settings.get("dpsMeter.language").unwrap_or_else(|| "en".into());
        let text = Texts::load(i18n_dir(&state).as_ref(), &lang);
        let stat = Stat::from_setting(state.settings.get(STAT_KEY).as_deref());
        let next = describe(&state.data_storage, &state, &text, &lang, stat, &mut last_stats);

        // A Discord started after the game is picked up on a later scan, and
        // told the current activity even when it has not changed.
        let mut joined = false;
        if last_scan.is_none_or(|t| t.elapsed() >= RECONNECT_EVERY) {
            last_scan = Some(Instant::now());
            joined = discords.connect_new();
        }
        if discords.is_empty() || (shown.as_ref() == Some(&next) && !joined) {
            continue;
        }

        let mut assets = Assets::new().small_image("a2tools").small_text("A2Tools DPS Meter");
        // The image's hover text is the class with its level ("Cleric · Lv
        // 30"): the line beside it adds the name.
        if let Some(key) = next.class_key {
            assets = assets.large_image(key).large_text(next.class_line.as_str());
        }
        let mut activity = Activity::new()
            .details(next.details.as_str())
            .assets(assets)
            .timestamps(Timestamps::new().start(since));
        if !next.state.is_empty() {
            activity = activity.state(next.state.as_str());
        }
        discords.set_all(&activity);
        // Discord closed in the meantime: told again once it is back.
        shown = if discords.is_empty() { None } else { Some(next) };
    }
}

/// One Discord socket (`discord-ipc-0` … `-9`, also inside Flatpak and Snap
/// sandboxes). The crate speaks Discord's protocol over it; this only opens
/// it, as `platform::discord_ipc` finds it.
struct Socket {
    path: PathBuf,
    stream: Option<IpcStream>,
    /// The crate reads a message as an 8-byte header, then as many bytes as
    /// the header says: this read is the header.
    header_next: bool,
}

/// The longest reply taken from a socket. Discord's are a few KB; anything
/// can make a socket by that name (`/tmp/discord-ipc-1` on Linux, a pipe on
/// Windows), and the crate allocates whatever length the header claims, up
/// to 4 GiB (security audit M9).
const MAX_REPLY: u32 = 1 << 20;

impl DiscordIpc for Socket {
    fn get_client_id(&self) -> &str {
        DISCORD_APPLICATION_ID
    }

    fn connect_ipc(&mut self) -> IpcResult<()> {
        let stream = discord_ipc::connect(&self.path).map_err(|_| IpcError::IPCConnectionFailed)?;
        self.stream = Some(stream);
        self.header_next = true;
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> IpcResult<()> {
        let stream = self.stream.as_mut().ok_or(IpcError::NotConnected)?;
        stream.write_all(data).map_err(IpcError::WriteError)?;
        stream.flush().map_err(IpcError::FlushError)
    }

    fn read(&mut self, buffer: &mut [u8]) -> IpcResult<()> {
        let stream = self.stream.as_mut().ok_or(IpcError::NotConnected)?;
        stream.read_exact(buffer).map_err(IpcError::ReadError)?;
        let header = self.header_next;
        self.header_next = !header;
        if header && buffer.len() == 8 {
            let length = u32::from_le_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]);
            if length > MAX_REPLY {
                self.stream = None;
                return Err(IpcError::ReadError(std::io::Error::other(format!("a {length}-byte reply"))));
            }
        }
        Ok(())
    }

    fn close(&mut self) -> IpcResult<()> {
        self.stream = None;
        Ok(())
    }
}

/// How long a scan waits for the Discords it is connecting to.
const HANDSHAKE_WAIT: Duration = Duration::from_secs(5);

/// A connection attempt's outcome, from its own thread.
type Attempt = (Socket, Result<(), String>);

/// Every Discord running, one connection each.
struct Discords {
    open: Vec<Socket>,
    /// Sockets that refused the handshake, by which socket file it was: not
    /// tried again until a Discord makes a new one there.
    refused: HashMap<PathBuf, Option<u64>>,
    /// Handshakes still running. Each runs on its own thread: a Windows pipe
    /// has no read timeout, so a Discord that hangs mid-handshake holds only
    /// its own thread, never the others (and never this one past
    /// HANDSHAKE_WAIT). It is not tried again while it hangs; if it ever
    /// answers, a later scan takes it.
    pending: HashSet<PathBuf>,
    done_tx: Sender<Attempt>,
    done_rx: Receiver<Attempt>,
}

impl Default for Discords {
    fn default() -> Self {
        let (done_tx, done_rx) = mpsc::channel();
        Self { open: Vec::new(), refused: HashMap::new(), pending: HashSet::new(), done_tx, done_rx }
    }
}

impl Discords {
    fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// Connects to every Discord not connected yet; true when one was added.
    fn connect_new(&mut self) -> bool {
        let paths = discord_ipc::socket_paths();
        self.refused.retain(|p, _| paths.contains(p));
        let mut started = HashSet::new();
        for path in paths {
            if self.pending.contains(&path) || self.open.iter().any(|s| s.path == path) {
                continue;
            }
            let id = discord_ipc::socket_id(&path);
            if id.is_some() && self.refused.get(&path) == Some(&id) {
                continue;
            }
            let tx = self.done_tx.clone();
            let mut socket = Socket { path: path.clone(), stream: None, header_next: true };
            let spawned = std::thread::Builder::new().name("discord-ipc".into()).spawn(move || {
                let result = socket.connect().map_err(|e| e.to_string());
                let _ = tx.send((socket, result));
            });
            if spawned.is_ok() {
                self.pending.insert(path.clone());
                started.insert(path);
            }
        }
        // This scan's attempts, for up to HANDSHAKE_WAIT; any earlier one
        // that has finished meanwhile comes in too.
        let mut added = false;
        let deadline = Instant::now() + HANDSHAKE_WAIT;
        loop {
            let attempt = if started.is_empty() {
                match self.done_rx.try_recv() {
                    Ok(a) => a,
                    Err(_) => break,
                }
            } else {
                match self.done_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(a) => a,
                    Err(_) => break,
                }
            };
            let (socket, result) = attempt;
            self.pending.remove(&socket.path);
            started.remove(&socket.path);
            match result {
                Ok(()) => {
                    tracing::info!("Discord activity: connected to {}", socket.path.display());
                    self.refused.remove(&socket.path);
                    self.open.push(socket);
                    added = true;
                }
                // A socket left behind by a Discord that crashed.
                Err(e) => {
                    tracing::debug!("Discord activity: {} not reachable ({e})", socket.path.display());
                    let id = discord_ipc::socket_id(&socket.path);
                    self.refused.insert(socket.path, id);
                }
            }
        }
        for path in &started {
            tracing::info!("Discord activity: {} has not answered yet; leaving it", path.display());
        }
        added
    }

    fn set_all(&mut self, activity: &Activity) {
        self.open.retain_mut(|socket| match socket.set_activity(activity.clone()) {
            Ok(()) => true,
            // Discord closed or restarted: connect again on a later scan.
            Err(e) => {
                tracing::info!("Discord activity: lost {} ({e}); will reconnect", socket.path.display());
                false
            }
        });
    }

    fn clear_all(&mut self) {
        for mut socket in std::mem::take(&mut self.open) {
            let _ = socket.clear_activity();
            let _ = socket.close();
        }
    }
}

/// The class's art-asset key in the Discord application.
fn class_key(class: JobClass) -> &'static str {
    match class {
        JobClass::Gladiator => "gladiator",
        JobClass::Templar => "templar",
        JobClass::Ranger => "ranger",
        JobClass::Assassin => "assassin",
        JobClass::Sorcerer => "sorcerer",
        JobClass::Cleric => "cleric",
        // The asset is named for the class as the game calls it.
        JobClass::Elementalist => "spiritmaster",
        JobClass::Chanter => "chanter",
        JobClass::Fighter => "brawler",
    }
}

fn describe(
    storage: &Arc<DataStorage>,
    state: &AppState,
    text: &Texts,
    lang: &str,
    stat: Stat,
    last_stats: &mut LastStats,
) -> Shown {
    let profile = storage.local_profile();
    let stat_text = stat_text(&profile, stat, last_stats, text);
    let class_name = profile.class.map(|c| text.class(c));
    let mut details: Vec<String> = Vec::new();
    if let Some(name) = &class_name {
        details.push(name.clone());
    }
    if let Some(level) = profile.level {
        details.push(text.get("level", "Lv {level}").replace("{level}", &level.to_string()));
    }
    if let Some(t) = &stat_text {
        details.push(t.clone());
    }
    let class_line = if details.is_empty() { text.get("inGame", "In game") } else { details.join(" · ") };
    // The character's name leads: "Naicha · Cleric · Lv 30". With a combat
    // power or gear score the class gives way to it ("Naicha · Lv 30 · CP
    // 42k"): the profile popout cuts the line at about 30 characters, and the
    // class icon beside it (hover: class, level and the number) already says it.
    let short_line = match (&class_name, &stat_text) {
        (Some(_), Some(_)) if details.len() > 1 => details[1..].join(" · "),
        _ => class_line.clone(),
    };
    let details = match profile.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => format!("{name} · {short_line}"),
        None => class_line.clone(),
    };

    let state_line = fighting(storage, state)
        .map(|boss| text.get("fighting", "Fighting {boss}").replace("{boss}", &boss))
        .or_else(|| in_dungeon(storage.current_dungeon_id(), i18n_dir(state).as_ref(), text, lang))
        .unwrap_or_else(|| where_from(&profile, text, lang));

    Shown {
        details,
        class_line,
        state: state_line,
        class_key: profile.class.map(class_key),
        class_name,
    }
}

/// Which number follows the level.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Stat {
    CombatPower,
    GearScore,
    Off,
}

impl Stat {
    fn from_setting(value: Option<&str>) -> Self {
        match value {
            Some("combatPower") => Stat::CombatPower,
            Some("gearScore") => Stat::GearScore,
            _ => Stat::Off,
        }
    }
}

/// The last combat power and gear score seen, with the character's name.
#[derive(Default)]
struct LastStats {
    combat_power: Option<(String, i64)>,
    gear_score: Option<(String, i32)>,
}

/// "CP 109k" or "GS 2040", as chosen, for this character.
fn stat_text(profile: &LocalProfile, stat: Stat, last: &mut LastStats, text: &Texts) -> Option<String> {
    let name = profile.name.as_deref().map(str::trim).filter(|n| !n.is_empty())?;
    let combat_power = remembered(name, profile.combat_power, &mut last.combat_power);
    let gear_score = remembered(name, profile.gear_score, &mut last.gear_score);
    match stat {
        // In thousands, as the meter shows it beside names.
        Stat::CombatPower => combat_power.map(|cp| {
            let thousands = (cp as f64 / 1000.0).round() as i64;
            let cp = if thousands > 0 { format!("{thousands}k") } else { cp.to_string() };
            text.get("combatPower", "CP {cp}").replace("{cp}", &cp)
        }),
        Stat::GearScore => gear_score.map(|gs| text.get("gearScore", "GS {gs}").replace("{gs}", &gs.to_string())),
        Stat::Off => None,
    }
}

/// The value now, else the last one for the same character.
fn remembered<T: Copy>(name: &str, now: Option<T>, last: &mut Option<(String, T)>) -> Option<T> {
    if let Some(v) = now {
        *last = Some((name.to_string(), v));
        return Some(v);
    }
    last.as_ref().filter(|(n, _)| n == name).map(|(_, v)| *v)
}

/// The meter's translations: `i18n` inside its data folder. Without it the
/// class showed under its code name ("Elementalist" for a Spiritmaster).
fn i18n_dir(state: &AppState) -> Option<PathBuf> {
    state.i18n_data_dir.as_ref().map(|d| d.join("i18n"))
}

/// The boss being fought right now, by name, if any.
fn fighting(storage: &Arc<DataStorage>, state: &AppState) -> Option<String> {
    let target = storage.current_target();
    if target == 0 {
        return None;
    }
    let code = *storage.get_mob_data().get(&target)?;
    if !state.npc_lookup.is_boss(code) || state.npc_lookup.is_training_dummy(code) {
        return None;
    }
    let last_hit = storage.get_combat_snapshot_light().get(&target)?.last_damage_time;
    if crate::clock::now_ms() - last_hit > FIGHTING_WITHIN_MS {
        return None;
    }
    let name = state.npc_lookup.get_npc_name(code);
    (!name.is_empty()).then_some(name)
}

/// "Urugugu Canyon · Conquest (Normal)", for the instance the party roster
/// says you are in. Its last digit is the difficulty; dungeons with nine ids
/// number levels instead, and are named without one.
fn in_dungeon(dungeon_id: i32, dir: Option<&PathBuf>, text: &Texts, lang: &str) -> Option<String> {
    if dungeon_id <= 0 {
        return None;
    }
    let read = |l: &str| -> Option<serde_json::Value> {
        let path = dir?.join("dungeons").join(format!("{l}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    };
    let table = read(lang).or_else(|| read("en"))?;
    let table = table.as_object()?;
    let name = table.get(&dungeon_id.to_string())?.get("name")?.as_str()?.to_string();
    let group = dungeon_id - dungeon_id % 10;
    let ids_in_group = table
        .keys()
        .filter_map(|k| k.parse::<i32>().ok())
        .filter(|id| id - id % 10 == group)
        .count();
    let tier = if ids_in_group >= 9 {
        None
    } else {
        match dungeon_id % 10 {
            1 => Some(text.get("tierExpedition", "Expedition")),
            2 => Some(text.get("tierConquestNormal", "Conquest (Normal)")),
            3 => Some(text.get("tierConquestHard", "Conquest (Hard)")),
            _ => None,
        }
    };
    Some(match tier {
        Some(t) => format!("{name} · {t}"),
        None => name,
    })
}

/// "Kaisinel (Elyos) · Europe".
fn where_from(profile: &LocalProfile, text: &Texts, lang: &str) -> String {
    let sid = profile.server_id;
    if !(1000..3000).contains(&sid) {
        return String::new();
    }
    let table_lang = match lang {
        "zh-Hans" | "zh-Hant" => "zh-Hant",
        other => other,
    };
    let server = SERVERS
        .servers
        .get(&sid.to_string())
        .and_then(|names| names.get(table_lang).or_else(|| names.get("en")))
        .cloned();
    let faction = if sid / 1000 == 1 { text.get("elyos", "Elyos") } else { text.get("asmodian", "Asmodian") };
    let region = match sid / 100 % 10 {
        // Korea and Taiwan share these numbers; the meter's language and the
        // computer's time zone tell them apart, as for uploaded logs.
        0 => {
            let korean = lang == "ko" || chrono::Local::now().offset().local_minus_utc() == 9 * 3600;
            Some(if korean { text.get("korea", "Korea") } else { text.get("taiwan", "Taiwan") })
        }
        d => {
            let code = match d {
                1 => "NAE",
                2 => "NAW",
                3 => "EU",
                4 => "LA",
                5 => "AS",
                _ => "",
            };
            SERVERS
                .regions
                .get(table_lang)
                .and_then(|r| r.get(code))
                .or_else(|| SERVERS.regions.get("en").and_then(|r| r.get(code)))
                .cloned()
        }
    };
    let mut out = match server {
        Some(s) => format!("{s} ({faction})"),
        None => faction,
    };
    if let Some(r) = region {
        out = format!("{out} · {r}");
    }
    out
}

/// A class as the Global game names it in English, for when the meter's
/// strings can't be read.
fn english_class(class: JobClass) -> &'static str {
    match class {
        JobClass::Gladiator => "Gladiator",
        JobClass::Templar => "Templar",
        JobClass::Ranger => "Ranger",
        JobClass::Assassin => "Assassin",
        JobClass::Sorcerer => "Sorcerer",
        JobClass::Cleric => "Cleric",
        JobClass::Elementalist => "Spiritmaster",
        JobClass::Chanter => "Chanter",
        JobClass::Fighter => "Brawler",
    }
}

/// The `presence` and `classes` sections of the meter's own UI strings.
struct Texts {
    presence: HashMap<String, String>,
    classes: HashMap<String, String>,
}

impl Texts {
    fn load(dir: Option<&PathBuf>, lang: &str) -> Self {
        let read = |l: &str| -> Option<serde_json::Value> {
            let path = dir?.join("ui").join(format!("{l}.json"));
            serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
        };
        let doc = read(lang).or_else(|| read("en")).unwrap_or_default();
        let section = |name: &str| -> HashMap<String, String> {
            doc.get(name)
                .and_then(|v| v.as_object())
                .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
                .unwrap_or_default()
        };
        Self { presence: section("presence"), classes: section("classes") }
    }

    fn get(&self, key: &str, fallback: &str) -> String {
        self.presence.get(key).cloned().unwrap_or_else(|| fallback.to_string())
    }

    fn class(&self, class: JobClass) -> String {
        let key = match class {
            JobClass::Fighter => "FIGHTER".to_string(),
            JobClass::Elementalist => "ELEMENTALIST".to_string(),
            other => class_key(other).to_uppercase(),
        };
        self.classes.get(&key).cloned().unwrap_or_else(|| english_class(class).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts() -> Texts {
        Texts { presence: HashMap::new(), classes: HashMap::new() }
    }

    #[test]
    fn a_global_server_reads_as_name_faction_and_region() {
        let p = LocalProfile { server_id: 1304, ..Default::default() };
        assert_eq!(where_from(&p, &texts(), "en"), "Kaisinel (Elyos) · Europe");
        let p = LocalProfile { server_id: 2304, ..Default::default() };
        assert_eq!(where_from(&p, &texts(), "en"), "Lumiel (Asmodian) · Europe");
        assert_eq!(where_from(&LocalProfile::default(), &texts(), "en"), "");
    }

    #[test]
    fn a_dungeon_is_named_with_its_difficulty() {
        let dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../src/data/i18n"));
        assert_eq!(in_dungeon(600012, Some(&dir), &texts(), "en").as_deref(), Some("Urugugu Canyon · Conquest (Normal)"));
        assert_eq!(in_dungeon(600091, Some(&dir), &texts(), "en").as_deref(), Some("Ferocious Horn Den · Expedition"));
        assert_eq!(in_dungeon(0, Some(&dir), &texts(), "en"), None);
    }

    #[test]
    fn the_number_outlives_the_party_for_the_same_character() {
        let mut last = LastStats::default();
        let in_party = LocalProfile { name: Some("Naicha".into()), gear_score: Some(1520), combat_power: Some(41_600), ..Default::default() };
        assert_eq!(stat_text(&in_party, Stat::CombatPower, &mut last, &texts()).as_deref(), Some("CP 42k"));
        let solo = LocalProfile { name: Some("Naicha".into()), ..Default::default() };
        assert_eq!(stat_text(&solo, Stat::GearScore, &mut last, &texts()).as_deref(), Some("GS 1520"));
        assert_eq!(stat_text(&solo, Stat::CombatPower, &mut last, &texts()).as_deref(), Some("CP 42k"));
        assert_eq!(stat_text(&solo, Stat::Off, &mut last, &texts()), None);
        let other = LocalProfile { name: Some("Alt".into()), ..Default::default() };
        assert_eq!(stat_text(&other, Stat::CombatPower, &mut last, &texts()), None);
    }

    #[test]
    fn the_number_follows_the_setting() {
        assert_eq!(Stat::from_setting(None), Stat::Off);
        assert_eq!(Stat::from_setting(Some("combatPower")), Stat::CombatPower);
        assert_eq!(Stat::from_setting(Some("gearScore")), Stat::GearScore);
        assert_eq!(Stat::from_setting(Some("off")), Stat::Off);
        assert_eq!(Stat::from_setting(Some("nonsense")), Stat::Off);
    }

    #[test]
    fn classes_are_named_as_the_game_names_them() {
        let dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../src/data/i18n"));
        assert_eq!(Texts::load(Some(&dir), "en").class(JobClass::Elementalist), "Spiritmaster");
        assert_eq!(Texts::load(Some(&dir), "en").class(JobClass::Fighter), "Brawler");
        assert_eq!(Texts::load(Some(&dir), "ko").class(JobClass::Elementalist), "정령성");
        // Without the strings: still the game's English name, not the code's.
        assert_eq!(texts().class(JobClass::Elementalist), "Spiritmaster");
    }

    #[test]
    fn the_server_name_follows_the_language() {
        let p = LocalProfile { server_id: 1304, ..Default::default() };
        assert!(where_from(&p, &texts(), "ko").starts_with("카이시넬"));
    }
}
