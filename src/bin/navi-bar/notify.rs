//! Notifications as the freedesktop spec 1.2 has them, minus actions, markup and images: what is
//! shown, for how long, and what each call may change. Every field comes from any process on the
//! session bus, so it is sanitised before it is stored. Times are ms on an injected clock.

pub const MAX_SHOWN: usize = 3;
const APP_CHARS: usize = 64;
const SUMMARY_CHARS: usize = 120;
const BODY_CHARS: usize = 400;
const DEFAULT_MS: u64 = 5_000;
const MIN_MS: u64 = 1_000;
const MAX_MS: u64 = 10_000;
const WINDOW_MS: u64 = 10_000;
const PER_SENDER: usize = 5;
const GLOBAL: usize = 20;

/// Why a notification closed, as `NotificationClosed` says it.
pub const EXPIRED: u32 = 1;
pub const DISMISSED: u32 = 2;
pub const CLOSED: u32 = 3;

/// The sender of navi-bar's own cards.
pub const LOCAL: &str = "navi";

/// What navi keeps of a `Notify` call. The icon, actions and hints have no field: the D-Bus glue
/// drops them, so nothing a notification names is ever opened or decoded.
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub app_name: String,
    pub replaces_id: u32,
    pub summary: String,
    pub body: String,
    pub expire_timeout: i32,
    pub app_id: String,
    pub actions: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub id: u32,
    /// The unique bus name (`:1.x`) that sent it.
    pub sender: String,
    pub app: String,
    pub summary: String,
    pub body: String,
    pub expires_ms: u64,
    pub app_id: String,
    pub actions: Vec<(String, String)>,
}

/// A `NotificationClosed` to emit: id and reason.
pub type Closed = (u32, u32);

#[derive(Debug)]
pub struct Notes {
    next: u32,
    /// Newest first.
    shown: Vec<Note>,
    /// Calls let through in the last `WINDOW_MS`: sender and time.
    recent: Vec<(String, u64)>,
    pub history: Vec<Note>,
}

impl Default for Notes {
    fn default() -> Self {
        Notes {
            next: 1,
            shown: Vec::new(),
            recent: Vec::new(),
            history: Vec::new(),
        }
    }
}

impl Notes {
    pub fn shown(&self) -> &[Note] {
        &self.shown
    }

    /// The id the caller gets, whether it is shown (`Some` closes) or rate limited (`None`).
    pub fn notify(&mut self, now: u64, sender: &str, r: &Request) -> (u32, Option<Vec<Closed>>) {
        self.recent
            .retain(|&(_, t)| now.saturating_sub(t) < WINDOW_MS);
        let mine = self.recent.iter().filter(|(s, _)| s == sender).count();
        if mine >= PER_SENDER || self.recent.len() >= GLOBAL {
            return (self.fresh(), None);
        }
        self.recent.push((sender.to_string(), now));
        let (id, closed) = self.post(now, sender, r);
        (id, Some(closed))
    }

    /// navi closed a window past its client's limit: one card from `LOCAL`, past the rate limit
    /// (navi paces it). A repeat replaces the one shown. Bus senders start with `:`, so none can
    /// replace or close it.
    pub fn window_limit(&mut self, now: u64) -> (u32, Vec<Closed>) {
        let shown = self.shown.iter().find(|n| n.sender == LOCAL);
        let r = Request {
            app_name: "Navi".into(),
            replaces_id: shown.map_or(0, |n| n.id),
            summary: "Window limit reached".into(),
            body: format!(
                "An app tried to open more than {} windows. Close some of its windows to open more.",
                crate::control::CLIENT_TOPLEVELS
            ),
            expire_timeout: -1,
            ..Request::default()
        };
        self.post(now, LOCAL, &r)
    }

