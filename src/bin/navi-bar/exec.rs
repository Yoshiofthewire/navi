//! A `.desktop` `Exec=` value split into argv, per the Desktop Entry Spec, for running without a
//! shell. Anything a shell would interpret is rejected, never guessed at.

/// Field codes navi-bar drops: it passes no files, URLs, icon or name.
const DROPPED: &str = "fFuUdDnNickvm";
/// Outside quotes these mean something to a shell, so an entry using them is rejected.
const RESERVED: &str = "`$;&|<>()";

/// The argv of an unescaped `Exec=` value, or why it's refused.
pub fn split(exec: &str) -> Result<Vec<String>, String> {
    let mut argv = Vec::new();
    let mut arg = String::new();
    // An argument is kept if it has text or quotes, so one of field codes alone is dropped.
    let (mut quoted, mut in_quotes) = (false, false);
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if c.is_control() {
            return Err("a control character".into());
        }
        match c {
            '"' => {
                in_quotes = !in_quotes;
                quoted = true;
            }
            '\\' if in_quotes => match chars.next() {
                Some(e @ ('"' | '`' | '$' | '\\')) => arg.push(e),
                _ => return Err("a bad escape in quotes".into()),
            },
            '%' => match chars.next() {
                Some('%') => arg.push('%'),
                Some(f) if DROPPED.contains(f) => {}
                Some(f) => return Err(format!("unknown field code %{f}")),
                None => return Err("a lone %".into()),
            },
            ' ' if !in_quotes => {
                if quoted || !arg.is_empty() {
                    argv.push(std::mem::take(&mut arg));
                }
                quoted = false;
            }
            c if !in_quotes && RESERVED.contains(c) => {
                return Err(format!("{c:?} outside quotes"));
            }
            c => arg.push(c),
        }
    }
    if in_quotes {
        return Err("an unterminated quote".into());
    }
    if quoted || !arg.is_empty() {
        argv.push(arg);
    }
    match argv.first() {
        Some(p) if !p.is_empty() => Ok(argv),
        _ => Err("no program".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::unescape;

    /// As read from a file: string escapes first, then the split.
    fn argv(raw: &str) -> Result<Vec<String>, String> {
        split(&unescape(raw))
    }

    #[test]
    fn ac3_splits_like_the_spec() {
        let ok = |raw: &str, want: &[&str]| assert_eq!(argv(raw).unwrap(), want, "{raw}");
        ok("konsole", &["konsole"]);
        ok("firefox %u", &["firefox"]);
        ok(
            r#""/opt/My App/run" --x=%f -y"#,
            &["/opt/My App/run", "--x=", "-y"],
        );
        ok(r#"a "b \"c\" \\$d""#, &["a", r#"b "c" $d"#]);
        ok("echo 100%%", &["echo", "100%"]);
        ok("  a   b ", &["a", "b"]);
        ok(r#"a "" b"#, &["a", "", "b"]);
    }

    #[test]
    fn ac3_rejects_what_a_shell_would_interpret() {
        for raw in [
            "a %z",
            r#"a "b"#,
            "a; rm -rf ~",
            "a $(x)",
            "a | b",
            "a `x`",
            "a\u{7}b",
            "a\tb",
            "",
            "   ",
            "%f %U",
            r#""" a"#,
            "a %",
            r#"a "\x""#,
        ] {
            assert!(argv(raw).is_err(), "{raw:?} accepted: {:?}", argv(raw));
        }
    }
}
