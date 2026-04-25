use std::io::{self, Write};

use crossterm::style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor};

use super::transcript::character_color;
use super::{
    abbreviate_model, parse_timestamp, print_dim_line, term_width, use_color, write_dim, write_fg,
    write_row, write_row_colored, write_section_header,
};

// ---------------------------------------------------------------------------
// Status formatter -- human-readable dashboard
// ---------------------------------------------------------------------------

/// Translate a heartbeat state string to a human-readable description.
fn heartbeat_description(state: &str, ticks: u64, max_ticks: u64) -> String {
    match state {
        "Active" if ticks == 0 => "active \u{2014} in conversation".to_string(),
        "Active" => format!("active \u{2014} idle {ticks}/{max_ticks} ticks"),
        "Dormant" => "dormant \u{2014} waiting for you".to_string(),
        other => other.to_string(),
    }
}

/// Map a normalized density (0.0-1.0) to a bar character.
///
/// Uses 8 Unicode block elements for non-zero values and a light shade for
/// effectively-zero values.
fn density_to_block(normalized: f64) -> char {
    const BLOCKS: [char; 8] = [
        '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];
    if normalized < 0.05 {
        '\u{2591}'
    } else {
        let idx = ((normalized * 7.0).round() as usize).min(7);
        BLOCKS[idx]
    }
}

/// Color for an hour classification label.
fn classification_color(class: &str) -> Color {
    match class {
        "peak" => Color::Cyan,
        "trough" => Color::DarkGrey,
        _ => Color::White,
    }
}

/// Write the activity heatmap section into the status dashboard.
///
/// Renders a 24-character bar chart (one block per hour) with hour labels
/// underneath, plus engagement and session stats.
fn write_activity_section(out: &mut impl Write, activity: &serde_json::Value, width: usize) {
    let histogram: Vec<f64> = match activity["hour_histogram"].as_array() {
        Some(arr) => arr.iter().filter_map(|v| v.as_f64()).collect(),
        None => return,
    };
    if histogram.len() != 24 {
        return;
    }
    let classifications: Vec<String> = activity["hour_classifications"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if classifications.len() != 24 {
        return;
    }

    let sufficient = activity["has_sufficient_heatmap"]
        .as_bool()
        .unwrap_or(false);
    let suffix = if sufficient { "" } else { "sparse" };
    write_section_header(out, "Activity", suffix, width);

    // -- bar chart row --
    let max_val = histogram.iter().cloned().fold(0.0_f64, f64::max);
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = write!(out, "  {:<13}", "");
    for (i, &density) in histogram.iter().enumerate() {
        let linear = if max_val > 0.0 {
            density / max_val
        } else {
            0.0
        };
        // Log scale: ln(1 + x*k) / ln(1+k) -- spreads low values, compresses peaks.
        let normalized = (1.0 + linear * 9.0).ln() / 10.0_f64.ln();
        let ch = density_to_block(normalized);
        if use_color() {
            let color = classification_color(&classifications[i]);
            let _ = crossterm::execute!(out, SetForegroundColor(color));
        }
        let _ = write!(out, "{ch}");
    }
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out);

    // -- hour labels row --
    //    0  3  6  9  12 15 18 21
    if use_color() {
        let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
    }
    let _ = write!(out, "  {:<13}0  3  6  9  12 15 18 21", "");
    if use_color() {
        let _ = crossterm::execute!(out, ResetColor);
    }
    let _ = writeln!(out);

    // -- stats row --
    let engagement = activity["engagement_score"].as_f64().unwrap_or(0.0);
    let sessions = activity["sessions_per_day"].as_f64().unwrap_or(0.0);
    let msg_count = activity["message_count"].as_u64().unwrap_or(0);
    write_row(
        out,
        "Engagement",
        &format!("{engagement:.2} \u{00b7} {sessions:.1} sessions/day \u{00b7} {msg_count} msgs"),
    );

    let _ = writeln!(out);
}

/// Print the status dashboard.
pub fn print_status(data: &serde_json::Value, character_name: &str) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    // -- Status --
    write_section_header(&mut out, "Status", "", width);

    // Prefer the character name from the daemon response over the CLI fallback.
    let effective_name = data["character"].as_str().unwrap_or(character_name);
    let char_color = character_color(effective_name);
    write_row_colored(&mut out, "Character", effective_name, char_color);

    let model = data["active_model"].as_str().unwrap_or("(none)");
    write_row(&mut out, "Model", abbreviate_model(model));

    if let Some(count) = data["message_count"].as_u64() {
        write_row(&mut out, "Messages", &count.to_string());
    }

    let pending_deferred_edit_count = data["pending_deferred_edit_count"].as_u64().unwrap_or(0);
    if pending_deferred_edit_count > 0 {
        let paths: Vec<&str> = data["pending_deferred_edits"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|path| path.as_str())
            .collect();
        let label = if pending_deferred_edit_count == 1 {
            "1 pending".to_string()
        } else {
            format!("{pending_deferred_edit_count} pending")
        };
        let detail = if paths.is_empty() {
            label
        } else {
            format!("{label}: {}", paths.join(", "))
        };
        write_row(&mut out, "Prompt Edits", &detail);
    }

    let _ = writeln!(out);

    // -- Clients --
    if let Some(clients) = data.get("clients").and_then(|c| c.as_array()) {
        if !clients.is_empty() {
            write_section_header(&mut out, "Clients", "", width);
            for client in clients {
                let ctype = client["client_type"].as_str().unwrap_or("?");
                let cname = client["client_name"].as_str().unwrap_or("?");
                write_row(&mut out, ctype, cname);
            }
            let _ = writeln!(out);
        }
    }

    // -- Autonomy --
    if let Some(autonomy) = data.get("autonomy") {
        if !autonomy.is_null() {
            let paused = autonomy["paused"].as_bool().unwrap_or(false);
            let suffix = if paused { "paused" } else { "" };
            write_section_header(&mut out, "Autonomy", suffix, width);

            let int_state = autonomy["heartbeat_state"].as_str().unwrap_or("Active");
            let ticks = autonomy["ticks_without_user"].as_u64().unwrap_or(0);
            let max_ticks = autonomy["dormant_after_heartbeat_turns"]
                .as_u64()
                .unwrap_or(3);
            let description = heartbeat_description(int_state, ticks, max_ticks);

            // Heartbeat row: description + state label.
            if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            let _ = write!(out, "  {:<13}", "Heartbeat");
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
            let _ = write!(out, "{description}  ");
            if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            let _ = write!(out, "({int_state})");
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
            let _ = writeln!(out);

            // Effective tick interval.
            if let Some(eff) = autonomy["effective_interval_secs"].as_u64() {
                let mins = eff / 60;
                let secs = eff % 60;
                let label = if secs == 0 {
                    format!("{mins}m")
                } else {
                    format!("{mins}m{secs}s")
                };
                write_row(&mut out, "Interval", &label);
            }

            let _ = writeln!(out);
        }
    }

    // -- Activity --
    if let Some(activity) = data.get("activity") {
        if !activity.is_null() {
            let msg_count = activity["message_count"].as_u64().unwrap_or(0);
            if msg_count > 0 {
                write_activity_section(&mut out, activity, width);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Command-specific formatters
// ---------------------------------------------------------------------------

/// Dispatch a command response to the appropriate formatter.
/// Falls back to generic JSON output for unknown command names.
pub fn format_command(name: &str, data: &serde_json::Value) {
    match name {
        "character_info" => print_character_info(data),
        "list_models" => print_model_list(data),
        "switch_model" => print_model_switched(data),
        "reset_model" => print_model_reset(data),
        "model_info" => print_model_info(data),
        "set_reasoning_effort" => print_reasoning_effort(data),
        "memory" => print_memory(data),
        "compact" => print_compact_result(data),
        "memory_changelog" => print_changelog(data),
        "memory_dream" => print_memory_dream(data),
        "config" => print_config(data),
        "config_check" => print_config_check(data),
        "config_reset" => print_config_reset(data),
        "edit" => print_edit_confirmation(data),
        "delete" => print_delete_confirmation(data),
        "inject_system" => println!("System instruction injected."),
        "diagnostics" => print_diagnostics(data),
        "usage" => print_usage(data),
        "heartbeat_tick_now" => print_heartbeat_tick_now(data),
        "heartbeat_set_dormant" => print_heartbeat_status_change(data, "dormant"),
        "heartbeat_set_active" => print_heartbeat_status_change(data, "active"),
        _ => print_command_output_fallback(name, data),
    }
}

fn print_memory_dream(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();
    let char_name = data["character"].as_str().unwrap_or("?");
    write_section_header(&mut out, "Dreaming", char_name, width);

    if data.get("state_path").is_some() {
        write_row(
            &mut out,
            "Enabled",
            if data["enabled"].as_bool().unwrap_or(false) {
                "yes"
            } else {
                "no"
            },
        );
        write_row(
            &mut out,
            "Frequency",
            data["frequency"].as_str().unwrap_or("?"),
        );
        write_row(
            &mut out,
            "Due",
            if data["due"].as_bool().unwrap_or(false) {
                "yes"
            } else {
                "no"
            },
        );
        if let Some(last) = data["last_run_at"].as_str() {
            write_row(&mut out, "Last run", last);
        }
    } else if data["status"].as_str() == Some("not_due") {
        write_row(&mut out, "Status", "not due");
    } else {
        let dry = data["dry_run"].as_bool().unwrap_or(false);
        write_row(&mut out, "Status", if dry { "dry run" } else { "ran" });
        let candidates = data["candidates"].as_array().map_or(0, Vec::len);
        let promoted = data["promoted"].as_array().map_or(0, Vec::len);
        write_row(&mut out, "Candidates", &candidates.to_string());
        write_row(&mut out, "Promoted", &promoted.to_string());
    }
    let _ = writeln!(out);
}

fn print_command_output_fallback(name: &str, data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if use_color() {
        let _ = crossterm::execute!(out, SetAttribute(Attribute::Bold));
    }
    let _ = write!(out, "{name}");
    if use_color() {
        let _ = crossterm::execute!(out, SetAttribute(Attribute::Reset));
    }
    let _ = writeln!(out);
    if let Ok(pretty) = serde_json::to_string_pretty(data) {
        let _ = writeln!(out, "{pretty}");
    }
}

fn print_heartbeat_tick_now(data: &serde_json::Value) {
    let character = data["character"].as_str().unwrap_or("?");
    println!("Tick scheduled for {character}.");
    if let Some(warning) = data["warning"].as_str() {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        write_fg(&mut out, Color::Yellow, warning);
        let _ = writeln!(out);
    }
}

fn print_heartbeat_status_change(data: &serde_json::Value, status: &str) {
    let character = data["character"].as_str().unwrap_or("?");
    println!("Heartbeat forced {status} for {character}.");
}

/// Print edit confirmation.
fn print_edit_confirmation(data: &serde_json::Value) {
    let msg_ref = data["ref"].as_str().unwrap_or("?");
    println!("Edited message {msg_ref}");
}

/// Print delete confirmation.
fn print_delete_confirmation(data: &serde_json::Value) {
    if let Some(arr) = data["deleted"].as_array() {
        let n = arr.len();
        if n == 1 {
            let id = arr[0].as_str().unwrap_or("?");
            println!("Deleted message {id}");
        } else {
            println!("Deleted {n} messages");
        }
    } else if let Some(id) = data["deleted"].as_str() {
        println!("Deleted message {id}");
    }
}

/// Print model list.
fn print_model_list(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let active = data["active"].as_str().unwrap_or("");

    write_section_header(&mut out, "Models", "", width);

    if let Some(models) = data["models"].as_array() {
        for m in models {
            let name = m["name"].as_str().unwrap_or("?");
            let provider = m["provider"].as_str().unwrap_or("?");
            let is_active = name == active || m["qualified_name"].as_str() == Some(active);

            let marker = if is_active { "*" } else { " " };

            if use_color() && is_active {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::Cyan));
            } else if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            let _ = write!(out, "  {marker} ");
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
            let _ = write!(out, "{name:<24}");
            if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            let _ = write!(out, "{provider}");
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
            let _ = writeln!(out);
        }
    }
    let _ = writeln!(out);
}

/// Print model switch confirmation.
fn print_model_switched(data: &serde_json::Value) {
    let model = data["active"].as_str().unwrap_or("(none)");
    println!("Switched to model: {}", abbreviate_model(model));
}

/// Print model reset confirmation.
fn print_model_reset(data: &serde_json::Value) {
    let model = data["active"].as_str().unwrap_or("(none)");
    println!("Model reset to: {}", abbreviate_model(model));
}

/// Print the current / newly-applied reasoning_effort state.
///
/// Expected fields from the daemon:
/// - `override` — `null` (no override) or `{ "set": true, "value": <str|null> }`
/// - `effective` — string or null, the value that will reach the request
/// - `config_default` — string or null, the model's configured value
/// - `changed` — bool (only on writes)
fn print_reasoning_effort(data: &serde_json::Value) {
    let effective = match data.get("effective") {
        Some(v) if v.is_null() => "off".to_string(),
        Some(v) => v
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| v.to_string()),
        None => "(unknown)".to_string(),
    };
    let config_default = match data.get("config_default") {
        Some(v) if v.is_null() => "(unset)".to_string(),
        Some(v) => v
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| v.to_string()),
        None => "(unknown)".to_string(),
    };

    let override_info = data.get("override");
    let override_label = match override_info {
        Some(v) if v.is_null() => "(none, using config)".to_string(),
        Some(obj) if obj.is_object() => match obj.get("value") {
            Some(x) if x.is_null() => "forced off".to_string(),
            Some(x) => x
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| x.to_string()),
            None => "(set)".to_string(),
        },
        _ => "(unknown)".to_string(),
    };

    let changed = data
        .get("changed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if changed {
        println!("Reasoning effort updated.");
    }
    println!("  override:       {override_label}");
    println!("  effective:      {effective}");
    println!("  config default: {config_default}");
}

