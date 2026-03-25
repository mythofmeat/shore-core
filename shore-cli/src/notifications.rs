use std::process::Command;

use shore_protocol::server_msg::NewMessage;

/// Send a desktop notification for a new push message using `notify-send`.
///
/// Best-effort: silently ignores failures (e.g., `notify-send` not installed,
/// not running on Linux, or no display server available).
pub fn notify_new_message(msg: &NewMessage) {
    let role = format!("{:?}", msg.message.role);
    let body = truncate(&msg.message.content, 200);
    let _ = Command::new("notify-send")
        .arg("--app-name=shore")
        .arg(format!("Shore — {role}"))
        .arg(body)
        .spawn();
}

/// Truncate a string to `max` characters, appending "…" if truncated.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let result = truncate("hello world", 5);
        assert_eq!(result, "hello…");
    }

    #[test]
    fn truncate_unicode() {
        let result = truncate("héllo wörld", 5);
        assert_eq!(result, "héllo…");
    }
}
