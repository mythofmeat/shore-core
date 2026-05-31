#![allow(
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unwrap_used
)]

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use serde::de::DeserializeOwned;
use serde::Serialize;
use shore_config::app::{
    AdvancedConfig, AppConfig, AutonomyConfig, BackgroundDefaultsConfig, BehaviorConfig,
    BudgetWeekday, CommandNotifyConfig, CompactionConfig, ConnectionsConfig, DaemonConfig,
    DefaultsConfig, DreamingConfig, EmbeddedConfig, HeartbeatConfig, MatrixConfig, MemoryConfig,
    NotificationBackend, NotificationEventsConfig, NotificationsConfig, NtfyConfig,
    RetrievalBinaryMode, RetrievalConfig, RetrievalMode, SearchConfig, ServiceEntry,
    ServicesConfig, ThinkingConfig, ToolToggles, ToolUseConfig, UsageBudgetAction,
    UsageBudgetConfig, UsageBudgetPeriod, UsageConfig, UsageSpikeWarningsConfig,
};
use shore_config::models::{ModelConfigFields, Sdk};
use shore_config::providers::{ProviderDiscovery, ProviderEntry, ProviderKeyEntry};
use shore_config::ConfigDuration;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct DurationHolder {
    duration: ConfigDuration,
}

fn arb_text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![(b'a'..=b'z'), (b'A'..=b'Z'), (b'0'..=b'9'), Just(b'_')],
        0..24,
    )
    .prop_map(|bytes| String::from_utf8(bytes).unwrap())
}

fn arb_nonempty_text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![(b'a'..=b'z'), (b'A'..=b'Z'), (b'0'..=b'9'), Just(b'_')],
        1..24,
    )
    .prop_map(|bytes| String::from_utf8(bytes).unwrap())
}

fn arb_duration() -> impl Strategy<Value = ConfigDuration> {
    (0u64..(14 * 24 * 60 * 60 * 1000)).prop_map(ConfigDuration::from_millis)
}

fn arb_cost() -> impl Strategy<Value = f64> {
    (0u32..100_000).prop_map(|cents| f64::from(cents) / 100.0)
}

fn arb_fraction() -> impl Strategy<Value = f64> {
    (0u32..200).prop_map(|hundredths| f64::from(hundredths) / 100.0)
}

fn arb_decimal(max_tenths: u32) -> impl Strategy<Value = f64> {
    (0u32..max_tenths).prop_map(|tenths| f64::from(tenths) / 10.0)
}

fn arb_sdk() -> impl Strategy<Value = Sdk> {
    prop_oneof![
        Just(Sdk::Anthropic),
        Just(Sdk::Openai),
        Just(Sdk::Gemini),
        Just(Sdk::Zai),
    ]
}

fn arb_toml_value() -> impl Strategy<Value = toml::Value> {
    prop_oneof![
        any::<bool>().prop_map(toml::Value::Boolean),
        arb_text().prop_map(toml::Value::String),
        prop::collection::vec(arb_text().prop_map(toml::Value::String), 0..4)
            .prop_map(toml::Value::Array),
    ]
}