    fn post(&mut self, now: u64, sender: &str, r: &Request) -> (u32, Vec<Closed>) {
        let replaced = (r.replaces_id != 0)
            .then(|| {
                self.shown
                    .iter()
                    .position(|n| n.id == r.replaces_id && n.sender == sender)
            })
            .flatten();
        let mut note = Note {
            id: 0,
            sender: sender.to_string(),
            app: text(&r.app_name, APP_CHARS, false),
            summary: text(&r.summary, SUMMARY_CHARS, true),
            body: text(&r.body, BODY_CHARS, true),
            expires_ms: now.saturating_add(timeout_ms(r.expire_timeout)),
            app_id: sanitize(&r.app_id, 128),
            actions: r
                .actions
                .iter()
                .take(8)
                .map(|(k, v)| (sanitize(k, 64), sanitize(v, 64)))
                .collect(),
        };
        if let Some(at) = replaced {
            note.id = r.replaces_id;
            self.remember(&note);
            self.shown[at] = note;
            return (r.replaces_id, Vec::new());
        }
        note.id = self.fresh();
        let id = note.id;
        self.remember(&note);
        self.shown.insert(0, note);
        let evicted = self.shown.split_off(self.shown.len().min(MAX_SHOWN));
        (id, evicted.iter().map(|n| (n.id, EXPIRED)).collect())
    }

    fn remember(&mut self, note: &Note) {
        self.history.retain(|n| n.id != note.id);
        self.history.insert(0, note.clone());
        self.history.truncate(50);
    }
    pub fn hide(&mut self, id: u32) {
        self.shown.retain(|n| n.id != id);
    }
    pub fn clear_shown(&mut self) {
        self.shown.clear();
    }
    /// `CloseNotification`: only the sender's own notification closes.
    pub fn close(&mut self, sender: &str, id: u32) -> Option<Closed> {
        let at = self
            .shown
            .iter()
            .position(|n| n.id == id && n.sender == sender)?;
        self.shown.remove(at);
        Some((id, CLOSED))
    }

    /// A click on card `id` invokes its `default` action: the id and sender to signal, if it has
    /// one. A card without one sends nothing and asks for no token.
    pub fn default_action(&self, id: u32) -> Option<(u32, String)> {
        let n = self.shown.iter().find(|n| n.id == id)?;
        n.actions
            .iter()
            .any(|(k, _)| k == "default")
            .then(|| (n.id, n.sender.clone()))
    }

    /// A click on its card.
    pub fn dismiss(&mut self, id: u32) -> Option<Closed> {
        let at = self.shown.iter().position(|n| n.id == id)?;
        self.shown.remove(at);
        Some((id, DISMISSED))
    }

    /// Shown notes keep `by` ms more: the time they were hidden behind the session menu.
    pub fn delay(&mut self, by: u64) {
        for n in &mut self.shown {
            n.expires_ms = n.expires_ms.saturating_add(by);
        }
    }

    pub fn expire(&mut self, now: u64) -> Vec<Closed> {
        let mut out = Vec::new();
        self.shown.retain(|n| {
            let keep = n.expires_ms > now;
            if !keep {
                out.push((n.id, EXPIRED));
            }
            keep
        });
        out
    }

    /// From 1, wrapping past 0.
    fn fresh(&mut self) -> u32 {
        let id = self.next;
        self.next = self.next.checked_add(1).unwrap_or(1);
        id
    }
}

/// −1 is the server's default; anything else, 0 ("never") included, is clamped to 1–10 s.
fn timeout_ms(t: i32) -> u64 {
    match t {
        -1 => DEFAULT_MS,
        0 => MAX_MS,
        t => u64::try_from(t).unwrap_or(0).clamp(MIN_MS, MAX_MS),
    }
}

/// What is shown of a field: only its first `4 × max` chars are read, so the work per call is
/// bounded whatever the message size; markup is made `plain`, then the text sanitised.
fn text(s: &str, max: usize, markup: bool) -> String {
    let head = s.char_indices().nth(4 * max).map_or(s, |(at, _)| &s[..at]);
    match markup {
        true => sanitize(&plain(head), max),
        false => sanitize(head, max),
    }
}

