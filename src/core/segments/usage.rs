use super::{Segment, SegmentData};
use crate::config::{InputData, SegmentId};
use crate::utils::credentials;
use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
struct ApiUsageResponse {
    five_hour: UsagePeriod,
    seven_day: UsagePeriod,
}

#[derive(Debug, Deserialize)]
struct UsagePeriod {
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ApiUsageCache {
    #[serde(default)]
    five_hour_utilization: f64,
    #[serde(default)]
    seven_day_utilization: f64,
    #[serde(default)]
    five_hour_resets_at: Option<String>,
    #[serde(default)]
    seven_day_resets_at: Option<String>,
    // Legacy single field (seven_day reset), kept for backward compatibility
    #[serde(default)]
    resets_at: Option<String>,
    cached_at: String,
}

#[derive(Default)]
pub struct UsageSegment;

impl UsageSegment {
    pub fn new() -> Self {
        Self
    }

    /// ANSI color (foreground) for a remaining-quota percentage:
    /// green when plenty left, yellow when getting low, red when nearly out.
    fn remaining_color(remaining: u8) -> &'static str {
        if remaining >= 50 {
            "\x1b[92m" // bright green
        } else if remaining >= 20 {
            "\x1b[93m" // bright yellow
        } else {
            "\x1b[91m" // bright red
        }
    }

    /// Format a reset timestamp in local time.
    /// `with_date = false` -> "HH:MM" (for the 5h window, always today)
    /// `with_date = true`  -> "M/D HH:MM" (for the 7d window, days away)
    fn format_reset_time(reset_time_str: Option<&str>, with_date: bool) -> String {
        if let Some(time_str) = reset_time_str {
            if let Ok(dt) = DateTime::parse_from_rfc3339(time_str) {
                let local_dt = dt.with_timezone(&Local);
                return if with_date {
                    format!(
                        "{}/{} {:02}:{:02}",
                        local_dt.month(),
                        local_dt.day(),
                        local_dt.hour(),
                        local_dt.minute()
                    )
                } else {
                    format!("{:02}:{:02}", local_dt.hour(), local_dt.minute())
                };
            }
        }
        "?".to_string()
    }

    fn get_cache_path() -> Option<std::path::PathBuf> {
        let home = dirs::home_dir()?;
        Some(
            home.join(".claude")
                .join("ccline")
                .join(".api_usage_cache.json"),
        )
    }

    fn load_cache(&self) -> Option<ApiUsageCache> {
        let cache_path = Self::get_cache_path()?;
        if !cache_path.exists() {
            return None;
        }

        let content = std::fs::read_to_string(&cache_path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn save_cache(&self, cache: &ApiUsageCache) {
        if let Some(cache_path) = Self::get_cache_path() {
            if let Some(parent) = cache_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(cache) {
                let _ = std::fs::write(&cache_path, json);
            }
        }
    }

    fn is_cache_valid(&self, cache: &ApiUsageCache, cache_duration: u64) -> bool {
        if let Ok(cached_at) = DateTime::parse_from_rfc3339(&cache.cached_at) {
            let now = Utc::now();
            let elapsed = now.signed_duration_since(cached_at.with_timezone(&Utc));
            elapsed.num_seconds() < cache_duration as i64
        } else {
            false
        }
    }

    fn get_claude_code_version() -> String {
        use std::process::Command;

        let output = Command::new("npm")
            .args(["view", "@anthropic-ai/claude-code", "version"])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !version.is_empty() {
                    return format!("claude-code/{}", version);
                }
            }
            _ => {}
        }

        "claude-code".to_string()
    }

    fn get_proxy_from_settings() -> Option<String> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        let settings_path = format!("{}/.claude/settings.json", home);

        let content = std::fs::read_to_string(&settings_path).ok()?;
        let settings: serde_json::Value = serde_json::from_str(&content).ok()?;