/// Print detailed model info.
fn print_model_info(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let name = data["name"].as_str().unwrap_or("?");
    write_section_header(&mut out, "Model", name, width);

    if let Some(qn) = data["qualified_name"].as_str() {
        write_row(&mut out, "Qualified", qn);
    }
    if let Some(mid) = data["model_id"].as_str() {
        write_row(&mut out, "Model ID", mid);
    }
    if let Some(sdk) = data["sdk"].as_str() {
        write_row(&mut out, "SDK", sdk);
    }
    if let Some(pk) = data["provider_key"].as_str() {
        write_row(&mut out, "Provider", pk);
    }
    if let Some(url) = data["base_url"].as_str() {
        write_row(&mut out, "Base URL", url);
    }
    if let Some(key) = data["api_key_env"].as_str() {
        write_row(&mut out, "API key env", &format!("${key}"));
    }

    // Cache settings
    if let Some(ttl) = data["cache_ttl_secs"].as_u64() {
        if ttl > 0 {
            write_row(&mut out, "Cache TTL", &format!("{ttl}s"));
        }
    }
    if let Some(depth) = data["cache_depth"].as_u64() {
        if depth > 0 {
            write_row(&mut out, "Cache depth", &depth.to_string());
        }
    }
    if let Some(re) = data["reasoning_effort"].as_str() {
        write_row(&mut out, "Reasoning", re);
    }
    if let Some(mt) = data["max_tokens"].as_u64() {
        write_row(&mut out, "Max tokens", &mt.to_string());
    }
    let _ = writeln!(out);
}

