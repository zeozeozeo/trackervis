use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail};
use openmpt_sys::{
    openmpt_error_func, openmpt_free_string, openmpt_get_supported_extensions, openmpt_log_func,
    openmpt_module,
    openmpt_module_format_pattern_row_channel_command, openmpt_module_get_current_pattern,
    openmpt_module_get_current_row, openmpt_module_get_instrument_name,
    openmpt_module_get_metadata, openmpt_module_get_num_channels,
    openmpt_module_get_num_instruments, openmpt_module_get_num_samples,
    openmpt_module_get_num_subsongs, openmpt_module_get_pattern_row_channel_command,
    openmpt_module_get_sample_name, openmpt_module_read_interleaved_float_stereo,
    openmpt_module_select_subsong, openmpt_module_set_repeat_count,
};

#[repr(C)]
pub struct openmpt_module_ext {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct openmpt_module_ext_interface_interactive {
    set_current_speed: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32) -> i32>,
    set_current_tempo: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32) -> i32>,
    set_tempo_factor: Option<unsafe extern "C" fn(*mut openmpt_module_ext, f64) -> i32>,
    get_tempo_factor: Option<unsafe extern "C" fn(*mut openmpt_module_ext) -> f64>,
    set_pitch_factor: Option<unsafe extern "C" fn(*mut openmpt_module_ext, f64) -> i32>,
    get_pitch_factor: Option<unsafe extern "C" fn(*mut openmpt_module_ext) -> f64>,
    set_global_volume: Option<unsafe extern "C" fn(*mut openmpt_module_ext, f64) -> i32>,
    get_global_volume: Option<unsafe extern "C" fn(*mut openmpt_module_ext) -> f64>,
    set_channel_volume: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32, f64) -> i32>,
    get_channel_volume: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32) -> f64>,
    set_channel_mute_status: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32, i32) -> i32>,
    get_channel_mute_status: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32) -> i32>,
    set_instrument_mute_status:
        Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32, i32) -> i32>,
    get_instrument_mute_status: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32) -> i32>,
    play_note: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32, i32, f64, f64) -> i32>,
    stop_note: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32) -> i32>,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct openmpt_module_ext_interface_interactive2 {
    note_off: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32) -> i32>,
    note_fade: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32) -> i32>,
    set_channel_panning: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32, f64) -> i32>,
    get_channel_panning: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32) -> f64>,
    set_note_finetune: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32, f64) -> i32>,
    get_note_finetune: Option<unsafe extern "C" fn(*mut openmpt_module_ext, i32) -> f64>,
}

#[link(name = "openmpt")]
unsafe extern "C" {
    fn openmpt_module_ext_create_from_memory(
        filedata: *const c_void,
        filesize: usize,
        logfunc: openmpt_log_func,
        loguser: *mut c_void,
        errfunc: openmpt_error_func,
        erruser: *mut c_void,
        error: *mut c_int,
        error_message: *mut *const c_char,
        ctls: *const openmpt_sys::openmpt_module_initial_ctl,
    ) -> *mut openmpt_module_ext;
    fn openmpt_module_ext_destroy(mod_ext: *mut openmpt_module_ext);
    fn openmpt_module_ext_get_module(mod_ext: *mut openmpt_module_ext) -> *mut openmpt_module;
    fn openmpt_module_ext_get_interface(
        mod_ext: *mut openmpt_module_ext,
        interface_id: *const c_char,
        interface: *mut c_void,
        interface_size: usize,
    ) -> c_int;
}

const INTERACTIVE_ID: &[u8] = b"interactive\0";
const INTERACTIVE2_ID: &[u8] = b"interactive2\0";

static SUPPORTED_EXTENSIONS: OnceLock<Vec<String>> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct ModuleSource {
    pub path: PathBuf,
    bytes: Arc<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct ModuleMetadata {
    pub label: String,
    pub subsong_count: usize,
}

pub struct ModuleHandle {
    _owner: Arc<Vec<u8>>,
    ext_module: *mut openmpt_module_ext,
    module: *mut openmpt_module,
    interactive: openmpt_module_ext_interface_interactive,
    interactive2: Option<openmpt_module_ext_interface_interactive2>,
    channel_count: usize,
}

unsafe impl Send for ModuleHandle {}

pub fn supported_extensions() -> &'static [String] {
    SUPPORTED_EXTENSIONS.get_or_init(load_supported_extensions).as_slice()
}