        // Try HTTPS_PROXY first, then HTTP_PROXY
        settings
            .get("env")?
            .get("HTTPS_PROXY")
            .or_else(|| settings.get("env")?.get("HTTP_PROXY"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn fetch_api_usage(
        &self,
        api_base_url: &str,
        token: &str,
        timeout_secs: u64,
    ) -> Option<ApiUsageResponse> {
        let url = format!("{}/api/oauth/usage", api_base_url);
        let user_agent = Self::get_claude_code_version();

        let agent = if let Some(proxy_url) = Self::get_proxy_from_settings() {
            if let Ok(proxy) = ureq::Proxy::new(&proxy_url) {
                ureq::Agent::config_builder()
                    .proxy(Some(proxy))
                    .build()
                    .new_agent()
            } else {
                ureq::Agent::new_with_defaults()
            }
        } else {
            ureq::Agent::new_with_defaults()
        };

        let response = agent
            .get(&url)
            .header("Authorization", &format!("Bearer {}", token))
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("User-Agent", &user_agent)
            .config()
            .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
            .build()
            .call()
            .ok()?;

        response.into_body().read_json().ok()
    }
}

impl Segment for UsageSegment {
    fn collect(&self, _input: &InputData) -> Option<SegmentData> {
        let token = credentials::get_oauth_token()?;

        // Load config from file to get segment options
        let config = crate::config::Config::load().ok()?;
        let segment_config = config.segments.iter().find(|s| s.id == SegmentId::Usage);

        let api_base_url = segment_config
            .and_then(|sc| sc.options.get("api_base_url"))
            .and_then(|v| v.as_str())
            .unwrap_or("https://api.anthropic.com");

        let cache_duration = segment_config
            .and_then(|sc| sc.options.get("cache_duration"))
            .and_then(|v| v.as_u64())
            .unwrap_or(300);

        let timeout = segment_config
            .and_then(|sc| sc.options.get("timeout"))
            .and_then(|v| v.as_u64())
            .unwrap_or(2);

        let cached_data = self.load_cache();
        let use_cached = cached_data
            .as_ref()
            .map(|cache| self.is_cache_valid(cache, cache_duration))
            .unwrap_or(false);

        let (five_hour_util, seven_day_util, five_hour_resets, seven_day_resets) = if use_cached {
            let cache = cached_data.unwrap();
            let seven_day_resets = cache.seven_day_resets_at.or(cache.resets_at);
            (
                cache.five_hour_utilization,
                cache.seven_day_utilization,
                cache.five_hour_resets_at,
                seven_day_resets,
            )
        } else {
            match self.fetch_api_usage(api_base_url, &token, timeout) {
                Some(response) => {
                    let cache = ApiUsageCache {
                        five_hour_utilization: response.five_hour.utilization,
                        seven_day_utilization: response.seven_day.utilization,
                        five_hour_resets_at: response.five_hour.resets_at.clone(),
                        seven_day_resets_at: response.seven_day.resets_at.clone(),
                        resets_at: response.seven_day.resets_at.clone(),
                        cached_at: Utc::now().to_rfc3339(),
                    };
                    self.save_cache(&cache);
                    (
                        response.five_hour.utilization,
                        response.seven_day.utilization,
                        response.five_hour.resets_at,
                        response.seven_day.resets_at,
                    )
                }
                None => {
                    if let Some(cache) = cached_data {
                        let seven_day_resets = cache.seven_day_resets_at.or(cache.resets_at);
                        (
                            cache.five_hour_utilization,
                            cache.seven_day_utilization,
                            cache.five_hour_resets_at,
                            seven_day_resets,
                        )
                    } else {
                        return None;
                    }
                }
            }
        };

        // Remaining quota = 100 - utilization, clamped to [0, 100].
        let session_remaining = (100.0 - five_hour_util).round().clamp(0.0, 100.0) as u8;
        let week_remaining = (100.0 - seven_day_util).round().clamp(0.0, 100.0) as u8;

        // Dedicated second-line layout: "Session 40% 🔄 17:20   Week 59% 🔄 6/21 23:00"
        let primary = format!(
            "Session {sc}{s}%\x1b[0m \x1b[90m🔄 {sr}\x1b[0m   \
             Week {wc}{w}%\x1b[0m \x1b[90m🔄 {wr}\x1b[0m",
            sc = Self::remaining_color(session_remaining),
            s = session_remaining,
            sr = Self::format_reset_time(five_hour_resets.as_deref(), false),
            wc = Self::remaining_color(week_remaining),
            w = week_remaining,
            wr = Self::format_reset_time(seven_day_resets.as_deref(), true),
        );

        let mut metadata = HashMap::new();
        metadata.insert(
            "five_hour_utilization".to_string(),
            five_hour_util.to_string(),
        );
        metadata.insert(
            "seven_day_utilization".to_string(),
            seven_day_util.to_string(),
        );
        metadata.insert(
            "session_remaining".to_string(),
            session_remaining.to_string(),
        );
        metadata.insert("week_remaining".to_string(), week_remaining.to_string());

        Some(SegmentData {
            primary,
            secondary: String::new(),
            metadata,
        })
    }

    fn id(&self) -> SegmentId {
        SegmentId::Usage
    }
}