/// Print character info.
fn print_character_info(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let name = data["name"].as_str().unwrap_or("?");
    let char_color = character_color(name);

    write_section_header(&mut out, "Character", "", width);
    write_row_colored(&mut out, "Name", name, char_color);

    let active = data["active"].as_bool().unwrap_or(false);
    if active {
        write_row_colored(&mut out, "Active", "yes", Color::Green);
    }

    if let Some(dir) = data["config_dir"].as_str() {
        write_row(&mut out, "Config", dir);
    }

    let has_def = data["has_definition"].as_bool().unwrap_or(false);
    let has_user = data["has_user_definition"].as_bool().unwrap_or(false);
    write_row(&mut out, "Definition", if has_def { "yes" } else { "no" });
    if has_user {
        write_row(&mut out, "User def", "yes");
    }

    if data["has_config_override"].as_bool().unwrap_or(false) {
        write_row_colored(&mut out, "Config override", "yes", Color::Yellow);
    }

    if let Some(overrides) = data["prompt_overrides"].as_array() {
        if !overrides.is_empty() {
            let names: Vec<&str> = overrides.iter().filter_map(|v| v.as_str()).collect();
            write_row(&mut out, "Prompts", &names.join(", "));
        }
    }

    if let Some(dir) = data["data_dir"].as_str() {
        write_row(&mut out, "Data", dir);
    }

    // Definition preview
    if let Some(preview) = data["definition_preview"].as_str() {
        if !preview.is_empty() {
            let _ = writeln!(out);
            write_section_header(&mut out, "Preview", "", width);
            // Show first few lines, dimmed
            if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            for line in preview.lines().take(8) {
                let _ = writeln!(out, "  {line}");
            }
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
        }
    }
    let _ = writeln!(out);
}

