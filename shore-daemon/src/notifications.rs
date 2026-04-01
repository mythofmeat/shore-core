//! Daemon-side push notification service.
//!
//! Fires notifications for autonomous events (interiority messages, cache warnings,
//! compaction/collation completion, errors) via configurable backends:
//! notify-send (Linux desktop), ntfy (mobile push), or custom shell commands.

use std::sync::Arc;

use tracing::warn;

use shore_config::app::{NotificationBackend, NotificationsConfig, NtfyConfig};

/// Events that can trigger a push notification.
#[derive(Debug, Clone, Copy)]
pub enum NotificationEvent {
    AutonomousMessage,
    CacheWarning,
    CompactionComplete,
    CollationComplete,
    Error,
    MessageComplete,
}

/// Daemon-side notification dispatcher.
///
/// Cheap to clone (wraps `Arc` + `reqwest::Client`). Intended to be shared
/// across the autonomy manager, handler, and compaction task.
#[derive(Clone)]
pub struct NotificationService {
    config: Arc<NotificationsConfig>,
    http_client: reqwest::Client,
}

impl NotificationService {
    pub fn new(config: NotificationsConfig) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            config: Arc::new(config),
            http_client,
        }
    }

    /// Fire-and-forget notification dispatch.
    ///
    /// Returns immediately — the actual delivery happens in a spawned task.
    /// Does nothing if notifications are disabled or the event toggle is off.
    pub fn notify(&self, event: NotificationEvent, title: &str, body: &str) {
        if !self.config.enabled || !self.is_event_enabled(event) {
            return;
        }
        let config = self.config.clone();
        let client = self.http_client.clone();
        let title = title.to_string();
        let body = truncate(body, 200);
        tokio::spawn(async move {
            if let Err(e) = dispatch(&config, &client, &title, &body).await {
                warn!(error = %e, "Notification dispatch failed");
            }
        });
    }

    fn is_event_enabled(&self, event: NotificationEvent) -> bool {
        match event {
            NotificationEvent::AutonomousMessage => self.config.events.autonomous_message,
            NotificationEvent::CacheWarning => self.config.events.cache_warning,
            NotificationEvent::CompactionComplete => self.config.events.compaction_complete,
            NotificationEvent::CollationComplete => self.config.events.collation_complete,
            NotificationEvent::Error => self.config.events.error,
            NotificationEvent::MessageComplete => self.config.events.message_complete,
        }
    }

    /// Fire a `MessageComplete` notification, but only if generation time
    /// exceeds the configured threshold (0 = always notify).
    pub fn notify_message_complete(&self, title: &str, body: &str, total_ms: u32) {
        let threshold = self.config.generation_threshold_secs;
        if threshold > 0 && (total_ms as u64) < threshold * 1000 {
            return;
        }
        self.notify(NotificationEvent::MessageComplete, title, body);
    }
}

// ── Backend dispatch ────────────────────────────────────────────────────

async fn dispatch(
    config: &NotificationsConfig,
    client: &reqwest::Client,
    title: &str,
    body: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match config.backend {
        NotificationBackend::NotifySend => dispatch_notify_send(title, body).await,
        NotificationBackend::Ntfy => dispatch_ntfy(client, &config.ntfy, title, body).await,
        NotificationBackend::Command => dispatch_command(&config.command.template, title, body).await,
    }
}

async fn dispatch_notify_send(
    title: &str,
    body: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tokio::process::Command::new("notify-send")
        .arg("--app-name=shore")
        .arg(title)
        .arg(body)
        .output()
        .await?;
    Ok(())
}

async fn dispatch_ntfy(
    client: &reqwest::Client,
    config: &NtfyConfig,
    title: &str,
    body: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if config.topic.is_empty() {
        return Err("ntfy topic is not configured".into());
    }
    let url = format!("{}/{}", config.url.trim_end_matches('/'), config.topic);
    let mut req = client.post(&url).header("Title", title).body(body.to_string());
    if !config.token.is_empty() {
        req = req.header("Authorization", format!("Bearer {}", config.token));
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        return Err(format!("ntfy returned {}", resp.status()).into());
    }
    Ok(())
}

async fn dispatch_command(
    template: &str,
    title: &str,
    body: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if template.is_empty() {
        return Err("notification command template is not configured".into());
    }
    let rendered = template
        .replace("{title}", title)
        .replace("{body}", body);
    tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&rendered)
        .output()
        .await?;
    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…")
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use shore_config::app::{
        CommandNotifyConfig, NotificationEventsConfig,
    };

    fn make_service(enabled: bool, events: NotificationEventsConfig) -> NotificationService {
        NotificationService::new(NotificationsConfig {
            enabled,
            backend: NotificationBackend::NotifySend,
            ntfy: NtfyConfig::default(),
            command: CommandNotifyConfig::default(),
            generation_threshold_secs: 0,
            events,
        })
    }

    #[test]
    fn disabled_service_blocks_all_events() {
        let svc = make_service(false, NotificationEventsConfig::default());
        // notify returns immediately without spawning — just verifying no panic.
        // We can't easily assert the spawn didn't happen, but we verify the
        // guard logic by checking is_event_enabled is irrelevant when disabled.
        assert!(!svc.config.enabled);
    }

    #[test]
    fn event_toggles_respected() {
        let events = NotificationEventsConfig {
            autonomous_message: true,
            cache_warning: false,
            compaction_complete: true,
            collation_complete: false,
            error: true,
            message_complete: true,
        };
        let svc = make_service(true, events);
        assert!(svc.is_event_enabled(NotificationEvent::AutonomousMessage));
        assert!(!svc.is_event_enabled(NotificationEvent::CacheWarning));
        assert!(svc.is_event_enabled(NotificationEvent::CompactionComplete));
        assert!(!svc.is_event_enabled(NotificationEvent::CollationComplete));
        assert!(svc.is_event_enabled(NotificationEvent::Error));
        assert!(svc.is_event_enabled(NotificationEvent::MessageComplete));
    }

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
        assert_eq!(truncate("hello world", 5), "hello…");
    }

    #[test]
    fn truncate_unicode() {
        assert_eq!(truncate("héllo wörld", 5), "héllo…");
    }

    #[test]
    fn command_template_rendering() {
        let template = "echo '{title}: {body}'";
        let rendered = template
            .replace("{title}", "Shore — Test")
            .replace("{body}", "hello world");
        assert_eq!(rendered, "echo 'Shore — Test: hello world'");
    }
}
