#[cfg(windows)]
pub fn init_dll_search_path() {
    if option_env!("TRACKERVIS_LIBOPENMPT_DYNAMIC") != Some("1") {
        return;
    }

    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetDllDirectoryW(path_name: *const u16) -> i32;
    }

    if let Some(path) = option_env!("TRACKERVIS_LIBOPENMPT_BIN") {
        let wide: Vec<u16> = OsStr::new(path).encode_wide().chain([0]).collect();
        unsafe {
            SetDllDirectoryW(wide.as_ptr());
        }
        return;
    }

    let fallback = PathBuf::from(".vendor").join("libopenmpt").join("bin");
    if fallback.is_dir() {
        let wide: Vec<u16> = fallback.as_os_str().encode_wide().chain([0]).collect();
        unsafe {
            SetDllDirectoryW(wide.as_ptr());
        }
    }
}

#[cfg(not(windows))]
pub fn init_dll_search_path() {}