/// Print memory status or query result.
fn print_memory(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    // If there's a "result" field, this is a query response.
    if let Some(result) = data["result"].as_str() {
        let _ = writeln!(out, "{result}");
        return;
    }

    // Otherwise it's a status response.
    let char_name = data["character"].as_str().unwrap_or("?");
    write_section_header(&mut out, "Memory", char_name, width);

    let files = data["entries"].as_u64().unwrap_or(0);
    let curated = data["curated_files"].as_u64().unwrap_or(0);
    let daily = data["daily_files"].as_u64().unwrap_or(0);
    let images = data["image_files"].as_u64().unwrap_or(0);

    write_row(&mut out, "Files", &files.to_string());
    if files > 0 {
        write_row(
            &mut out,
            "Breakdown",
            &format!("{curated} curated, {daily} daily, {images} images"),
        );
    }
    let _ = writeln!(out);
}

/// Print memory changelog.
fn print_changelog(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let char_name = data["character"].as_str().unwrap_or("?");
    write_section_header(&mut out, "Memory Changelog", char_name, width);

    if let Some(entries) = data["changelog"].as_array() {
        if entries.is_empty() {
            if use_color() {
                let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
            }
            let _ = writeln!(out, "  (no entries)");
            if use_color() {
                let _ = crossterm::execute!(out, ResetColor);
            }
        } else {
            for entry in entries {
                let ts = entry["timestamp"].as_str().unwrap_or("");
                let op = entry["operation"].as_str().unwrap_or("?");
                let desc = entry["description"].as_str().unwrap_or("");

                let time_display = parse_timestamp(ts)
                    .map(|dt| dt.format("%b %d %H:%M").to_string())
                    .unwrap_or_else(|| ts.to_string());

                if use_color() {
                    let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
                }
                let _ = write!(out, "  {time_display:<16}");

                let op_color = match op {
                    s if s.starts_with("create") || s.starts_with("compaction") => Color::Green,
                    s if s.starts_with("update") => Color::DarkYellow,
                    s if s.starts_with("supersede")
                        || s.starts_with("delete")
                        || s.starts_with("decay") =>
                    {
                        Color::Red
                    }
                    _ => Color::White,
                };
                if use_color() {
                    let _ = crossterm::execute!(out, SetForegroundColor(op_color));
                }
                let _ = write!(out, "{op:<18}");
                if use_color() {
                    let _ = crossterm::execute!(out, ResetColor);
                }
                let _ = writeln!(out, "{desc}");
            }
        }
    }
    let _ = writeln!(out);
}

/// Print compaction result.
fn print_compact_result(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let status = data["status"].as_str().unwrap_or("?");
    let suffix = if status == "dry_run" { "dry run" } else { "" };
    write_section_header(&mut out, "Compaction", suffix, width);

    let char_name = data["character"].as_str().unwrap_or("?");
    write_row(&mut out, "Character", char_name);

    if status == "dry_run" {
        let would = data["would_write_files"].as_u64().unwrap_or(0);
        write_row(&mut out, "Would write", &format!("{would} files"));
        let msgs = data["message_count"].as_u64().unwrap_or(0);
        let retained_turns = data["retained_turns"].as_u64().unwrap_or(0);
        write_row(
            &mut out,
            "Messages",
            &format!("{msgs} compacted, {retained_turns} turns retained"),
        );
    } else {
        let files = data["memory_files_written"]
            .as_array()
            .map_or(0, |files| files.len());
        write_row(&mut out, "Memory files", &format!("{files} written"));
        let msgs = data["message_count"].as_u64().unwrap_or(0);
        let retained_turns = data["retained_turns"].as_u64().unwrap_or(0);
        write_row(
            &mut out,
            "Messages",
            &format!("{msgs} compacted, {retained_turns} turns retained"),
        );
        if data["recap_generated"].as_bool().unwrap_or(false) {
            write_row(&mut out, "Recap", "generated");
        }
    }

    let _ = writeln!(out);
}

