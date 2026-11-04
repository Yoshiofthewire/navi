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

/// What navi keeps of a `Notify` call. The icon, actions and hints have no field: the D-Bus glue
/// drops them, so nothing a notification names is ever opened or decoded.
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub app_name: String,
    pub replaces_id: u32,
    pub summary: String,
    pub body: String,
    pub expire_timeout: i32,
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
}

impl Default for Notes {
    fn default() -> Self {
        Notes {
            next: 1,
            shown: Vec::new(),
            recent: Vec::new(),
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
            app: sanitize(&r.app_name, APP_CHARS),
            summary: sanitize(&plain(&r.summary), SUMMARY_CHARS),
            body: sanitize(&plain(&r.body), BODY_CHARS),
            expires_ms: now.saturating_add(timeout_ms(r.expire_timeout)),
        };
        if let Some(at) = replaced {
            note.id = r.replaces_id;
            self.shown[at] = note;
            return (r.replaces_id, Some(Vec::new()));
        }
        note.id = self.fresh();
        let id = note.id;
        self.shown.insert(0, note);
        let evicted = self.shown.split_off(self.shown.len().min(MAX_SHOWN));
        (id, Some(evicted.iter().map(|n| (n.id, EXPIRED)).collect()))
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

    /// A click on its card.
    pub fn dismiss(&mut self, id: u32) -> Option<Closed> {
        let at = self.shown.iter().position(|n| n.id == id)?;
        self.shown.remove(at);
        Some((id, DISMISSED))
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

/// Markup made readable as plain text: simple tags (`<b>`, `</a>`, `<a href="…">`) removed, then
/// the five XML entities decoded. Nothing is interpreted, so `&lt;b&gt;` shows as `<b>`.
pub fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let named = after
            .trim_start_matches('/')
            .starts_with(|c: char| c.is_ascii_alphabetic());
        let end = after
            .find('>')
            .filter(|&e| named && !after[..e].contains('<'));
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
    fn ac11_dismiss() {
        let mut n = Notes::default();
        let (id, _) = n.notify(0, ":1.1", &req("a"));
        assert_eq!(n.dismiss(id), Some((id, DISMISSED)));
        assert!(n.shown().is_empty());
    }
}