impl ModuleSource {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read module {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            bytes: Arc::new(bytes),
        })
    }

    pub fn metadata(&self) -> Result<ModuleMetadata> {
        let handle = self.open()?;
        Ok(ModuleMetadata {
            label: handle.display_label(&self.path),
            subsong_count: handle.subsong_count(),
        })
    }

    pub fn open(&self) -> Result<ModuleHandle> {
        let mut error = 0;
        let mut error_message: *const c_char = ptr::null();
        let ext_module = unsafe {
            openmpt_module_ext_create_from_memory(
                self.bytes.as_ptr().cast(),
                self.bytes.len(),
                None,
                ptr::null_mut(),
                None,
                ptr::null_mut(),
                &mut error,
                &mut error_message,
                ptr::null(),
            )
        };

        if ext_module.is_null() {
            let message = error_message_to_string(error_message)
                .unwrap_or_else(|| format!("libopenmpt error code {error}"));
            bail!("failed to load {}: {message}", self.path.display());
        }

        let module = unsafe { openmpt_module_ext_get_module(ext_module) };
        if module.is_null() {
            unsafe { openmpt_module_ext_destroy(ext_module) };
            bail!("failed to obtain module handle for {}", self.path.display());
        }

        let mut interactive = openmpt_module_ext_interface_interactive {
            set_current_speed: None,
            set_current_tempo: None,
            set_tempo_factor: None,
            get_tempo_factor: None,
            set_pitch_factor: None,
            get_pitch_factor: None,
            set_global_volume: None,
            get_global_volume: None,
            set_channel_volume: None,
            get_channel_volume: None,
            set_channel_mute_status: None,
            get_channel_mute_status: None,
            set_instrument_mute_status: None,
            get_instrument_mute_status: None,
            play_note: None,
            stop_note: None,
        };

        let ok = unsafe {
            openmpt_module_ext_get_interface(
                ext_module,
                INTERACTIVE_ID.as_ptr().cast(),
                (&mut interactive as *mut openmpt_module_ext_interface_interactive).cast(),
                std::mem::size_of::<openmpt_module_ext_interface_interactive>(),
            )
        };
        if ok == 0 {
            unsafe { openmpt_module_ext_destroy(ext_module) };
            bail!("libopenmpt interactive interface is unavailable");
        }

        let mut interactive2 = openmpt_module_ext_interface_interactive2 {
            note_off: None,
            note_fade: None,
            set_channel_panning: None,
            get_channel_panning: None,
            set_note_finetune: None,
            get_note_finetune: None,
        };
        let interactive2_ok = unsafe {
            openmpt_module_ext_get_interface(
                ext_module,
                INTERACTIVE2_ID.as_ptr().cast(),
                (&mut interactive2 as *mut openmpt_module_ext_interface_interactive2).cast(),
                std::mem::size_of::<openmpt_module_ext_interface_interactive2>(),
            )
        };

        unsafe {
            openmpt_module_set_repeat_count(module, 0);
        }

        let channel_count = unsafe { openmpt_module_get_num_channels(module) }.max(0) as usize;

        Ok(ModuleHandle {
            _owner: Arc::clone(&self.bytes),
            ext_module,
            module,
            interactive,
            interactive2: (interactive2_ok != 0).then_some(interactive2),
            channel_count,
        })
    }

    pub fn open_subsong(&self, subsong: usize) -> Result<ModuleHandle> {
        let mut handle = self.open()?;
        handle.select_subsong(subsong)?;
        Ok(handle)
    }
}

impl ModuleHandle {
    pub fn channel_count(&self) -> usize {
        self.channel_count
    }

    pub fn subsong_count(&self) -> usize {
        unsafe { openmpt_module_get_num_subsongs(self.module) }.max(1) as usize
    }

    pub fn select_subsong(&mut self, subsong: usize) -> Result<()> {
        let ok = unsafe { openmpt_module_select_subsong(self.module, subsong as i32) };
        if ok == 0 {
            bail!("failed to select subsong {}", subsong + 1);
        }
        Ok(())
    }

    pub fn display_label(&self, path: &Path) -> String {
        let title = self.metadata("title");
        let artist = self.metadata("artist");
        let fallback = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("untitled")
            .to_owned();

        match (artist.as_deref(), title.as_deref()) {
            (Some(artist), Some(title)) if !artist.is_empty() && !title.is_empty() => {
                format!("{artist} - {title}")
            }
            (_, Some(title)) if !title.is_empty() => title.to_owned(),
            _ => fallback,
        }
    }

