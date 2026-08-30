use std::{os::windows::process::CommandExt, process::Command};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDateTime, SecondsFormat, Utc};

use crate::{model::DiagnosticEvent, timestamp::SessionClock};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct EventCollection {
    pub events: Vec<DiagnosticEvent>,
    pub warnings: Vec<String>,
}

pub fn collect_relevant_events(
    started_utc: DateTime<Utc>,
    clock: &SessionClock,
) -> EventCollection {
    collect_relevant_events_since(started_utc, started_utc, clock)
}

pub fn collect_relevant_events_since(
    query_from_utc: DateTime<Utc>,
    session_started_utc: DateTime<Utc>,
    clock: &SessionClock,
) -> EventCollection {
    let mut collection = EventCollection {
        events: Vec::new(),
        warnings: Vec::new(),
    };

    let query_until_utc = Utc::now() + Duration::seconds(2);
    for (channel, query) in [
        ("System", system_query(query_from_utc, query_until_utc)),
        (
            "Application",
            application_query(query_from_utc, query_until_utc),
        ),
    ] {
        match query_channel(
            channel,
            &query,
            query_from_utc,
            query_until_utc,
            session_started_utc,
            clock,
        ) {
            Ok(mut events) => collection.events.append(&mut events),
            Err(error) => collection
                .warnings
                .push(format!("{channel} Event Log query failed: {error:#}")),
        }
    }

    collection.events.sort_by_key(|event| event.time.utc);
    collection
}

fn system_query(started_utc: DateTime<Utc>, until_utc: DateTime<Utc>) -> String {
    let start = started_utc.to_rfc3339_opts(SecondsFormat::Millis, true);
    let end = until_utc.to_rfc3339_opts(SecondsFormat::Millis, true);
    format!(
        "*[System[TimeCreated[@SystemTime>='{start}' and @SystemTime<='{end}'] and (Level=1 or Level=2 or Level=3 or Provider[@Name='Microsoft-Windows-WHEA-Logger'] or Provider[@Name='Display'] or Provider[@Name='Microsoft-Windows-Kernel-Power'] or Provider[@Name='Microsoft-Windows-BugCheck'] or Provider[@Name='Microsoft-Windows-DxgKrnl'] or Provider[@Name='amdwddmg'] or Provider[@Name='amdkmdag'])]]"
    )
}

fn application_query(started_utc: DateTime<Utc>, until_utc: DateTime<Utc>) -> String {
    let start = started_utc.to_rfc3339_opts(SecondsFormat::Millis, true);
    let end = until_utc.to_rfc3339_opts(SecondsFormat::Millis, true);
    format!(
        "*[System[TimeCreated[@SystemTime>='{start}' and @SystemTime<='{end}'] and (Level=1 or Level=2 or Level=3 or Provider[@Name='Application Error'] or Provider[@Name='Windows Error Reporting'] or Provider[@Name='Application Hang'] or Provider[@Name='Microsoft-Windows-Resource-Exhaustion-Detector'])]]"
    )
}

