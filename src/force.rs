//! Force close: the 90 s gate after a user's close, and what a confirmed force close may end.
//! navi re-checks both itself; navi-bar's view is never trusted.

use std::time::{Duration, Instant};

/// How long a user's first close must go unanswered before the window may be force closed.
pub const FORCE_AFTER: Duration = Duration::from_secs(90);

/// Whether a window first asked to close at `close_sent` (and still open) is overdue at `now`.
pub fn overdue(close_sent: Option<Instant>, now: Instant) -> bool {
    close_sent.is_some_and(|t| now.saturating_duration_since(t) >= FORCE_AFTER)
}

/// Who owns the window a force close names. `F` is the client's pidfd.
#[derive(Debug, PartialEq)]
pub enum Owner<F> {
    X11 {
        window: u32,
    },
    Wayland {
        /// navi-bar, the locker or Xwayland's own connection.
        protected: bool,
        /// The peer's pid and its pidfd, captured at accept.
        pid: Option<u32>,
        pidfd: Option<F>,
    },
}

/// The window, as navi knows it.
#[derive(Debug, PartialEq)]
pub struct Target<F> {
    pub close_sent: Option<Instant>,
    pub owner: Owner<F>,
}

/// What is true of navi when the request comes.
#[derive(Debug, Default)]
pub struct Facts {
    pub locked: bool,
    /// navi's own pid; every socketpair client reports it.
    pub own_pid: u32,
    /// navi-bar's and the locker's child pids, and the peer pids of protected clients.
    pub protected_pids: Vec<u32>,
}

#[derive(Debug, PartialEq)]
pub enum Plan<F> {
    /// XKillClient on `window`'s X client; never a signal.
    X11 { window: u32, client: u32 },
    /// Disconnect the client, after SIGKILL through its pidfd when `signal` is set.
    Wayland { signal: Option<F> },
}

#[derive(Debug, PartialEq)]
pub enum Refused {
    Locked,
    NotOverdue,
    Protected,
}

/// What a confirmed force close of `target` (`None`: no such handle) does at `now`.
pub fn plan<F>(target: Option<Target<F>>, now: Instant, facts: &Facts) -> Result<Plan<F>, Refused> {
    if facts.locked {
        return Err(Refused::Locked);
    }
    let Some(t) = target.filter(|t| overdue(t.close_sent, now)) else {
        return Err(Refused::NotOverdue);
    };
    match t.owner {
        Owner::X11 { window } => Ok(Plan::X11 {
            window,
            client: crate::xwayland::x_client(window),
        }),
        Owner::Wayland {
            protected: true, ..
        } => Err(Refused::Protected),
        Owner::Wayland { pid, pidfd, .. } => {
            let ours = |p: u32| p <= 1 || p == facts.own_pid || facts.protected_pids.contains(&p);
            let signal = pidfd.filter(|_| pid.is_some_and(|p| !ours(p)));
            Ok(Plan::Wayland { signal })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn ac9_overdue_90_s_after_the_first_close() {
        let t = Instant::now();
        assert!(!overdue(None, t + S(1000)), "no close");
        assert!(!overdue(Some(t), t + S(90) - Duration::from_millis(1)));
        assert!(overdue(Some(t), t + S(90)));
        // A second close at 60 s doesn't move the stamp: `Managed` keeps the first.
        let mut stamp = None;
        for at in [t, t + S(60)] {
            stamp = stamp.or(Some(at));
        }
        assert!(overdue(stamp, t + S(90)));
        // Each window on its own clock.
        let (a, b) = (Some(t), Some(t + S(30)));
        assert!(overdue(a, t + S(90)) && !overdue(b, t + S(90)));
        assert!(overdue(b, t + S(120)));
        // A clock that went backwards is never overdue.
        assert!(!overdue(Some(t + S(5)), t));
    }

    fn wayland(protected: bool, pid: Option<u32>, pidfd: Option<&str>) -> Owner<&str> {
        Owner::Wayland {
            protected,
            pid,
            pidfd,
        }
    }

    #[test]
    fn ac10_plan_follows_every_rule() {
        let t = Instant::now();
        let now = t + FORCE_AFTER;
        let facts = Facts {
            locked: false,
            own_pid: 4000,
            protected_pids: vec![4100, 4200, 4300],
        };
        let target = |owner| {
            Some(Target {
                close_sent: Some(t),
                owner,
            })
        };
        let plan = |target, facts: &Facts| plan(target, now, facts);
        let app = || wayland(false, Some(5000), Some("fd"));

        let locked = Facts {
            locked: true,
            ..Facts::default()
        };
        assert_eq!(plan(target(app()), &locked), Err(Refused::Locked));
        assert_eq!(plan(None, &facts), Err(Refused::NotOverdue), "unknown");
        let fresh = Target {
            close_sent: Some(now),
            owner: app(),
        };
        assert_eq!(plan(Some(fresh), &facts), Err(Refused::NotOverdue));
        let never = Target {
            close_sent: None,
            owner: app(),
        };
        assert_eq!(plan(Some(never), &facts), Err(Refused::NotOverdue));

        let window = 0x0060_0007;
        assert_eq!(
            plan(target(Owner::X11 { window }), &facts),
            Ok(Plan::X11 {
                window,
                client: 0x0060_0000
            })
        );
        // navi-bar, the locker and Xwayland: whatever pid they report.
        assert_eq!(
            plan(target(wayland(true, Some(5000), Some("fd"))), &facts),
            Err(Refused::Protected)
        );
        // Disconnected but never signalled.
        for pid in [
            Some(0),
            Some(1),
            Some(4000),
            Some(4100),
            Some(4200),
            Some(4300),
            None,
        ] {
            assert_eq!(
                plan(target(wayland(false, pid, Some("fd"))), &facts),
                Ok(Plan::Wayland { signal: None }),
                "{pid:?}"
            );
        }
        assert_eq!(
            plan(target(wayland(false, Some(5000), None)), &facts),
            Ok(Plan::Wayland { signal: None }),
            "no pidfd"
        );
        assert_eq!(
            plan(target(app()), &facts),
            Ok(Plan::Wayland { signal: Some("fd") })
        );
    }
}