    pub fn metadata(&self, key: &str) -> Option<String> {
        let c_key = CString::new(key).ok()?;
        let ptr = unsafe { openmpt_module_get_metadata(self.module, c_key.as_ptr()) };
        if ptr.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .trim()
            .to_owned();
        if text.is_empty() { None } else { Some(text) }
    }

    pub fn read_stereo(
        &mut self,
        sample_rate: u32,
        frames: usize,
        scratch: &mut Vec<f32>,
    ) -> usize {
        scratch.resize(frames * 2, 0.0);
        let rendered = unsafe {
            openmpt_module_read_interleaved_float_stereo(
                self.module,
                sample_rate as i32,
                frames,
                scratch.as_mut_ptr(),
            )
        };
        scratch.truncate(rendered * 2);
        rendered
    }

    pub fn mute_all_except(&mut self, selected: usize) -> Result<()> {
        let set_mute = self
            .interactive
            .set_channel_mute_status
            .ok_or_else(|| anyhow!("interactive mute API missing"))?;
        for channel in 0..self.channel_count {
            let mute = if channel == selected { 0 } else { 1 };
            let ok = unsafe { set_mute(self.ext_module, channel as i32, mute) };
            if ok == 0 {
                bail!("failed to set mute state for channel {channel}");
            }
        }
        Ok(())
    }

    pub fn current_pattern(&self) -> usize {
        unsafe { openmpt_module_get_current_pattern(self.module) }.max(0) as usize
    }

    pub fn current_row(&self) -> usize {
        unsafe { openmpt_module_get_current_row(self.module) }.max(0) as usize
    }

    pub fn instrument_count(&self) -> usize {
        unsafe { openmpt_module_get_num_instruments(self.module) }.max(0) as usize
    }

    pub fn sample_count(&self) -> usize {
        unsafe { openmpt_module_get_num_samples(self.module) }.max(0) as usize
    }

    pub fn pattern_command(&self, pattern: usize, row: usize, channel: usize, command: i32) -> u8 {
        if channel >= self.channel_count {
            return 0;
        }
        unsafe {
            openmpt_module_get_pattern_row_channel_command(
                self.module,
                pattern as i32,
                row as i32,
                channel as i32,
                command,
            )
        }
    }

    pub fn instrument_name(&self, index: usize) -> Option<String> {
        allocated_string(unsafe { openmpt_module_get_instrument_name(self.module, index as i32) })
    }

    pub fn sample_name(&self, index: usize) -> Option<String> {
        allocated_string(unsafe { openmpt_module_get_sample_name(self.module, index as i32) })
    }

    pub fn channel_sample_label(&self, pattern: usize, row: usize, channel: usize) -> String {
        if channel >= self.channel_count {
            return String::new();
        }
        let instrument_count = self.instrument_count();
        let sample_count = self.sample_count();
        let slot = self.pattern_command(
            pattern,
            row,
            channel,
            openmpt_sys::OPENMPT_MODULE_COMMAND_INSTRUMENT as i32,
        ) as usize;
        if slot == 0 {
            return String::new();
        }

        let display = if instrument_count > 0 {
            let name = self
                .instrument_name(slot.saturating_sub(1))
                .unwrap_or_default();
            format_label(slot, name)
        } else if sample_count > 0 {
            let name = self.sample_name(slot.saturating_sub(1)).unwrap_or_default();
            format_label(slot, name)
        } else {
            String::new()
        };
        display.trim_end().to_owned()
    }

    pub fn channel_effect_label(&self, pattern: usize, row: usize, channel: usize) -> String {
        if channel >= self.channel_count {
            return String::new();
        }
        let volume = format_compact_effect(
            self.formatted_pattern_command(
                pattern,
                row,
                channel,
                openmpt_sys::OPENMPT_MODULE_COMMAND_VOLUMEEFFECT as i32,
            ),
            self.formatted_pattern_command(
                pattern,
                row,
                channel,
                openmpt_sys::OPENMPT_MODULE_COMMAND_VOLUME as i32,
            ),
        );
        let effect = format_compact_effect(
            self.formatted_pattern_command(
                pattern,
                row,
                channel,
                openmpt_sys::OPENMPT_MODULE_COMMAND_EFFECT as i32,
            ),
            self.formatted_pattern_command(
                pattern,
                row,
                channel,
                openmpt_sys::OPENMPT_MODULE_COMMAND_PARAMETER as i32,
            ),
        );

        match (volume.is_empty(), effect.is_empty()) {
            (true, true) => String::new(),
            (false, true) => volume,
            (true, false) => effect,
            (false, false) => format!("{volume} {effect}"),
        }
    }