fn arb_model_config_fields() -> impl Strategy<Value = ModelConfigFields> {
    let transport = (
        prop::option::of(arb_sdk()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(1u32..1_000_000),
        prop::option::of(1u32..200_000),
        prop::option::of(arb_decimal(20)),
        prop::option::of(arb_decimal(10)),
    );
    let reasoning = (
        prop::option::of(arb_nonempty_text()),
        prop::option::of(1u32..200_000),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(any::<bool>()),
        prop::option::of(arb_duration()),
        prop::option::of(0u32..100),
        prop::option::of(arb_toml_value()),
    );
    let provider_specific = (
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(0u32..3),
        prop::option::of(any::<bool>()),
        prop::option::of(any::<bool>()),
        prop::option::of(any::<bool>()),
    );

    (transport, reasoning, provider_specific).prop_map(
        |(
            (sdk, api_key_env, base_url, max_context_tokens, max_output_tokens, temperature, top_p),
            (
                reasoning_effort,
                budget_tokens,
                cache_ttl,
                keepalive_enabled,
                keepalive_ttl,
                keepalive_max_pings,
                openrouter_provider,
            ),
            (
                vertex_project,
                vertex_location,
                gemini_generation,
                gemini_web_search,
                zai_clear_thinking,
                zai_subscription,
            ),
        )| ModelConfigFields {
            sdk,
            api_key_env,
            base_url,
            max_context_tokens,
            max_output_tokens,
            temperature,
            top_p,
            reasoning_effort,
            budget_tokens,
            cache_ttl,
            keepalive_enabled,
            keepalive_ttl,
            keepalive_max_pings,
            openrouter_provider,
            vertex_project,
            vertex_location,
            gemini_generation,
            gemini_web_search,
            zai_clear_thinking,
            zai_subscription,
        },
    )
}

fn arb_provider_key_entry() -> impl Strategy<Value = ProviderKeyEntry> {
    (
        arb_nonempty_text(),
        arb_nonempty_text(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(name, env, enabled, warn_on_fallback)| ProviderKeyEntry {
            name,
            env,
            enabled,
            warn_on_fallback,
        })
}

fn arb_provider_discovery() -> impl Strategy<Value = ProviderDiscovery> {
    (
        any::<bool>(),
        prop::collection::vec(arb_nonempty_text(), 0..4),
    )
        .prop_map(|(enabled, ignore)| ProviderDiscovery { enabled, ignore })
}

fn arb_provider_entry() -> impl Strategy<Value = ProviderEntry> {
    let key_mode = prop_oneof![
        Just((None, Vec::new())),
        arb_nonempty_text().prop_map(|env| (Some(env), Vec::new())),
        prop::collection::vec(arb_provider_key_entry(), 0..3).prop_map(|keys| (None, keys)),
    ];

    (
        any::<bool>(),
        prop::option::of(arb_sdk()),
        prop::option::of(arb_nonempty_text()),
        key_mode,
        arb_provider_discovery(),
    )
        .prop_map(
            |(enabled, sdk, base_url, (api_key_env, keys), discovery)| ProviderEntry {
                enabled,
                sdk,
                base_url,
                api_key_env,
                keys,
                discovery,
            },
        )
}

fn arb_daemon_config() -> impl Strategy<Value = DaemonConfig> {
    (
        arb_nonempty_text(),
        any::<bool>(),
        prop::collection::vec(arb_nonempty_text(), 0..3),
    )
        .prop_map(
            |(addr, unsafe_allow_remote_access, allowed_hosts)| DaemonConfig {
                addr,
                unsafe_allow_remote_access,
                allowed_hosts,
            },
        )
}

fn arb_defaults_config() -> impl Strategy<Value = DefaultsConfig> {
    (
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        any::<bool>(),
    )
        .prop_map(
            |(
                model,
                background_model,
                background_heartbeat,
                background_compaction,
                background_dreaming,
                embedding,
                image_generation,
                display_name,
                stream,
            )| DefaultsConfig {
                model,
                background: BackgroundDefaultsConfig {
                    model: background_model,
                    heartbeat: background_heartbeat,
                    compaction: background_compaction,
                    dreaming: background_dreaming,
                },
                heartbeat: None,
                dreaming: None,
                embedding,
                image_generation,
                display_name,
                stream,
            },
        )
}

fn arb_heartbeat_config() -> impl Strategy<Value = HeartbeatConfig> {
    (
        any::<bool>(),
        arb_duration(),
        0u32..10,
        arb_duration(),
        arb_duration(),
        0u32..40,
        0u32..20,
    )
        .prop_map(
            |(
                enabled,
                fallback_heartbeat_interval,
                dormant_after_heartbeat_turns,
                dormant_after_idle_time,
                minimum_heartbeat_latency,
                max_tool_rounds,
                wrap_up_grace_rounds,
            )| HeartbeatConfig {
                enabled,
                fallback_heartbeat_interval,
                dormant_after_heartbeat_turns,
                dormant_after_idle_time,
                minimum_heartbeat_latency,
                max_tool_rounds,
                wrap_up_grace_rounds,
            },
        )
}

fn arb_tool_toggles() -> impl Strategy<Value = ToolToggles> {
    prop::collection::vec((arb_nonempty_text(), any::<bool>()), 0..5).prop_map(|entries| {
        let mut toggles = ToolToggles::default();
        for (tool, enabled) in entries {
            toggles.set(&tool, enabled);
        }
        toggles
    })
}

fn arb_tool_use_config() -> impl Strategy<Value = ToolUseConfig> {
    (
        any::<bool>(),
        0u32..40,
        0usize..100_000,
        arb_tool_toggles(),
        (
            arb_nonempty_text(),
            0u32..25,
            arb_nonempty_text(),
            any::<bool>(),
        ),
    )
        .prop_map(
            |(enabled, max_iterations, max_result_chars, tools, search)| {
                let (api_key_env, max_results, search_depth, include_answer) = search;
                ToolUseConfig {
                    enabled,
                    max_iterations,
                    max_result_chars,
                    tools,
                    search: SearchConfig {
                        api_key_env,
                        max_results,
                        search_depth,
                        include_answer,
                    },
                }
            },
        )
}

fn arb_behavior_config() -> impl Strategy<Value = BehaviorConfig> {
    (any::<bool>(), arb_heartbeat_config(), arb_tool_use_config()).prop_map(
        |(enabled, heartbeat, tool_use)| BehaviorConfig {
            autonomy: AutonomyConfig { enabled, heartbeat },
            tool_use,
        },
    )
}

fn arb_compaction_config() -> impl Strategy<Value = CompactionConfig> {
    (
        any::<bool>(),
        arb_duration(),
        0usize..100,
        0usize..200,
        0usize..500_000,
        0usize..20,
        0u32..50,
    )
        .prop_map(
            |(
                enabled,
                idle_trigger,
                min_turns,
                max_turns,
                max_context_tokens,
                keep_recent_turns,
                max_tool_rounds,
            )| CompactionConfig {
                enabled,
                idle_trigger,
                min_turns,
                max_turns,
                max_context_tokens,
                keep_recent_turns,
                max_tool_rounds,
            },
        )
}

fn arb_dreaming_config() -> impl Strategy<Value = DreamingConfig> {
    (
        any::<bool>(),
        Just("0 3 * * *".to_string()),
        0u32..50,
        arb_duration(),
        arb_duration(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(
                enabled,
                frequency,
                max_tool_rounds,
                minimum_inactive_time,
                max_lateness,
                compact_before,
                compact_to_zero,
            )| DreamingConfig {
                enabled,
                frequency,
                max_tool_rounds,
                minimum_inactive_time,
                max_lateness,
                compact_before,
                compact_to_zero,
            },
        )
}

fn arb_retrieval_mode() -> impl Strategy<Value = RetrievalMode> {
    prop_oneof![
        Just(RetrievalMode::Auto),
        Just(RetrievalMode::Lexical),
        Just(RetrievalMode::Hybrid),
    ]
}

fn arb_retrieval_binary_mode() -> impl Strategy<Value = RetrievalBinaryMode> {
    prop_oneof![
        Just(RetrievalBinaryMode::Skip),
        Just(RetrievalBinaryMode::Metadata),
        Just(RetrievalBinaryMode::TryEmbed),
    ]
}

fn arb_memory_config() -> impl Strategy<Value = MemoryConfig> {
    (
        arb_compaction_config(),
        arb_dreaming_config(),
        any::<bool>(),
        arb_retrieval_mode(),
        0u64..10_000_000,
        0usize..100_000,
        0u64..1_000_000_000,
        0usize..100_000,
        arb_retrieval_binary_mode(),
    )
        .prop_map(
            |(
                compaction,
                dreaming,
                preserve_prior_turns,
                mode,
                max_file_bytes,
                max_indexed_files,
                max_total_indexed_bytes,
                max_embed_chars_per_file,
                binary,
            )| MemoryConfig {
                compaction,
                dreaming,
                thinking: ThinkingConfig {
                    preserve_prior_turns,
                },
                retrieval: RetrievalConfig {
                    mode,
                    max_file_bytes,
                    max_indexed_files,
                    max_total_indexed_bytes,
                    max_embed_chars_per_file,
                    binary,
                },
            },
        )
}

fn arb_matrix_config() -> impl Strategy<Value = MatrixConfig> {
    let embedded = (
        arb_nonempty_text(),
        arb_nonempty_text(),
        1u16..9000,
        arb_nonempty_text(),
        arb_nonempty_text(),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
    )
        .prop_map(
            |(server_name, bind_address, port, admin_user, admin_password, data_dir, binary)| {
                EmbeddedConfig {
                    server_name,
                    bind_address,
                    port,
                    admin_user,
                    admin_password,
                    data_dir,
                    binary,
                }
            },
        );

    (
        any::<bool>(),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(embedded),
    )
        .prop_map(
            |(enabled, homeserver, user_id, room_id, trusted_user, embedded)| MatrixConfig {
                enabled,
                homeserver,
                user_id,
                room_id,
                trusted_user,
                embedded,
            },
        )
}

fn arb_connections_config() -> impl Strategy<Value = ConnectionsConfig> {
    prop::option::of(arb_matrix_config()).prop_map(|matrix| ConnectionsConfig {
        matrix,
        telegram: None,
        discord: None,
    })
}

fn arb_notification_backend() -> impl Strategy<Value = NotificationBackend> {
    prop_oneof![
        Just(NotificationBackend::NotifySend),
        Just(NotificationBackend::Ntfy),
        Just(NotificationBackend::Command),
    ]
}

fn arb_notifications_config() -> impl Strategy<Value = NotificationsConfig> {
    let base = (
        any::<bool>(),
        arb_notification_backend(),
        arb_nonempty_text(),
        arb_text(),
        arb_text(),
        arb_text(),
        arb_duration(),
    );
    let events = (
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    );

    (base, events).prop_map(
        |(
            (
                enabled,
                backend,
                ntfy_url,
                ntfy_topic,
                ntfy_token,
                command_template,
                generation_threshold,
            ),
            (
                autonomous_message,
                cache_warning,
                compaction_complete,
                error,
                message_complete,
                usage_warning,
            ),
        )| NotificationsConfig {
            enabled,
            backend,
            ntfy: NtfyConfig {
                url: ntfy_url,
                topic: ntfy_topic,
                token: ntfy_token,
            },
            command: CommandNotifyConfig {
                template: command_template,
            },
            generation_threshold,
            events: NotificationEventsConfig {
                autonomous_message,
                cache_warning,
                compaction_complete,
                error,
                message_complete,
                usage_warning,
            },
        },
    )
}

fn arb_usage_budget_period() -> impl Strategy<Value = UsageBudgetPeriod> {
    prop_oneof![
        Just(UsageBudgetPeriod::Hour),
        Just(UsageBudgetPeriod::Day),
        Just(UsageBudgetPeriod::Week),
        Just(UsageBudgetPeriod::Month),
    ]
}

fn arb_usage_budget_action() -> impl Strategy<Value = UsageBudgetAction> {
    prop_oneof![
        Just(UsageBudgetAction::Warn),
        Just(UsageBudgetAction::Block),
        Just(UsageBudgetAction::PauseBackground),
    ]
}

fn arb_budget_weekday() -> impl Strategy<Value = BudgetWeekday> {
    prop_oneof![
        Just(BudgetWeekday::Monday),
        Just(BudgetWeekday::Tuesday),
        Just(BudgetWeekday::Wednesday),
        Just(BudgetWeekday::Thursday),
        Just(BudgetWeekday::Friday),
        Just(BudgetWeekday::Saturday),
        Just(BudgetWeekday::Sunday),
    ]
}

fn arb_usage_budget_config() -> impl Strategy<Value = UsageBudgetConfig> {
    let base = (
        arb_text(),
        arb_usage_budget_period(),
        arb_cost(),
        prop::collection::vec(arb_fraction(), 0..4),
        arb_usage_budget_action(),
    );
    let filters = (
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(arb_nonempty_text()),
        prop::collection::vec(arb_nonempty_text(), 0..3),
    );
    let resets = (
        prop::option::of(any::<bool>()),
        prop::option::of(0u32..24),
        prop::option::of(arb_budget_weekday()),
        prop::option::of(1u32..32),
    );

    (base, filters, resets).prop_map(
        |(
            (name, period, cost_usd, warn_at, limit),
            (character, provider, api_key, model, call_type, usage_kind),
            (allow_compaction_over_budget, reset_hour, reset_day_of_week, reset_day_of_month),
        )| UsageBudgetConfig {
            name,
            period,
            cost_usd,
            warn_at,
            limit,
            character,
            provider,
            api_key,
            model,
            call_type,
            usage_kind,
            allow_compaction_over_budget,
            reset_hour,
            reset_day_of_week,
            reset_day_of_month,
        },
    )
}

fn arb_usage_config() -> impl Strategy<Value = UsageConfig> {
    (
        prop_oneof![Just("local".to_string()), Just("utc".to_string())],
        any::<bool>(),
        prop::collection::vec(arb_usage_budget_config(), 0..2),
        any::<bool>(),
        arb_usage_budget_period(),
        (10u32..100).prop_map(|tenths| f64::from(tenths) / 10.0),
        arb_cost(),
    )
        .prop_map(
            |(
                timezone,
                allow_compaction_over_budget,
                budgets,
                spike_enabled,
                spike_period,
                multiplier,
                min_cost_usd,
            )| UsageConfig {
                timezone,
                allow_compaction_over_budget,
                budgets,
                spike_warnings: UsageSpikeWarningsConfig {
                    enabled: spike_enabled,
                    period: spike_period,
                    multiplier,
                    min_cost_usd,
                },
            },
        )
}

fn arb_advanced_config() -> impl Strategy<Value = AdvancedConfig> {
    (
        any::<bool>(),
        any::<bool>(),
        prop::option::of(arb_nonempty_text()),
        prop::option::of(0u32..10),
        prop::option::of(arb_duration()),
        0u64..20_000_000,
    )
        .prop_map(
            |(
                api_payload_logging,
                cache_forensics,
                editor,
                max_retries,
                retry_backoff,
                max_image_size,
            )| AdvancedConfig {
                api_payload_logging,
                cache_forensics,
                editor,
                max_retries,
                retry_backoff,
                max_image_size,
            },
        )
}

fn arb_app_config() -> impl Strategy<Value = AppConfig> {
    (
        arb_daemon_config(),
        arb_defaults_config(),
        arb_behavior_config(),
        arb_memory_config(),
        arb_connections_config(),
        prop::option::of(arb_nonempty_text()),
        arb_notifications_config(),
        arb_usage_config(),
        arb_advanced_config(),
    )
        .prop_map(
            |(
                daemon,
                defaults,
                behavior,
                memory,
                connections,
                service_command,
                notifications,
                usage,
                advanced,
            )| AppConfig {
                daemon,
                defaults,
                behavior,
                memory,
                connections,
                services: ServicesConfig {
                    llm: ServiceEntry {
                        command: service_command,
                        socket: None,
                    },
                },
                notifications,
                usage,
                advanced,
            },
        )
}

fn assert_toml_round_trip<T>(value: &T) -> Result<(), TestCaseError>
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = toml::to_string(value).expect("config value serializes to TOML");
    let decoded: T = toml::from_str(&encoded).expect("serialized TOML parses");
    prop_assert_eq!(&decoded, value);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn config_duration_round_trips_through_toml(duration in arb_duration()) {
        assert_toml_round_trip(&DurationHolder { duration })?;
    }

    #[test]
    fn model_config_fields_round_trip_through_toml(fields in arb_model_config_fields()) {
        assert_toml_round_trip(&fields)?;
    }

    #[test]
    fn provider_entries_round_trip_through_toml(entry in arb_provider_entry()) {
        assert_toml_round_trip(&entry)?;
    }

    #[test]
    fn app_config_round_trips_through_toml(config in arb_app_config()) {
        assert_toml_round_trip(&config)?;
    }
}
