use super::{Segment, SegmentData};
use crate::config::{InputData, ModelConfig, NormalizedUsage, SegmentId, TranscriptEntry};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct ContextWindowSegment;

impl ContextWindowSegment {
    pub fn new() -> Self {
        Self
    }

    /// Get context limit for the specified model
    fn get_context_limit_for_model(model_id: &str) -> u32 {
        let model_config = ModelConfig::load();
        model_config.get_context_limit(model_id)
    }
}

impl Segment for ContextWindowSegment {
    fn collect(&self, input: &InputData) -> Option<SegmentData> {
        // Dynamically determine context limit based on current model ID
        let context_limit = Self::get_context_limit_for_model(&input.model.id);

        let usage_opt = parse_transcript_usage(&input.transcript_path);
        let context_used_token_opt = usage_opt.as_ref().map(|u| u.display_tokens());

        let (percentage_display, tokens_display) = match context_used_token_opt {
            Some(context_used_token) => {
                let context_used_rate = (context_used_token as f64 / context_limit as f64) * 100.0;

                let percentage = if context_used_rate.fract() == 0.0 {
                    format!("{:.0}%", context_used_rate)
                } else {
                    format!("{:.1}%", context_used_rate)
                };

                let tokens = if context_used_token >= 1000 {
                    let k_value = context_used_token as f64 / 1000.0;
                    if k_value.fract() == 0.0 {
                        format!("{}k", k_value as u32)
                    } else {
                        format!("{:.1}k", k_value)
                    }
                } else {
                    context_used_token.to_string()
                };

                (percentage, tokens)
            }
            None => {
                // No usage data available
                ("-".to_string(), "-".to_string())
            }
        };

        let mut metadata = HashMap::new();
        match context_used_token_opt {
            Some(context_used_token) => {
                let context_used_rate = (context_used_token as f64 / context_limit as f64) * 100.0;
                metadata.insert("tokens".to_string(), context_used_token.to_string());
                metadata.insert("percentage".to_string(), context_used_rate.to_string());
            }
            None => {
                metadata.insert("tokens".to_string(), "-".to_string());
                metadata.insert("percentage".to_string(), "-".to_string());
            }
        }
        metadata.insert("limit".to_string(), context_limit.to_string());
        metadata.insert("model".to_string(), input.model.id.clone());

        // Cumulative cache ratio across all assistant turns in the session:
        // Σ cache_read / Σ (input + cache_creation + cache_read)
        let cache_hit_display = match accumulate_cache_ratio(Path::new(&input.transcript_path)) {
            Some((cache_read, denom)) if denom > 0 => {
                let rate = (cache_read as f64 / denom as f64) * 100.0;
                metadata.insert("cache_hit_rate".to_string(), rate.to_string());
                metadata.insert("cache_read_tokens".to_string(), cache_read.to_string());
                if rate.fract() == 0.0 {
                    format!("{:.0}%", rate)
                } else {
                    format!("{:.1}%", rate)
                }
            }
            _ => "-".to_string(),
        };

        Some(SegmentData {
            primary: format!(
                "{} · {} tokens · cache {}",
                percentage_display, tokens_display, cache_hit_display
            ),
            secondary: String::new(),
            metadata,
        })
    }

    fn id(&self) -> SegmentId {
        SegmentId::ContextWindow
    }
}

fn parse_transcript_usage<P: AsRef<Path>>(transcript_path: P) -> Option<NormalizedUsage> {
    let path = transcript_path.as_ref();

    // Try to parse from current transcript file
    if let Some(usage) = try_parse_transcript_file(path) {
        return Some(usage);
    }

    // If file doesn't exist, try to find usage from project history
    if !path.exists() {
        if let Some(usage) = try_find_usage_from_project_history(path) {
            return Some(usage);
        }
    }

    None
}

fn try_parse_transcript_file(path: &Path) -> Option<NormalizedUsage> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader
        .lines()
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_default();

    if lines.is_empty() {
        return None;
    }

    // Check if the last line is a summary
    let last_line = lines.last()?.trim();
    if let Ok(entry) = serde_json::from_str::<TranscriptEntry>(last_line) {
        if entry.r#type.as_deref() == Some("summary") {
            // Handle summary case: find usage by leafUuid
            if let Some(leaf_uuid) = &entry.leaf_uuid {
                let project_dir = path.parent()?;
                return find_usage_by_leaf_uuid(leaf_uuid, project_dir);
            }
        }
    }

    // Normal case: find the last assistant message in current file
    for line in lines.iter().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Ok(entry) = serde_json::from_str::<TranscriptEntry>(line) {
            if entry.r#type.as_deref() == Some("assistant") {
                if let Some(message) = &entry.message {
                    if let Some(raw_usage) = &message.usage {
                        let normalized = raw_usage.clone().normalize();
                        return Some(normalized);
                    }
                }
            }
        }
    }

    None
}

