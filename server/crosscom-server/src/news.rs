//! Server news (protocol 5002, OverlordNews): what the game shows in the main menu's Ghost Feed
//! and on its Community Messages screen.
//!
//! The game asks once per sign-in, twice over: GetNews(predicate, range) with a
//! `ProtoPredicateUbiNews` predicate for the game's news (up to 14), and with a
//! `ProtoPredicatePrincipals` one (its friends' pids) for news between players. Both answers land
//! in the same list, so only the first gets the news; the second gets none.
//!
//! The answer is a list of NewsMessage: u32 id, u32 recipient, u32 recipient type (1: from a
//! friend; 0: the game's), u32 publisher pid, String publisher name, DateTime published (the date
//! shown), DateTime display, DateTime expires (neither used), String title, String link (unused),
//! String body. A tag at the start of the body says which Ghost Feed view shows the item (the game
//! strips it): `[MP]` Multiplayer and Extras, `[SP]` Campaign, `[CIS]` Guerrilla; an untagged
//! item shows only under Community Messages, except the first, which shows in every view.
//!
//! The news is read from a TOML file the host edits, at each request, so changes need no restart:
//!
//! ```toml
//! [[news]]
//! title = "Double XP weekend"
//! body = "All weekend, every ranked match."
//! date = 2026-11-20
//! shows = "multiplayer"   # or "campaign", "guerrilla"; left out: everywhere if first
//! ```
//!
//! Without the file, one welcome item.

use std::io::Cursor;
use std::io::Read;
use std::path::PathBuf;

use byteorder::LittleEndian;
use byteorder::ReadBytesExt;
use quazal::prudp::ClientRegistry;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::rmc::Request;
use quazal::ClientInfo;
use quazal::Context;
use serde::Deserialize;
use serde::Serialize;
use slog::warn;
use slog::Logger;

/// The most items the game takes.
const MAX_ITEMS: usize = 14;
/// The game copies each string into a 1 KiB buffer that a longer one would overrun; these keep
/// well inside it (and what fits on its screens).
const MAX_TITLE_CHARS: usize = 60;
const MAX_BODY_BYTES: usize = 500;
const PUBLISHER: &str = "Cross-Com";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    news: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub title: String,
    pub body: String,
    /// The day it's shown as published; today if left out.
    #[serde(default)]
    pub date: Option<toml::value::Datetime>,
    /// Which Ghost Feed view shows it.
    #[serde(default)]
    pub shows: Option<Shows>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Shows {
    Multiplayer,
    Campaign,
    Guerrilla,
}

impl Shows {
    fn tag(self) -> &'static str {
        match self {
            Self::Multiplayer => "[MP]",
            Self::Campaign => "[SP]",
            Self::Guerrilla => "[CIS]",
        }
    }
}

fn welcome() -> Item {
    Item {
        title: "Welcome to Cross-Com".into(),
        body: format!(
            "You're playing on a Cross-Com Enhanced server ({}). Ranked and player matches, co-op, \
             Guerrilla, squads and challenges are all online again.",
            env!("CARGO_PKG_VERSION")
        ),
        date: None,
        shows: None,
    }
}