/// Print config display.
fn print_config(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    // Config set confirmation: { "set": "key", "value": ... }
    if let Some(key) = data["set"].as_str() {
        let value = &data["value"];
        let _ = writeln!(out, "Set {key} = {value}");
        return;
    }

    // Section view: { "key": "name", "config": { ... } }
    if let Some(key) = data["key"].as_str() {
        write_section_header(&mut out, "Config", key, width);
        print_config_section(&mut out, &data["config"], 1);
        let _ = writeln!(out);
        return;
    }

    // Full config: { "config": { ... } }
    if let Some(config) = data.get("config") {
        write_section_header(&mut out, "Config", "", width);
        print_config_section(&mut out, config, 1);
        let _ = writeln!(out);
    }
}

/// Recursively print config as indented key-value pairs.
fn print_config_section(out: &mut impl Write, value: &serde_json::Value, depth: usize) {
    let indent = "  ".repeat(depth);
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                match v {
                    serde_json::Value::Object(_) => {
                        if use_color() {
                            let _ = crossterm::execute!(out, SetForegroundColor(Color::White));
                        }
                        let _ = writeln!(out, "{indent}{k}:");
                        if use_color() {
                            let _ = crossterm::execute!(out, ResetColor);
                        }
                        print_config_section(out, v, depth + 1);
                    }
                    serde_json::Value::Null => {} // skip nulls
                    _ => {
                        if use_color() {
                            let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey));
                        }
                        let _ = write!(out, "{indent}{k:<24}");
                        if use_color() {
                            let _ = crossterm::execute!(out, ResetColor);
                        }
                        let display = match v {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Bool(b) => b.to_string(),
                            serde_json::Value::Number(n) => n.to_string(),
                            serde_json::Value::Array(arr) => {
                                let items: Vec<String> = arr
                                    .iter()
                                    .map(|i| {
                                        i.as_str()
                                            .map(String::from)
                                            .unwrap_or_else(|| i.to_string())
                                    })
                                    .collect();
                                items.join(", ")
                            }
                            _ => v.to_string(),
                        };
                        let _ = writeln!(out, "{display}");
                    }
                }
            }
        }
        _ => {
            let _ = writeln!(out, "{indent}{value}");
        }
    }
}

/// Print config check results.
fn print_config_check(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    let valid = data["valid"].as_bool().unwrap_or(false);
    let suffix = if valid { "valid" } else { "warnings" };
    write_section_header(&mut out, "Config Check", suffix, width);

    if let Some(dir) = data["config_dir"].as_str() {
        write_row(&mut out, "Config dir", dir);
    }
    if let Some(dir) = data["data_dir"].as_str() {
        write_row(&mut out, "Data dir", dir);
    }

    let chat = data["chat_models"].as_u64().unwrap_or(0);
    let tool = data["tool_models"].as_u64().unwrap_or(0);
    let embed = data["embedding_models"].as_u64().unwrap_or(0);
    write_row(
        &mut out,
        "Models",
        &format!("{chat} chat, {tool} tool, {embed} embedding"),
    );

    let _ = writeln!(out);

    // Warnings
    if let Some(warnings) = data["warnings"].as_array() {
        for w in warnings {
            if let Some(msg) = w.as_str() {
                if use_color() {
                    let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkYellow));
                }
                let _ = write!(out, "  ! ");
                if use_color() {
                    let _ = crossterm::execute!(out, ResetColor);
                }
                let _ = writeln!(out, "{msg}");
            }
        }
    }

    // Info
    if let Some(info) = data["info"].as_array() {
        for i in info {
            if let Some(msg) = i.as_str() {
                if use_color() {
                    let _ = crossterm::execute!(out, SetForegroundColor(Color::Green));
                }
                let _ = write!(out, "  ");
                if use_color() {
                    let _ = crossterm::execute!(out, ResetColor);
                }
                let _ = writeln!(out, "{msg}");
            }
        }
    }
    let _ = writeln!(out);
}

/// Print config reset confirmation.
fn print_config_reset(data: &serde_json::Value) {
    let msg = data["message"]
        .as_str()
        .unwrap_or("Configuration reloaded from disk");
    println!("{msg}");
}