/// Markup made readable as plain text, in one pass: simple tags (`<b>`, `</a>`, `<a href="…">`)
/// removed, then the five XML entities decoded. Nothing is interpreted, so `&lt;b&gt;` shows as
/// `<b>`.
pub fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let named = after
            .trim_start_matches('/')
            .starts_with(|c: char| c.is_ascii_alphabetic());
        // The scan stops at the next `<`, which is where the loop resumes: linear overall.
        let end = after
            .find(['<', '>'])
            .filter(|&e| named && after.as_bytes()[e] == b'>');
        match end {
            Some(e) => rest = &after[e + 1..],
            None => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    [
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&apos;", "'"),
        ("&amp;", "&"),
    ]
    .iter()
    .fold(out, |s, (from, to)| s.replace(from, to))
}

/// Plain text: controls except `\n` and bidi overrides removed, at most `max` chars with `…`
/// last when cut.
pub fn sanitize(s: &str, max: usize) -> String {
    let kept: Vec<char> = s
        .chars()
        .filter(|&c| c == '\n' || !(c.is_control() || bidi(c)))
        .take(max.saturating_add(1))
        .collect();
    if kept.len() <= max {
        return kept.into_iter().collect();
    }
    let mut out: String = kept[..max.saturating_sub(1)].iter().collect();
    out.push('…');
    out
}