/// The news to show: the file's, or the welcome item when there's no file. A file that can't be
/// read or parsed gives no news (and a warning), rather than the welcome over the host's news.
pub fn load(path: &std::path::Path, logger: &Logger) -> Vec<Item> {
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<File>(&text) {
            Ok(file) => file.news,
            Err(e) => {
                warn!(logger, "{} isn't valid news: {}", path.display(), e.message());
                Vec::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![welcome()],
        Err(e) => {
            warn!(logger, "couldn't read {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day since 1970-01-01 (the inverse of `days_from_civil`).
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 1 to 12, 1 to 31
    (year, month as u32, day as u32)
}

fn unix(date: Option<&toml::value::Datetime>, now: u64) -> u64 {
    let Some(d) = date.and_then(|d| d.date) else {
        return now;
    };
    let days = days_from_civil(i64::from(d.year), u32::from(d.month), u32::from(d.day));
    u64::try_from(days * 86_400).unwrap_or(now)
}

/// Cuts a string to at most `max` bytes on a character boundary, without NULs.
fn cut(s: &str, max: usize) -> String {
    let s = s.replace('\0', "");
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn title(item: &Item) -> String {
    item.title.replace('\0', "").chars().take(MAX_TITLE_CHARS).collect()
}

fn body(item: &Item) -> String {
    cut(&item.body, MAX_BODY_BYTES)
}

/// A news item as the launcher shows it (`GET /api/info`).
#[derive(Debug, Serialize)]
pub struct Shown {
    pub title: String,
    pub body: String,
    /// `YYYY-MM-DD`: the item's date, or today's.
    pub date: String,
    pub shows: Option<Shows>,
}

/// The news the game shows, for the launcher: the same items, cut the same way.
pub fn shown(items: &[Item], now: u64) -> Vec<Shown> {
    items[..items.len().min(MAX_ITEMS)]
        .iter()
        .map(|item| {
            #[allow(clippy::cast_possible_wrap)] // seconds since 1970 fit an i64
            let (year, month, day) = civil_from_days((unix(item.date.as_ref(), now) / 86_400) as i64);
            Shown {
                title: title(item),
                body: body(item),
                date: format!("{year:04}-{month:02}-{day:02}"),
                shows: item.shows,
            }
        })
        .collect()
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    #[allow(clippy::cast_possible_truncation)] // every string is cut well below u16::MAX
    out.extend_from_slice(&((s.len() + 1) as u16).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// The GetNews answer for the game's news.
pub fn reply(items: &[Item], now: u64) -> Vec<u8> {
    let items = &items[..items.len().min(MAX_ITEMS)];
    let mut out = Vec::new();
    #[allow(clippy::cast_possible_truncation)] // at most MAX_ITEMS
    out.extend_from_slice(&(items.len() as u32).to_le_bytes());
    for (i, item) in items.iter().enumerate() {
        let when = crate::fixed::packed_date_time(unix(item.date.as_ref(), now));
        #[allow(clippy::cast_possible_truncation)]
        out.extend_from_slice(&(i as u32 + 1).to_le_bytes()); // id
        out.extend_from_slice(&0u32.to_le_bytes()); // recipient
        out.extend_from_slice(&0u32.to_le_bytes()); // recipient type: the game's news
        out.extend_from_slice(&0u32.to_le_bytes()); // publisher pid
        put_str(&mut out, PUBLISHER);
        for _ in 0..3 {
            out.extend_from_slice(&when.to_le_bytes()); // published, display, expires
        }
        put_str(&mut out, &title(item));
        put_str(&mut out, "");
        let tag = item.shows.map_or("", Shows::tag);
        put_str(&mut out, &format!("{tag}{}", body(item)));
    }
    out
}

/// The predicate's class name, the first thing GetNews sends.
fn predicate_class(params: &[u8]) -> Option<String> {
    let mut c = Cursor::new(params);
    let len = usize::from(c.read_u16::<LittleEndian>().ok()?);
    let mut bytes = vec![0; len];
    c.read_exact(&mut bytes).ok()?;
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(&bytes);
    String::from_utf8(bytes.to_vec()).ok()
}

pub struct OverlordNews {
    path: PathBuf,
}

impl OverlordNews {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl<CI> Protocol<CI> for OverlordNews {
    fn id(&self) -> u16 {
        5002
    }

    fn name(&self) -> String {
        "OverlordNewsProtocol".into()
    }

    fn num_methods(&self) -> u32 {
        1
    }

    fn handle(
        &self,
        logger: &Logger,
        _ctx: &Context,
        _ci: &mut ClientInfo<CI>,
        request: &Request,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<Vec<u8>, Error> {
        if request.method_id != 1 {
            return Err(Error::UnknownMethod);
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        match predicate_class(&request.parameters).as_deref() {
            Some("ProtoPredicateUbiNews") => Ok(reply(&load(&self.path, logger), now)),
            _ => Ok(reply(&[], now)),
        }
    }

    fn method_name(&self, method_id: u32) -> Option<String> {
        (method_id == 1).then(|| "GetNews".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_days_since_1970() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2026, 11, 15), 20_772);
        for days in [0, 11_017, 20_772, 59, 60, -1] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(20_772), (2026, 11, 15));
    }

    #[test]
    fn the_launcher_gets_the_same_news_as_json() {
        let file: File = toml::from_str(
            r#"
            [[news]]
            title = "Double XP"
            body = "All weekend."
            date = 2026-11-20
            shows = "multiplayer"
            [[news]]
            title = "Hi"
            body = "x"
            "#,
        )
        .unwrap();
        let json = serde_json::to_value(shown(&file.news, 20_772 * 86_400 + 3600)).unwrap();
        assert_eq!(
            json,
            serde_json::json!([
                { "title": "Double XP", "body": "All weekend.", "date": "2026-11-20", "shows": "multiplayer" },
                { "title": "Hi", "body": "x", "date": "2026-11-15", "shows": null },
            ])
        );
        assert_eq!(shown(&vec![file.news[1].clone(); 20], 0).len(), MAX_ITEMS);
    }

    #[test]
    fn the_file_gives_tagged_cut_items() {
        let file: File = toml::from_str(
            r#"
            [[news]]
            title = "Double XP"
            body = "All weekend."
            date = 2026-11-20
            shows = "multiplayer"
            [[news]]
            title = "Hi"
            body = "x"
            "#,
        )
        .unwrap();
        assert_eq!(file.news[0].shows, Some(Shows::Multiplayer));
        let out = reply(&file.news, 0);
        assert_eq!(&out[..4], &2u32.to_le_bytes());
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("[MP]All weekend."));
        assert!(text.contains("Double XP"));
        let first = 4 + 16 + 2 + PUBLISHER.len() + 1;
        let published = u64::from_le_bytes(out[first..first + 8].try_into().unwrap());
        assert_eq!(published, crate::fixed::packed_date_time(20_777 * 86_400));
    }

    #[test]
    fn long_text_is_cut_and_items_capped() {
        let item = Item {
            title: "t".repeat(200),
            body: "é".repeat(400),
            date: None,
            shows: None,
        };
        let out = reply(&vec![item; 20], 0);
        assert_eq!(&out[..4], &(MAX_ITEMS as u32).to_le_bytes());
        let title_at = 4 + 16 + 2 + PUBLISHER.len() + 1 + 24;
        assert_eq!(u16::from_le_bytes([out[title_at], out[title_at + 1]]) as usize, MAX_TITLE_CHARS + 1);
        let body_at = title_at + 2 + MAX_TITLE_CHARS + 1 + 2 + 1;
        assert_eq!(u16::from_le_bytes([out[body_at], out[body_at + 1]]) as usize, MAX_BODY_BYTES + 1);
    }

    #[test]
    fn only_the_game_news_predicate_gets_news() {
        let mut params = 22u16.to_le_bytes().to_vec();
        params.extend_from_slice(b"ProtoPredicateUbiNews\0");
        assert_eq!(predicate_class(&params).as_deref(), Some("ProtoPredicateUbiNews"));
        assert_eq!(predicate_class(&[5, 0, b'a']), None);
    }
}
