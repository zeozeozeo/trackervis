use std::cmp::Ordering;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use walkdir::WalkDir;

use crate::cli::SortOrder;

#[derive(Debug, Clone)]
pub struct PlaylistItem {
    pub path: PathBuf,
    pub modified: SystemTime,
}

const SUPPORTED_EXTENSIONS: &[&str] = &[
    "669", "amf", "ams", "dbm", "digi", "dmf", "far", "gdm", "imf", "it", "med", "mod", "mt2",
    "mtm", "okt", "psm", "s3m", "stm", "ult", "umx", "xm",
];

pub fn discover(inputs: &[PathBuf], sort: SortOrder, recursive: bool) -> Result<Vec<PlaylistItem>> {
    let mut items = Vec::new();
    let supported: HashSet<&'static str> = SUPPORTED_EXTENSIONS.iter().copied().collect();

    for input in inputs {
        let metadata = std::fs::metadata(input)
            .with_context(|| format!("failed to stat input {}", input.display()))?;
        if metadata.is_file() {
            if is_supported_module(input, &supported) {
                items.push(PlaylistItem {
                    path: input.clone(),
                    modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                });
            }
            continue;
        }

        if metadata.is_dir() {
            let mut folder_items = scan_dir(input, recursive, &supported)?;
            folder_items.sort_by(|left, right| compare_items(left, right, sort));
            items.extend(folder_items);
            continue;
        }

        bail!("unsupported input type: {}", input.display());
    }

    if items.is_empty() {
        bail!("no supported module files were found");
    }

    Ok(items)
}

fn scan_dir(root: &Path, recursive: bool, supported: &HashSet<&str>) -> Result<Vec<PlaylistItem>> {
    let mut items = Vec::new();
    let walker = if recursive {
        WalkDir::new(root)
    } else {
        WalkDir::new(root).max_depth(1)
    };

    for entry in walker.into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        if !is_supported_module(&path, supported) {
            continue;
        }
        let modified = std::fs::metadata(&path)
            .with_context(|| format!("failed to stat {}", path.display()))?
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        items.push(PlaylistItem { path, modified });
    }
    Ok(items)
}

fn is_supported_module(path: &Path, supported: &HashSet<&str>) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|ext| supported.contains(&ext.to_ascii_lowercase()[..]))
        .unwrap_or(false)
}

fn compare_items(left: &PlaylistItem, right: &PlaylistItem, sort: SortOrder) -> Ordering {
    match sort {
        SortOrder::Filename => left.path.file_name().cmp(&right.path.file_name()),
        SortOrder::Mtime => left
            .modified
            .cmp(&right.modified)
            .then_with(|| left.path.file_name().cmp(&right.path.file_name())),
    }
}

#[cfg(test)]
mod tests {
    use super::compare_items;
    use crate::cli::SortOrder;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    #[test]
    fn filename_sort_uses_basename() {
        let left = super::PlaylistItem {
            path: PathBuf::from("b.mod"),
            modified: SystemTime::UNIX_EPOCH,
        };
        let right = super::PlaylistItem {
            path: PathBuf::from("a.mod"),
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(5),
        };

        assert!(compare_items(&left, &right, SortOrder::Filename).is_gt());
    }

    #[test]
    fn mtime_sort_uses_modified_then_name() {
        let left = super::PlaylistItem {
            path: PathBuf::from("b.mod"),
            modified: SystemTime::UNIX_EPOCH,
        };
        let right = super::PlaylistItem {
            path: PathBuf::from("a.mod"),
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(5),
        };

        assert!(compare_items(&left, &right, SortOrder::Mtime).is_lt());
    }
}