fn bidi(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(summary: &str) -> Request {
        Request {
            app_name: "app".into(),
            summary: summary.into(),
            expire_timeout: -1,
            ..Request::default()
        }
    }

    fn ids(n: &Notes) -> Vec<u32> {
        n.shown().iter().map(|n| n.id).collect()
    }

    #[test]
    fn ac3_only_a_default_action_is_invoked_from_a_card() {
        let mut n = Notes::default();
        let (plain, _) = n.notify(0, ":1.1", &req("a"));
        let with = Request {
            actions: vec![("default".into(), "Open".into())],
            ..req("b")
        };
        let (id, _) = n.notify(0, ":1.2", &with);
        let other = Request {
            actions: vec![("reply".into(), "Reply".into())],
            ..req("c")
        };
        let (other, _) = n.notify(0, ":1.3", &other);
        assert_eq!(n.default_action(id), Some((id, ":1.2".into())));
        assert_eq!(n.default_action(plain), None);
        assert_eq!(n.default_action(other), None);
        assert_eq!(n.default_action(99), None, "no such card");
    }

    #[test]
    fn ac7_sanitize() {
        assert_eq!(
            sanitize("<b>x</b>", 120),
            "<b>x</b>",
            "sanitize leaves text alone"
        );
        assert_eq!(sanitize("a\u{202E}b\x07c\nd", 400), "abc\nd");
        assert_eq!(
            sanitize("\u{200E}\u{2066}\u{2069}\u{0085}\u{009F}\r\tx", 400),
            "x"
        );
        let long = sanitize(&"x".repeat(1000), BODY_CHARS);
        assert_eq!(long.chars().count(), 400);
        assert!(long.ends_with('…'));
        // Cut on a char boundary.
        let cut = sanitize(&"é😀".repeat(100), 5);
        assert_eq!(cut, "é😀é😀…");
        assert_eq!(sanitize("ab", 2), "ab");
    }

    #[test]
    fn markup_reads_as_plain_text() {
        assert_eq!(plain("Hello <b>navi</b>"), "Hello navi");
        assert_eq!(
            plain("<a href=\"https://x\">link</a> <i>it</i><br/>"),
            "link it"
        );
        assert_eq!(plain("a < b and c > d"), "a < b and c > d");
        assert_eq!(plain("1 <2 <b>3</b>"), "1 <2 3");
        assert_eq!(plain("open <b"), "open <b");
        assert_eq!(
            plain("&lt;b&gt; &amp;amp; &quot;q&quot; &apos;s&apos;"),
            "<b> &amp; \"q\" 's'"
        );
        assert_eq!(plain("x &nbsp; y"), "x &nbsp; y");
        let mut n = Notes::default();
        n.notify(
            0,
            ":1.1",
            &Request {
                body: "<b>line</b> one".into(),
                ..req("Hello <b>navi</b>")
            },
        );
        assert_eq!(
            (n.shown()[0].summary.as_str(), n.shown()[0].body.as_str()),
            ("Hello navi", "line one")
        );
    }

    /// Round 2: one huge `Notify` can't stall navi-bar's loop. The old `plain` took 9.3 s on a
    /// 1 MiB summary (release build).
    #[test]
    fn huge_markup_is_linear_and_capped() {
        let t = std::time::Instant::now();
        for s in [
            "<".repeat(1 << 20),
            "<a".repeat(1 << 19),
            "</".repeat(1 << 19),
        ] {
            let out = plain(&s);
            assert_eq!(out.len(), s.len(), "no tag closes, so all is kept");
        }
        // Linear: ~13 ms in release, ~0.6 s in a debug build; the quadratic version took minutes.
        let plain_ms = t.elapsed().as_millis();
        assert!(plain_ms < 2_000, "plain on 3 MiB took {plain_ms} ms");

        let t = std::time::Instant::now();
        let mut n = Notes::default();
        let flood = Request {
            app_name: "<".repeat(1 << 20),
            body: "<a".repeat(1 << 19),
            ..req(&"<".repeat(1 << 20))
        };
        n.notify(0, ":1.1", &flood);
        let notify_ms = t.elapsed().as_millis();
        assert!(notify_ms < 50, "notify took {notify_ms} ms");
        let shown = &n.shown()[0];
        assert_eq!(shown.app.chars().count(), APP_CHARS);
        assert_eq!(shown.summary.chars().count(), SUMMARY_CHARS);
        assert_eq!(shown.body.chars().count(), BODY_CHARS);
        assert!(shown.body.ends_with('…'));
        assert_eq!(text("<b>bold</b> text", 120, true), "bold text");
    }

    #[test]
    fn ac8_ids() {
        let mut n = Notes::default();
        assert_eq!(n.notify(0, ":1.1", &req("a")).0, 1);
        assert_eq!(n.notify(0, ":1.1", &req("b")).0, 2);
        n.next = u32::MAX;
        assert_eq!(n.fresh(), u32::MAX);
        assert_eq!(n.fresh(), 1, "0 is skipped");
    }

    #[test]
    fn ac8_replace_and_close_are_the_senders_own() {
        let mut n = Notes::default();
        let (id, _) = n.notify(0, ":1.1", &req("a"));
        let again = Request {
            replaces_id: id,
            ..req("a2")
        };
        assert_eq!(n.notify(0, ":1.1", &again), (id, Some(vec![])));
        assert_eq!(n.shown().len(), 1);
        assert_eq!(n.shown()[0].summary, "a2");
        // Another sender can't replace it: a new one.
        let (other, _) = n.notify(0, ":1.2", &again);
        assert_ne!(other, id);
        assert_eq!(ids(&n), [other, id]);
        assert_eq!(n.shown()[1].summary, "a2");
        // Or close it.
        assert_eq!(n.close(":1.2", id), None);
        assert_eq!(n.shown().len(), 2);
        assert_eq!(n.close(":1.1", id), Some((id, CLOSED)));
        assert_eq!(ids(&n), [other]);
        // Replacing a closed one gets a new id.
        assert_ne!(n.notify(0, ":1.1", &again).0, id);
    }

    #[test]
    fn hidden_time_is_given_back() {
        let mut n = Notes::default();
        n.notify(0, ":1.1", &req("a"));
        n.delay(3_000);
        assert_eq!(n.expire(5_000), vec![], "hidden 3 s of its 5");
        assert_eq!(n.expire(8_000).len(), 1);
    }

    #[test]
    fn ac8_timeouts_and_eviction() {
        let expiry = |t| {
            let mut n = Notes::default();
            n.notify(
                100,
                ":1.1",
                &Request {
                    expire_timeout: t,
                    ..req("a")
                },
            );
            n.shown()[0].expires_ms - 100
        };
        assert_eq!(expiry(-1), 5_000);
        assert_eq!(expiry(0), 10_000);
        assert_eq!(expiry(60_000), 10_000);
        assert_eq!(expiry(1), 1_000);
        assert_eq!(expiry(-7), 1_000);
        assert_eq!(expiry(2_500), 2_500);

        let mut n = Notes::default();
        for s in ["a", "b", "c"] {
            assert_eq!(n.notify(0, ":1.1", &req(s)).1, Some(vec![]));
        }
        assert_eq!(
            n.notify(0, ":1.1", &req("d")),
            (4, Some(vec![(1, EXPIRED)]))
        );
        assert_eq!(ids(&n), [4, 3, 2], "newest first");
        assert_eq!(n.expire(4_999), vec![]);
        assert_eq!(
            n.expire(5_000),
            vec![(4, EXPIRED), (3, EXPIRED), (2, EXPIRED)]
        );
        assert!(n.shown().is_empty());
        assert_eq!(n.dismiss(4), None);
    }

    #[test]
    fn ac8_rate_limits() {
        let mut n = Notes::default();
        for k in 0..5 {
            assert!(n.notify(k, ":1.1", &req("x")).1.is_some());
        }
        let shown = ids(&n);
        let (id, closes) = n.notify(9_999, ":1.1", &req("flood"));
        assert_eq!(
            (id, closes),
            (6, None),
            "a fresh id, nothing shown or emitted"
        );
        assert_eq!(ids(&n), shown);
        // Another sender still gets through, and the first again 10 s after its first call.
        assert!(n.notify(9_999, ":1.2", &req("x")).1.is_some());
        assert!(n.notify(10_000, ":1.1", &req("x")).1.is_some());

        let mut n = Notes::default();
        for k in 0..20 {
            assert!(n.notify(0, &format!(":1.{k}"), &req("x")).1.is_some());
        }
        assert_eq!(n.notify(9_999, ":1.99", &req("x")).1, None, "the 21st");
        assert!(n.notify(10_000, ":1.99", &req("x")).1.is_some());
    }

    #[test]
    fn a_window_limit_flood_shows_one_card_no_bus_client_can_close() {
        let mut n = Notes::default();
        for k in 0..10 {
            n.window_limit(k * 1_000);
        }
        let [card] = n.shown() else {
            panic!("one card: {:?}", n.shown());
        };
        let id = card.id;
        assert_eq!(card.sender, LOCAL);
        assert_eq!(card.summary, "Window limit reached");
        assert_eq!(
            card.body,
            "An app tried to open more than 256 windows. Close some of its windows to open more."
        );
        assert!(card.actions.is_empty());
        assert_eq!(card.expires_ms, 9_000 + DEFAULT_MS);
        for sender in [":1.1", ":1.99", ":navi"] {
            assert_eq!(n.close(sender, id), None);
        }
        // Two, so the card isn't simply pushed out past `MAX_SHOWN`.
        for sender in [":1.1", ":navi"] {
            let replace = Request {
                replaces_id: id,
                ..req("spoof")
            };
            n.notify(9_000, sender, &replace);
        }
        assert!(n.shown().iter().any(|c| c.id == id && c.sender == LOCAL));
        assert_eq!(n.shown().iter().filter(|c| c.sender == LOCAL).count(), 1);
    }

    #[test]
    fn ac11_dismiss() {
        let mut n = Notes::default();
        let (id, _) = n.notify(0, ":1.1", &req("a"));
        assert_eq!(n.dismiss(id), Some((id, DISMISSED)));
        assert!(n.shown().is_empty());
    }
}