fn query_channel(
    channel: &str,
    query: &str,
    query_from_utc: DateTime<Utc>,
    query_until_utc: DateTime<Utc>,
    session_started_utc: DateTime<Utc>,
    clock: &SessionClock,
) -> Result<Vec<DiagnosticEvent>> {
    let output = Command::new("wevtutil.exe")
        .args([
            "qe",
            channel,
            &format!("/q:{query}"),
            "/f:RenderedXml",
            "/rd:false",
            "/uni:false",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .with_context(|| format!("failed to start wevtutil for {channel}"))?;

    if !output.status.success() {
        anyhow::bail!(
            "wevtutil returned {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut events = Vec::new();
    for xml in split_events(&text) {
        let event_utc = extract_attribute(&xml, "TimeCreated", "SystemTime")
            .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        if event_utc < query_from_utc - Duration::seconds(1)
            || event_utc > query_until_utc + Duration::seconds(1)
        {
            continue;
        }
        let elapsed_ns = event_utc
            .signed_duration_since(session_started_utc)
            .num_nanoseconds()
            .unwrap_or(0)
            .max(0) as u64;
        let mut time = clock.now();
        time.utc = event_utc;
        time.monotonic_ns = elapsed_ns;

        let event = DiagnosticEvent {
            time,
            channel: extract_tag(&xml, "Channel").unwrap_or_else(|| channel.to_string()),
            provider: extract_attribute(&xml, "Provider", "Name"),
            event_id: extract_tag(&xml, "EventID").and_then(|value| value.parse().ok()),
            level: extract_tag(&xml, "Level").and_then(|value| value.parse().ok()),
            record_id: extract_tag(&xml, "EventRecordID").and_then(|value| value.parse().ok()),
            message: extract_tag(&xml, "Message").map(decode_xml),
            raw_xml: xml,
        };
        if is_historical_reprocessed_wer(&event, session_started_utc) {
            continue;
        }
        events.push(event);
    }
    Ok(events)
}

fn split_events(text: &str) -> Vec<String> {
    let mut events = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = text[cursor..].find("<Event") {
        let start = cursor + relative_start;
        let Some(relative_end) = text[start..].find("</Event>") else {
            break;
        };
        let end = start + relative_end + "</Event>".len();
        events.push(text[start..end].to_string());
        cursor = end;
    }
    events
}

fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let open_prefix = format!("<{tag}");
    let start = xml.find(&open_prefix)?;
    let content_start = start + xml[start..].find('>')? + 1;
    let close = format!("</{tag}>");
    let content_end = content_start + xml[content_start..].find(&close)?;
    Some(xml[content_start..content_end].trim().to_string())
}

fn extract_attribute(xml: &str, tag: &str, attribute: &str) -> Option<String> {
    let tag_start = xml.find(&format!("<{tag}"))?;
    let tag_end = tag_start + xml[tag_start..].find('>')?;
    let fragment = &xml[tag_start..tag_end];
    for quote in ['\'', '"'] {
        let needle = format!("{attribute}={quote}");
        if let Some(start) = fragment.find(&needle) {
            let value_start = start + needle.len();
            let value_end = value_start + fragment[value_start..].find(quote)?;
            return Some(decode_xml(fragment[value_start..value_end].to_string()));
        }
    }
    None
}

fn decode_xml(value: String) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#13;", "\r")
        .replace("&#10;", "\n")
        .replace("&amp;", "&")
}

pub(crate) fn is_live_kernel_event_code(event: &DiagnosticEvent, code: u32) -> bool {
    let provider = event.provider.as_deref().unwrap_or_default();
    if !provider
        .to_ascii_lowercase()
        .contains("windows error reporting")
    {
        return false;
    }
    event
        .message
        .as_deref()
        .is_some_and(|message| text_has_live_kernel_code(message, code))
        || text_has_live_kernel_code(&event.raw_xml, code)
}

fn text_has_live_kernel_code(text: &str, code: u32) -> bool {
    let lower = text.to_ascii_lowercase();
    let is_live_kernel = lower.contains("event name: livekernelevent")
        || lower.contains("eventname=livekernelevent")
        || lower.contains("eventtype=livekernelevent");
    if !is_live_kernel {
        return false;
    }
    let expected = code.to_string();
    lower.lines().any(|line| {
        let line = line.trim();
        line.strip_prefix("p1:")
            .or_else(|| line.strip_prefix("code="))
            .is_some_and(|value| value.trim().split_whitespace().next() == Some(expected.as_str()))
    }) || lower.contains(&format!("p1: {expected}"))
        || lower.contains(&format!("p1:{expected}"))
}

fn is_historical_reprocessed_wer(
    event: &DiagnosticEvent,
    session_started_utc: DateTime<Utc>,
) -> bool {
    let provider = event.provider.as_deref().unwrap_or_default();
    if !provider
        .to_ascii_lowercase()
        .contains("windows error reporting")
    {
        return false;
    }
    let Some(message) = event.message.as_deref() else {
        return false;
    };
    if !message
        .to_ascii_lowercase()
        .contains("event name: livekernelevent")
    {
        return false;
    }
    let Some(dump_local) = embedded_live_kernel_dump_local_time(message) else {
        return false;
    };
    let session_local = session_started_utc
        .with_timezone(&chrono::Local)
        .naive_local();
    dump_local < session_local - Duration::minutes(2)
}

pub(crate) fn embedded_live_kernel_dump_local_time(text: &str) -> Option<NaiveDateTime> {
    let upper = text.to_ascii_uppercase();
    for prefix in ["WATCHDOG-", "AMD_WATCHDOG-", "AMD_REPORT_UM-"] {
        let mut remaining = upper.as_str();
        while let Some(index) = remaining.find(prefix) {
            let timestamp = &remaining[index + prefix.len()..];
            if timestamp.len() >= 13 {
                if let Ok(value) = NaiveDateTime::parse_from_str(&timestamp[..13], "%Y%m%d-%H%M") {
                    return Some(value);
                }
            }
            remaining = &timestamp[timestamp.len().min(1)..];
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn wer_event(message: &str) -> DiagnosticEvent {
        DiagnosticEvent {
            time: crate::model::SampleTime {
                utc: Utc::now(),
                monotonic_ns: 0,
            },
            channel: "Application".into(),
            provider: Some("Windows Error Reporting".into()),
            event_id: Some(1001),
            level: Some(4),
            record_id: Some(1),
            message: Some(message.into()),
            raw_xml: String::new(),
        }
    }

    #[test]
    fn extracts_rendered_event_fields() {
        let xml = r#"<Event><System><Provider Name='Display'/><EventID>4101</EventID><Level>3</Level><TimeCreated SystemTime='2026-08-30T01:02:03.0000000Z'/><EventRecordID>42</EventRecordID><Channel>System</Channel></System><RenderingInfo><Message>Driver &amp; device reset</Message></RenderingInfo></Event>"#;
        assert_eq!(
            extract_attribute(xml, "Provider", "Name").as_deref(),
            Some("Display")
        );
        assert_eq!(extract_tag(xml, "EventID").as_deref(), Some("4101"));
        assert_eq!(
            extract_tag(xml, "Message").map(decode_xml).as_deref(),
            Some("Driver & device reset")
        );
    }

    #[test]
    fn splits_multiple_events() {
        let text = "header<Event><System/></Event>noise<Event><System/></Event>";
        assert_eq!(split_events(text).len(), 2);
    }

    #[test]
    fn recognizes_live_kernel_event_141() {
        let event = wer_event("Event Name: LiveKernelEvent\r\nP1: 141\r\n");
        assert!(is_live_kernel_event_code(&event, 141));
        assert!(!is_live_kernel_event_code(&event, 117));
    }

    #[test]
    fn rejects_reprocessed_live_kernel_report_from_before_session() {
        let local_start = chrono::Local
            .with_ymd_and_hms(2026, 8, 30, 2, 40, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let old = wer_event(
            "Event Name: LiveKernelEvent\r\nP1: 141\r\nAttached files:\r\nWATCHDOG-20260828-2121.dmp",
        );
        let current = wer_event(
            "Event Name: LiveKernelEvent\r\nP1: 141\r\nAttached files:\r\nWATCHDOG-20260830-0316.dmp",
        );
        assert!(is_historical_reprocessed_wer(&old, local_start));
        assert!(!is_historical_reprocessed_wer(&current, local_start));
    }
}
