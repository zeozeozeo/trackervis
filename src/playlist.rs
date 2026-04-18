use anyhow::Result;

use crate::openmpt::ModuleSource;

#[derive(Debug, Clone)]
pub struct PlaylistEntry {
    pub source: ModuleSource,
    pub playlist_index: usize,
    pub playlist_len: usize,
    pub subsong_index: usize,
    pub label: String,
    pub filename: String,
}

pub fn expand_sources(sources: Vec<ModuleSource>) -> Result<Vec<PlaylistEntry>> {
    let mut expanded = Vec::new();
    for source in sources {
        let metadata = source.metadata()?;
        let filename = source
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        let subsong_count = metadata.subsong_count.max(1);
        for subsong_index in 0..subsong_count {
            expanded.push(PlaylistEntry {
                source: source.clone(),
                playlist_index: 0,
                playlist_len: 0,
                subsong_index,
                label: format_entry_label(&metadata.label, subsong_index, subsong_count),
                filename: filename.clone(),
            });
        }
    }

    let playlist_len = expanded.len();
    for (playlist_index, entry) in expanded.iter_mut().enumerate() {
        entry.playlist_index = playlist_index;
        entry.playlist_len = playlist_len;
    }
    Ok(expanded)
}

pub fn format_entry_label(base_label: &str, subsong_index: usize, subsong_count: usize) -> String {
    if subsong_count > 1 {
        format!("{base_label} (Subsong {})", subsong_index + 1)
    } else {
        base_label.to_owned()
    }
}