fn find_usage_by_leaf_uuid(leaf_uuid: &str, project_dir: &Path) -> Option<NormalizedUsage> {
    // Search for the leafUuid across all session files in the project directory
    let entries = fs::read_dir(project_dir).ok()?;

    for entry in entries {
        let entry = entry.ok()?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }

        if let Some(usage) = search_uuid_in_file(&path, leaf_uuid) {
            return Some(usage);
        }
    }

    None
}

fn search_uuid_in_file(path: &Path, target_uuid: &str) -> Option<NormalizedUsage> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader
        .lines()
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_default();

    // Find the message with target_uuid
    for line in &lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Ok(entry) = serde_json::from_str::<TranscriptEntry>(line) {
            if let Some(uuid) = &entry.uuid {
                if uuid == target_uuid {
                    // Found the target message, check its type
                    if entry.r#type.as_deref() == Some("assistant") {
                        // Direct assistant message with usage
                        if let Some(message) = &entry.message {
                            if let Some(raw_usage) = &message.usage {
                                let normalized = raw_usage.clone().normalize();
                                return Some(normalized);
                            }
                        }
                    } else if entry.r#type.as_deref() == Some("user") {
                        // User message, need to find the parent assistant message
                        if let Some(parent_uuid) = &entry.parent_uuid {
                            return find_assistant_message_by_uuid(&lines, parent_uuid);
                        }
                    }
                    break;
                }
            }
        }
    }

    None
}

fn find_assistant_message_by_uuid(lines: &[String], target_uuid: &str) -> Option<NormalizedUsage> {
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Ok(entry) = serde_json::from_str::<TranscriptEntry>(line) {
            if let Some(uuid) = &entry.uuid {
                if uuid == target_uuid && entry.r#type.as_deref() == Some("assistant") {
                    if let Some(message) = &entry.message {
                        if let Some(raw_usage) = &message.usage {
                            let normalized = raw_usage.clone().normalize();
                            return Some(normalized);
                        }
                    }
                }
            }
        }
    }

    None
}

fn try_find_usage_from_project_history(transcript_path: &Path) -> Option<NormalizedUsage> {
    let project_dir = transcript_path.parent()?;

    // Find the most recent session file in the project directory
    let mut session_files: Vec<PathBuf> = Vec::new();
    let entries = fs::read_dir(project_dir).ok()?;

    for entry in entries {
        let entry = entry.ok()?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            session_files.push(path);
        }
    }

    if session_files.is_empty() {
        return None;
    }

    // Sort by modification time (most recent first)
    session_files.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH)
    });
    session_files.reverse();

    // Try to find usage from the most recent session
    for session_path in &session_files {
        if let Some(usage) = try_parse_transcript_file(session_path) {
            return Some(usage);
        }
    }

    None
}

/// Accumulate the cache ratio across all assistant turns in the session.
/// Returns (total_cache_read, total_input_side_tokens).
fn accumulate_cache_ratio(path: &Path) -> Option<(u64, u64)> {
    if let Some(r) = accumulate_cache_in_file(path) {
        return Some(r);
    }

    // Fallback: most recent session file in the project directory
    if !path.exists() {
        let project_dir = path.parent()?;
        let mut session_files: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(project_dir).ok()? {
            let entry = entry.ok()?;
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                session_files.push(p);
            }
        }
        session_files.sort_by_key(|p| {
            fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH)
        });
        session_files.reverse();
        for sp in &session_files {
            if let Some(r) = accumulate_cache_in_file(sp) {
                return Some(r);
            }
        }
    }

    None
}

/// Sum cache_read and the input-side denominator over every assistant message in a file.
fn accumulate_cache_in_file(path: &Path) -> Option<(u64, u64)> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut cache_read: u64 = 0;
    let mut denom: u64 = 0;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Ok(entry) = serde_json::from_str::<TranscriptEntry>(line) {
            if entry.r#type.as_deref() == Some("assistant") {
                if let Some(message) = &entry.message {
                    if let Some(raw_usage) = &message.usage {
                        let n = raw_usage.clone().normalize();
                        cache_read += n.cache_read_input_tokens as u64;
                        denom += (n.input_tokens
                            + n.cache_creation_input_tokens
                            + n.cache_read_input_tokens) as u64;
                    }
                }
            }
        }
    }

    if denom == 0 {
        return None;
    }
    Some((cache_read, denom))
}
