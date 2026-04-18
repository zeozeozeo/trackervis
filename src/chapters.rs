use std::time::Duration;

pub fn format_timestamp(duration: Duration) -> String {
    let total = duration.as_secs();
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

pub fn format_chapter_lines(entries: &[(Duration, String)]) -> String {
    let mut out = String::new();
    for (index, (timestamp, label)) in entries.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&format!("{} {}", format_timestamp(*timestamp), label));
    }
    out
}