/// Print diagnostics from ring buffers.
pub fn print_diagnostics(data: &serde_json::Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let width = term_width();

    // -- API Calls --
    print_diagnostics_section(
        &mut out,
        "API Calls",
        &data["api_calls"],
        width,
        |out, call| {
            let model = abbreviate_model(call["model"].as_str().unwrap_or("?"));
            let input = call["input_tokens"].as_u64().unwrap_or(0);
            let output_t = call["output_tokens"].as_u64().unwrap_or(0);
            let cr = call["cache_read_tokens"].as_u64().unwrap_or(0);
            let cw = call["cache_write_tokens"].as_u64().unwrap_or(0);
            let total = call["total_ms"].as_u64().unwrap_or(0);
            let secs = total as f64 / 1000.0;

            let _ = write!(out, "{model:<24}");
            write_dim(
                out,
                &format!("in:{input:<5} out:{output_t:<5} cache:{cr}/{cw}  {secs:.1}s"),
            );

            if let Some(err) = call.get("error").filter(|v| !v.is_null()) {
                write_fg(
                    out,
                    Color::Red,
                    &format!("  ERR: {}", err.as_str().unwrap_or("?")),
                );
            }
            let _ = writeln!(out);
        },
    );

    // -- Tool Calls --
    print_diagnostics_section(
        &mut out,
        "Tool Calls",
        &data["tool_calls"],
        width,
        |out, call| {
            let name = call["tool_name"].as_str().unwrap_or("?");
            let dur = call["duration_ms"].as_u64().unwrap_or(0);
            let ok = call["success"].as_bool().unwrap_or(true);

            let _ = write!(out, "{name:<24}");
            write_dim(out, &format!("{dur}ms  "));
            let (marker_color, marker_text) = if ok {
                (Color::Green, "ok")
            } else {
                (Color::Red, "FAIL")
            };
            write_fg(out, marker_color, marker_text);
            let _ = writeln!(out);
        },
    );

    // -- Errors --
    print_diagnostics_section(&mut out, "Errors", &data["errors"], width, |out, err| {
        let etype = err["error_type"].as_str().unwrap_or("?");
        let msg = err["message"].as_str().unwrap_or("?");

        write_fg(out, Color::Red, &format!("{etype:<12}"));
        let _ = writeln!(out, "{msg}");
    });
}

/// Print a diagnostics section with a header, shared timestamp formatting,
/// and a per-entry formatter.
fn print_diagnostics_section<W: Write>(
    out: &mut W,
    title: &str,
    section: &serde_json::Value,
    width: usize,
    mut format_row: impl FnMut(&mut W, &serde_json::Value),
) {
    let count = section["count"].as_u64().unwrap_or(0);
    write_section_header(out, title, &format!("{count} total"), width);

    if let Some(entries) = section["recent"].as_array() {
        if entries.is_empty() {
            print_dim_line(out, "(none)");
        } else {
            for entry in entries {
                let ts = entry["timestamp"].as_str().unwrap_or("");
                let time = parse_timestamp(ts)
                    .map(|dt| dt.format("%H:%M:%S").to_string())
                    .unwrap_or_else(|| ts.chars().take(8).collect());

                write_dim(out, &format!("  {time}  "));
                format_row(out, entry);
            }
        }
    }
    let _ = writeln!(out);
}

fn format_k(tokens: u64) -> String {
    if tokens == 0 {
        "\u{2014}".into()
    } else if tokens < 1000 {
        tokens.to_string()
    } else {
        format!("{:.1}K", tokens as f64 / 1000.0)
    }
}

