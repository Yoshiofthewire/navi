//! Caps on surface state a client can grow without drawing (job 036). They bound memory, not
//! the request-flood freeze: wayland-backend still drains a client's socket before serving
//! anyone else.

/// Damage rectangles a surface holds before they collapse to its whole buffer.
pub const DAMAGE: usize = 64;
/// Frame callbacks a surface holds; older extras complete at once.
pub const FRAMES: usize = 16;

/// Whether one more damage rectangle may join `list`. A full list becomes `[whole]`, which
/// then absorbs every rectangle until commit.
pub fn admit<T: PartialEq>(list: &mut Vec<T>, whole: T) -> bool {
    if list.first() == Some(&whole) {
        return false;
    }
    if list.len() < DAMAGE {
        return true;
    }
    *list = vec![whole];
    false
}

/// Collapses `list` to `[whole]` once it holds more than the cap.
pub fn cap_damage<T>(list: &mut Vec<T>, whole: T) {
    if list.len() > DAMAGE {
        *list = vec![whole];
    }
}

/// The oldest callbacks past the cap, in request order, taken out of `list`.
pub fn excess<T>(list: &mut Vec<T>) -> std::vec::Drain<'_, T> {
    list.drain(..list.len().saturating_sub(FRAMES))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHOLE: i32 = -1;

    fn damage(n: i32) -> Vec<i32> {
        let mut list = Vec::new();
        for r in 0..n {
            if admit(&mut list, WHOLE) {
                list.push(r);
            }
        }
        list
    }

    #[test]
    fn ac1_damage_holds_at_most_64_then_covers_the_whole_buffer() {
        assert_eq!(damage(63), (0..63).collect::<Vec<_>>());
        assert_eq!(damage(64), (0..64).collect::<Vec<_>>());
        assert_eq!(damage(65), [WHOLE]);
        assert_eq!(damage(10_000), [WHOLE]);
    }

    #[test]
    fn ac1_commit_collapses_damage_past_64() {
        for (n, want) in [(63, 63), (64, 64), (65, 1), (10_000, 1)] {
            let mut list: Vec<i32> = (0..n).collect();
            cap_damage(&mut list, WHOLE);
            assert_eq!(list.len(), want, "{n}");
            if want == 1 {
                assert_eq!(list, [WHOLE]);
            }
        }
    }

    #[test]
    fn ac1_frames_keep_16_and_complete_the_oldest_in_order() {
        for (n, done) in [(16, 0), (17, 1), (10_000, 9_984)] {
            let mut list: Vec<i32> = (0..n).collect();
            let gone: Vec<i32> = excess(&mut list).collect();
            assert_eq!(gone, (0..done).collect::<Vec<_>>(), "{n}");
            assert_eq!(list, (done..n).collect::<Vec<_>>(), "{n}");
        }
    }
}