    pub fn channel_panning(&self, channel: usize) -> Option<f32> {
        if channel >= self.channel_count {
            return None;
        }
        let getter = self.interactive2?.get_channel_panning?;
        let value = unsafe { getter(self.ext_module, channel as i32) };
        Some(value.clamp(-1.0, 1.0) as f32)
    }

    pub fn channel_panning_snapshot(&self) -> Option<Vec<f32>> {
        let mut values = Vec::with_capacity(self.channel_count);
        for channel in 0..self.channel_count {
            values.push(self.channel_panning(channel)?);
        }
        Some(values)
    }

    fn formatted_pattern_command(
        &self,
        pattern: usize,
        row: usize,
        channel: usize,
        command: i32,
    ) -> String {
        if channel >= self.channel_count {
            return String::new();
        }
        allocated_string(unsafe {
            openmpt_module_format_pattern_row_channel_command(
                self.module,
                pattern as i32,
                row as i32,
                channel as i32,
                command,
            )
        })
        .unwrap_or_default()
    }
}

impl Drop for ModuleHandle {
    fn drop(&mut self) {
        unsafe {
            openmpt_module_ext_destroy(self.ext_module);
        }
    }
}

pub fn snapshot_isolated_channel_annotations(
    isolated: &[ModuleHandle],
    labels: &mut Vec<String>,
    effects: &mut Vec<String>,
) {
    if labels.len() < isolated.len() {
        labels.resize(isolated.len(), String::new());
    }
    if effects.len() < isolated.len() {
        effects.resize(isolated.len(), String::new());
    }

    for (channel, handle) in isolated.iter().enumerate() {
        let pattern = handle.current_pattern();
        let row = handle.current_row();
        let label = handle.channel_sample_label(pattern, row, channel);
        if !label.is_empty() {
            labels[channel] = label;
        }
        effects[channel] = handle.channel_effect_label(pattern, row, channel);
    }
}

fn error_message_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe {
        openmpt_free_string(ptr);
    }
    Some(text)
}

fn allocated_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .trim()
        .to_owned();
    unsafe {
        openmpt_free_string(ptr);
    }
    if text.is_empty() { None } else { Some(text) }
}

fn format_label(slot: usize, name: String) -> String {
    let name = name.trim();
    if name.is_empty() {
        slot.to_string()
    } else {
        format!("{slot}: {name}")
    }
}

fn format_compact_effect(kind: String, value: String) -> String {
    let kind = kind.trim_matches(|ch: char| ch == '.' || ch.is_whitespace());
    let value = value.trim_matches(|ch: char| ch == '.' || ch.is_whitespace());
    match (kind.is_empty(), value.is_empty()) {
        (true, true) => String::new(),
        (false, true) => kind.to_owned(),
        (true, false) => value.to_owned(),
        (false, false) => format!("{kind}{value}"),
    }
}

fn load_supported_extensions() -> Vec<String> {
    let ptr = unsafe { openmpt_get_supported_extensions() };
    if ptr.is_null() {
        return fallback_supported_extensions();
    }

    let text = unsafe { CStr::from_ptr(ptr) }.to_string_lossy();
    let extensions = text
        .split(';')
        .map(str::trim)
        .filter(|extension| !extension.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();

    if extensions.is_empty() {
        fallback_supported_extensions()
    } else {
        extensions
    }
}

fn fallback_supported_extensions() -> Vec<String> {
    FALLBACK_SUPPORTED_EXTENSIONS
        .iter()
        .map(|extension| (*extension).to_owned())
        .collect()
}

const FALLBACK_SUPPORTED_EXTENSIONS: &[&str] = &[
    "669", "amf", "ams", "dbm", "digi", "dmf", "far", "gdm", "imf", "it", "med", "mod", "mt2",
    "mtm", "mptm", "okt", "psm", "s3m", "stm", "ult", "umx", "xm",
];