pub fn print_usage(data: &serde_json::Value) {
    let mode = data["mode"].as_str().unwrap_or("summary");

    match mode {
        "tsv" | "csv" => {
            if let Some(d) = data["data"].as_str() {
                print!("{d}");
            }
        }
        "summary_by_call_type" => {
            let period = data["period"].as_str().unwrap_or("today");
            let today = chrono::Utc::now().format("%Y-%m-%d");
            println!("Shore Usage by Call Type \u{2014} {today} (period: {period})\n");
            println!(
                "{:<18} {:>5}  {:>9}  {:>9}  {:>9}  {:>9}  {:>8}",
                "Call Type", "Calls", "Input", "Output", "Cache R", "Cache W", "Cost"
            );
            println!("{}", "-".repeat(78));
            let summary = data["summary"].as_array();
            let mut grand_total = 0.0f64;
            if let Some(rows) = summary {
                for s in rows {
                    let cost_str = s["total_cost"]
                        .as_f64()
                        .map(|c| {
                            grand_total += c;
                            format!("${c:.2}")
                        })
                        .unwrap_or_else(|| "\u{2014}".into());
                    println!(
                        "{:<18} {:>5}  {:>9}  {:>9}  {:>9}  {:>9}  {:>8}",
                        s["call_type"].as_str().unwrap_or(""),
                        s["call_count"].as_u64().unwrap_or(0),
                        format_k(s["total_input"].as_u64().unwrap_or(0)),
                        format_k(s["total_output"].as_u64().unwrap_or(0)),
                        format_k(s["total_cache_read"].as_u64().unwrap_or(0)),
                        format_k(s["total_cache_write"].as_u64().unwrap_or(0)),
                        cost_str,
                    );
                }
                if !rows.is_empty() {
                    println!("{:>70} ${grand_total:.2}", "Total:");
                } else {
                    println!("  No usage data for this period.");
                }
            }
        }
        "anomalies" => {
            let anomalies = data["anomalies"].as_array();
            if anomalies.is_none() || anomalies.unwrap().is_empty() {
                println!("No cache anomalies found.");
            } else if let Some(rows) = anomalies {
                println!("Cache Anomalies:\n");
                for r in rows {
                    println!(
                        "  {} {} {} {} \u{2014} {} (read: {}, write: {})",
                        r["ts"].as_str().unwrap_or("?"),
                        r["character"].as_str().unwrap_or("?"),
                        r["model"].as_str().unwrap_or("?"),
                        r["call_type"].as_str().unwrap_or("?"),
                        r["anomaly"].as_str().unwrap_or("?"),
                        r["cache_read_tokens"].as_u64().unwrap_or(0),
                        r["cache_write_tokens"].as_u64().unwrap_or(0),
                    );
                }
                println!("\nTotal: {} anomalies", rows.len());
            }
        }
        "refresh_pricing" => {
            println!("Pricing cache cleared. Prices will be re-fetched on next daemon use.");
        }
        "recalculate" => {
            let updated = data["updated"].as_u64().unwrap_or(0);
            let total = data["total"].as_u64().unwrap_or(0);
            if total == 0 {
                println!("All rows already have costs calculated.");
            } else {
                println!(
                    "Updated {updated}/{total} rows. {} still missing pricing data.",
                    total - updated
                );
                if let Some(failures) = data["failures"].as_array() {
                    if !failures.is_empty() {
                        println!("\nFailed models:");
                        for f in failures {
                            println!(
                                "  {} — {}",
                                f["model"].as_str().unwrap_or("?"),
                                f["reason"].as_str().unwrap_or("unknown")
                            );
                        }
                    }
                }
            }
        }
        _ => {
            let period = data["period"].as_str().unwrap_or("today");
            let today = chrono::Utc::now().format("%Y-%m-%d");
            println!("Shore Usage \u{2014} {today} (period: {period})\n");
            println!(
                "{:<12} {:<24} {:>5}  {:>9}  {:>9}  {:>9}  {:>9}  {:>8}",
                "Provider", "Model", "Calls", "Input", "Output", "Cache R", "Cache W", "Cost"
            );
            println!("{}", "-".repeat(90));

            let summary = data["summary"].as_array();
            let mut grand_total = 0.0f64;
            if let Some(rows) = summary {
                for s in rows {
                    let cost_str = s["total_cost"]
                        .as_f64()
                        .map(|c| {
                            grand_total += c;
                            format!("${c:.2}")
                        })
                        .unwrap_or_else(|| "\u{2014}".into());
                    println!(
                        "{:<12} {:<24} {:>5}  {:>9}  {:>9}  {:>9}  {:>9}  {:>8}",
                        s["provider"].as_str().unwrap_or(""),
                        s["model"].as_str().unwrap_or(""),
                        s["call_count"].as_u64().unwrap_or(0),
                        format_k(s["total_input"].as_u64().unwrap_or(0)),
                        format_k(s["total_output"].as_u64().unwrap_or(0)),
                        format_k(s["total_cache_read"].as_u64().unwrap_or(0)),
                        format_k(s["total_cache_write"].as_u64().unwrap_or(0)),
                        cost_str,
                    );
                }
                if !rows.is_empty() {
                    println!("{:>82} ${grand_total:.2}", "Total:");
                } else {
                    println!("  No usage data for this period.");
                }
            }

            if let Some(health) = data["cache_health"].as_array() {
                if !health.is_empty() {
                    println!("\nCache Health (anthropic):");
                    for entry in health {
                        let char_name = entry["character"].as_str().unwrap_or("?");
                        let state = entry["state"].as_str().unwrap_or("cold");
                        let streak = entry["streak"].as_u64().unwrap_or(0);
                        let state_str = if state == "warm" {
                            format!("Warm (streak: {streak} calls)")
                        } else {
                            "Cold".into()
                        };
                        println!("  {char_name:<8} \u{2014} {state_str}");
                    }
                }
            }

            let anomaly_count = data["anomaly_count_7d"].as_u64().unwrap_or(0);
            println!("\nAnomalies (last 7d): {anomaly_count}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::set_color_enabled;

    #[test]
    fn heartbeat_description_maps_states() {
        assert_eq!(
            heartbeat_description("Active", 0, 3),
            "active \u{2014} in conversation"
        );
        assert_eq!(
            heartbeat_description("Active", 2, 3),
            "active \u{2014} idle 2/3 ticks"
        );
        assert_eq!(
            heartbeat_description("Dormant", 4, 3),
            "dormant \u{2014} waiting for you"
        );
    }

    #[test]
    fn print_status_does_not_panic() {
        set_color_enabled(false);
        let data = serde_json::json!({
            "character": "Sable",
            "message_count": 142,
            "active_model": "claude-sonnet-4-20250514",
            "pending_deferred_edit_count": 2,
            "pending_deferred_edits": ["SOUL.md", "TOOLS.md"],
            "tokens": {
                "input": 12450,
                "output": 3218,
                "cache_read": 8100,
                "cache_write": 1024,
            },
            "autonomy": {
                "paused": false,
                "heartbeat_state": "Active",
                "ticks_without_user": 1,
                "dormant_after_heartbeat_turns": 3,
                "effective_interval_secs": 3540,
            }
        });
        print_status(&data, "Sable");
    }

    #[test]
    fn print_status_minimal_does_not_panic() {
        set_color_enabled(false);
        let data = serde_json::json!({
            "character": "Sable",
            "message_count": 5,
            "active_model": null,
        });
        print_status(&data, "Sable");
    }

    #[test]
    fn print_status_paused_does_not_panic() {
        set_color_enabled(false);
        let data = serde_json::json!({
            "character": "Sable",
            "message_count": 50,
            "active_model": "test-model",
            "autonomy": {
                "paused": true,
                "heartbeat_state": "Active",
                "ticks_without_user": 0,
                "dormant_after_heartbeat_turns": 3,
                "effective_interval_secs": 3600,
            }
        });
        print_status(&data, "Sable");
    }

    #[test]
    fn print_status_with_activity_does_not_panic() {
        set_color_enabled(false);
        // Simulate a realistic hour histogram: busier in afternoon/evening.
        let histogram: Vec<f64> = (0..24)
            .map(|h| match h {
                0..=5 => 0.01,
                6..=8 => 0.04,
                9..=11 => 0.06,
                12..=14 => 0.08,
                15..=17 => 0.05,
                18..=21 => 0.10,
                _ => 0.02,
            })
            .collect();
        let classifications: Vec<&str> = (0..24)
            .map(|h| match h {
                0..=5 => "trough",
                18..=21 => "peak",
                _ => "normal",
            })
            .collect();

        let data = serde_json::json!({
            "character": "Sable",
            "message_count": 200,
            "active_model": "claude-sonnet-4-20250514",
            "tokens": { "input": 5000, "output": 1200, "cache_read": 0, "cache_write": 0 },
            "activity": {
                "hour_histogram": histogram,
                "hour_classifications": classifications,
                "has_sufficient_heatmap": true,
                "engagement_score": 0.72,
                "sessions_per_day": 2.3,
                "message_count": 200,
            }
        });
        print_status(&data, "Sable");
    }

    #[test]
    fn print_status_sparse_activity_does_not_panic() {
        set_color_enabled(false);
        let data = serde_json::json!({
            "character": "Sable",
            "message_count": 3,
            "active_model": "test-model",
            "activity": {
                "hour_histogram": vec![0.0_f64; 24],
                "hour_classifications": vec!["normal"; 24],
                "has_sufficient_heatmap": false,
                "engagement_score": 0.0,
                "sessions_per_day": 0.0,
                "message_count": 3,
            }
        });
        print_status(&data, "Sable");
    }

    // ── classification_color ─────────────────────────────────────────

    #[test]
    fn classification_color_maps_correctly() {
        assert!(matches!(classification_color("peak"), Color::Cyan));
        assert!(matches!(classification_color("trough"), Color::DarkGrey));
        assert!(matches!(classification_color("normal"), Color::White));
        assert!(matches!(classification_color("unknown"), Color::White));
    }

    // ── heartbeat_description edge cases ──────────────────────────

    #[test]
    fn heartbeat_description_unknown_state() {
        // Unknown states pass through as-is.
        assert_eq!(heartbeat_description("CustomState", 0, 8), "CustomState");
    }

    // ── format_command dispatch ─────────────────────────────────────

    #[test]
    fn format_command_dispatches_known_commands() {
        set_color_enabled(false);
        // These should all run without panic and hit their formatters.
        format_command("config_reset", &serde_json::json!({"message": "reloaded"}));
        format_command("inject_system", &serde_json::json!({}));
        format_command("edit", &serde_json::json!({"ref": "m42"}));
    }

    #[test]
    fn format_command_fallback_for_unknown() {
        set_color_enabled(false);
        // Unknown commands should use fallback (JSON pretty print), not panic.
        format_command("totally_unknown", &serde_json::json!({"key": "val"}));
    }

    #[test]
    fn print_delete_confirmation_single_and_multiple() {
        set_color_enabled(false);
        // Single deletion.
        print_delete_confirmation(&serde_json::json!({"deleted": ["msg_1"]}));
        // Multiple deletions.
        print_delete_confirmation(&serde_json::json!({"deleted": ["msg_1", "msg_2", "msg_3"]}));
        // String form.
        print_delete_confirmation(&serde_json::json!({"deleted": "msg_42"}));
    }

    #[test]
    fn print_model_switched_shows_abbreviated_name() {
        set_color_enabled(false);
        // Should not panic and should abbreviate the date suffix.
        print_model_switched(&serde_json::json!({"active": "claude-sonnet-4-20250514"}));
    }

    #[test]
    fn format_k_correctness() {
        // Regression for SHA 31f20cb: verify correct formatting across boundary.
        // 0 maps to em-dash (—), not a number.
        assert_eq!(format_k(0), "\u{2014}");
        // Numbers < 1000 display without K suffix.
        assert_eq!(format_k(1), "1");
        assert_eq!(format_k(500), "500");
        assert_eq!(format_k(999), "999");
        // 1000 is the first value that rounds to K.
        assert_eq!(format_k(1000), "1.0K");
        // 1500 → "1.5K"
        assert_eq!(format_k(1500), "1.5K");
        // 10000 → "10.0K"
        assert_eq!(format_k(10000), "10.0K");
    }

    #[test]
    fn density_to_block_ranges() {
        assert_eq!(density_to_block(0.0), '\u{2591}'); // below threshold
        assert_eq!(density_to_block(0.04), '\u{2591}'); // below threshold
        assert_eq!(density_to_block(0.06), '\u{2581}'); // 0.06 * 7 = 0.42 -> round 0 -> first block
        assert_eq!(density_to_block(0.5), '\u{2585}'); // 0.5 * 7 = 3.5 -> round 4 -> fifth block
        assert_eq!(density_to_block(1.0), '\u{2588}'); // 1.0 * 7 = 7.0 -> index 7 -> full block
    }
}
