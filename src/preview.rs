use std::sync::Arc;

use anyhow::{Context, Result};
use cpal::BufferSize;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::cli::{PreviewArgs, SortOrder};
use crate::discover;
use crate::openmpt::{ModuleHandle, ModuleSource, snapshot_isolated_channel_annotations};
use crate::oscilloscope::SampleHistory;
use crate::playlist::{PlaylistEntry, expand_sources};
use crate::render_host::VelloSurfaceRenderer;
use crate::visualizer::{FrameModule, FrameView};

pub fn run(args: PreviewArgs) -> Result<()> {
    let items = discover::discover(&args.input.inputs, args.input.sort, args.input.recursive)?;
    let sources = items
        .iter()
        .map(|item| ModuleSource::load(&item.path))
        .collect::<Result<Vec<_>>>()?;
    let playlist = expand_sources(sources)?;

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("no default audio output device was found")?;
    let supported = device
        .default_output_config()
        .context("failed to read default output config")?;
    let sample_rate = supported.sample_rate().0;
    let output_channels = supported.channels() as usize;
    let history_ms = args.history_ms.clamp(120, 500);
    let max_history_samples =
        ((sample_rate as u64 * history_ms as u64) / 1_000).max(1_024) as usize;

    let shared = Arc::new(Mutex::new(PreviewVisualState::empty(
        sample_rate,
        max_history_samples,
        args.show_song_info,
    )));
    let engine = Arc::new(Mutex::new(AudioEngine::new(
        playlist,
        sample_rate,
        output_channels,
        Arc::clone(&shared),
        max_history_samples,
    )?));

    let stream_config: cpal::StreamConfig = supported.clone().into();
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => {
            build_stream::<f32>(&device, &stream_config, Arc::clone(&engine))?
        }
        cpal::SampleFormat::I16 => {
            build_stream::<i16>(&device, &stream_config, Arc::clone(&engine))?
        }
        cpal::SampleFormat::U16 => {
            build_stream::<u16>(&device, &stream_config, Arc::clone(&engine))?
        }
        other => anyhow::bail!("unsupported preview sample format: {other:?}"),
    };
    stream.play().context("failed to start audio stream")?;

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = PreviewApp::new(
        shared,
        engine,
        stream,
        args.input.sort,
        args.input.recursive,
    );
    event_loop
        .run_app(&mut app)
        .context("preview event loop failed")
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    engine: Arc<Mutex<AudioEngine>>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let low_latency = cpal::StreamConfig {
        buffer_size: BufferSize::Fixed(256),
        ..config.clone()
    };

    try_build_stream::<T>(device, &low_latency, Arc::clone(&engine))
        .or_else(|_| try_build_stream::<T>(device, config, engine))
        .context("failed to build output stream")
}

fn try_build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    engine: Arc<Mutex<AudioEngine>>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels as usize;
    Ok(device.build_output_stream(
        config,
        {
            let mut scratch = Vec::<f32>::new();
            move |data: &mut [T], _| {
                scratch.clear();
                scratch.resize(data.len(), 0.0f32);
                engine.lock().render(&mut scratch, channels);
                for (dst, src) in data.iter_mut().zip(scratch.iter().copied()) {
                    *dst = T::from_sample(src);
                }
            }
        },
        move |error| eprintln!("audio stream error: {error}"),
        None,
    )?)
}

struct PreviewApp {
    shared: Arc<Mutex<PreviewVisualState>>,
    engine: Arc<Mutex<AudioEngine>>,
    _stream: cpal::Stream,
    sort: SortOrder,
    recursive: bool,
    window: Option<&'static Window>,
    renderer: Option<VelloSurfaceRenderer>,
    last_title_key: Option<(String, String)>,
}

impl PreviewApp {
    fn new(
        shared: Arc<Mutex<PreviewVisualState>>,
        engine: Arc<Mutex<AudioEngine>>,
        stream: cpal::Stream,
        sort: SortOrder,
        recursive: bool,
    ) -> Self {
        Self {
            shared,
            engine,
            _stream: stream,
            sort,
            recursive,
            window: None,
            renderer: None,
            last_title_key: None,
        }
    }
}

impl ApplicationHandler for PreviewApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let window = event_loop
            .create_window(
                WindowAttributes::default()
                    .with_title("trackervis")
                    .with_inner_size(PhysicalSize::new(1400, 900)),
            )
            .expect("failed to create window");
        let window = Box::leak(Box::new(window));
        let renderer = pollster::block_on(VelloSurfaceRenderer::new(
            window,
            vello::wgpu::PresentMode::AutoNoVsync,
        ))
        .expect("renderer init failed");

        self.window = Some(window);
        self.renderer = Some(renderer);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size);
                }
            }
            WindowEvent::DroppedFile(path) => {
                if let Err(error) =
                    self.engine
                        .lock()
                        .replace_inputs(&[path], self.sort, self.recursive)
                {
                    eprintln!("failed to load dropped path: {error:#}");
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if !event.state.is_pressed() {
                    return;
                }
                match event.logical_key {
                    Key::Named(NamedKey::Space) => self.engine.lock().toggle_pause(),
                    Key::Named(NamedKey::ArrowRight) => self.engine.lock().skip_forward(),
                    Key::Named(NamedKey::ArrowLeft) => self.engine.lock().skip_backward(),
                    Key::Character(ref text) if text.eq_ignore_ascii_case("n") => {
                        let mut state = self.shared.lock();
                        state.show_song_info = !state.show_song_info;
                    }
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                if let (Some(renderer), Some(window)) = (&mut self.renderer, self.window) {
                    let (
                        channel_samples,
                        channel_panning,
                        channel_labels,
                        channel_effects,
                        local_time_seconds,
                        sample_rate,
                        max_history_samples,
                        show_song_info,
                        song_info,
                        title_key,
                    ) = {
                        let state = self.shared.lock();
                        let title_key_changed = self.last_title_key.as_ref().map_or(true, |(label, filename)| {
                            label != &state.label || filename != &state.filename
                        });
                        let title_key = if title_key_changed {
                            Some((state.label.clone(), state.filename.clone()))
                        } else {
                            None
                        };
                        (
                            Arc::clone(&state.channel_samples),
                            state.channel_panning.as_ref().map(Arc::clone),
                            Arc::clone(&state.channel_labels),
                            Arc::clone(&state.channel_effects),
                            state.local_time_seconds,
                            state.sample_rate,
                            state.max_history_samples,
                            state.show_song_info,
                            Arc::clone(&state.song_info),
                            title_key,
                        )
                    };

                    if let Some((label, filename)) = title_key {
                        let title = if filename.is_empty() {
                            format!("trackervis - {}", label)
                        } else {
                            format!("trackervis - {} ({})", label, filename)
                        };
                        window.set_title(&title);
                        self.last_title_key = Some((label, filename));
                    }

                    let frame = FrameView {
                        width: renderer.size.width.max(1),
                        height: renderer.size.height.max(1),
                        max_history_samples,
                        module: FrameModule {
                            local_time_seconds,
                            sample_rate,
                            channels: channel_samples.as_ref(),
                            channel_panning: channel_panning.as_deref(),
                            channel_labels: Some(channel_labels.as_ref()),
                            channel_effects: Some(channel_effects.as_ref()),
                            song_info: show_song_info.then_some(song_info.as_ref()),
                        },
                    };
                    renderer.render(&frame).expect("preview render failed");
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window {
            window.request_redraw();
        }
    }
}

#[derive(Debug, Clone)]
struct PreviewVisualState {
    label: String,
    filename: String,
    song_info: Arc<str>,
    show_song_info: bool,
    local_time_seconds: f64,
    sample_rate: u32,
    max_history_samples: usize,
    channel_samples: Arc<[Vec<f32>]>,
    channel_panning: Option<Arc<[f32]>>,
    channel_labels: Arc<[String]>,
    channel_effects: Arc<[String]>,
}

impl PreviewVisualState {
    fn empty(sample_rate: u32, max_history_samples: usize, show_song_info: bool) -> Self {
        Self {
            label: "loading".to_owned(),
            filename: String::new(),
            song_info: Arc::from(""),
            show_song_info,
            local_time_seconds: 0.0,
            sample_rate,
            max_history_samples,
            channel_samples: Arc::from(vec![vec![0.0; 2]]),
            channel_panning: None,
            channel_labels: Arc::from(vec![String::new()]),
            channel_effects: Arc::from(vec![String::new()]),
        }
    }
}

struct PlaybackState {
    local_time_seconds: f64,
    master: ModuleHandle,
    isolated: Vec<ModuleHandle>,
    histories: Vec<SampleHistory>,
    channel_panning: Option<Vec<f32>>,
    channel_labels: Vec<String>,
    channel_effects: Vec<String>,
    master_scratch: Vec<f32>,
    isolated_scratch: Vec<Vec<f32>>,
}

struct AudioEngine {
    playlist: Vec<PlaylistEntry>,
    current_index: usize,
    current: Option<PlaybackState>,
    paused: bool,
    sample_rate: u32,
    output_channels: usize,
    shared: Arc<Mutex<PreviewVisualState>>,
    max_history_samples: usize,
    snapshot_interval_frames: usize,
    frames_since_snapshot: usize,
}

impl AudioEngine {
    fn new(
        playlist: Vec<PlaylistEntry>,
        sample_rate: u32,
        output_channels: usize,
        shared: Arc<Mutex<PreviewVisualState>>,
        max_history_samples: usize,
    ) -> Result<Self> {
        let mut engine = Self {
            playlist,
            current_index: 0,
            current: None,
            paused: false,
            sample_rate,
            output_channels,
            shared,
            max_history_samples,
            snapshot_interval_frames: 256,
            frames_since_snapshot: 0,
        };
        engine.load_current()?;
        Ok(engine)
    }

    fn render(&mut self, output: &mut [f32], channels: usize) {
        output.fill(0.0);
        if self.paused {
            return;
        }

        let frames = output.len() / channels;
        loop {
            let rendered = match &mut self.current {
                Some(current) => current.master.read_stereo(
                    self.sample_rate,
                    frames,
                    &mut current.master_scratch,
                ),
                None => 0,
            };

            if rendered == 0 {
                if self.playlist.is_empty() || !self.advance_to_next() {
                    break;
                }
                continue;
            }

            if let Some(current) = &mut self.current {
                for frame_idx in 0..rendered {
                    let left = current.master_scratch[frame_idx * 2];
                    let right = current.master_scratch[frame_idx * 2 + 1];
                    let base = frame_idx * channels;
                    output[base] = left;
                    if channels > 1 {
                        output[base + 1] = right;
                    }
                    for extra in 2..channels.min(self.output_channels) {
                        output[base + extra] = 0.0;
                    }
                }

                for (channel, handle) in current.isolated.iter_mut().enumerate() {
                    let scratch = &mut current.isolated_scratch[channel];
                    let isolated_frames = handle.read_stereo(self.sample_rate, rendered, scratch);
                    current.histories[channel]
                        .push_mono_stereo_frames(&scratch[..isolated_frames * 2]);
                    for _ in isolated_frames..rendered {
                        current.histories[channel].push(0.0);
                    }
                }

                current.channel_panning = current.master.channel_panning_snapshot();
                snapshot_isolated_channel_annotations(
                    &current.isolated,
                    &mut current.channel_labels,
                    &mut current.channel_effects,
                );
                current.local_time_seconds += rendered as f64 / self.sample_rate as f64;
                self.frames_since_snapshot += rendered;
                if self.frames_since_snapshot >= self.snapshot_interval_frames {
                    self.publish_state();
                    self.frames_since_snapshot = 0;
                } else {
                    self.publish_time_only();
                }
            }
            break;
        }
    }

    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
    }

    fn skip_forward(&mut self) {
        let _ = self.advance_to_next();
    }

    fn skip_backward(&mut self) {
        if self.playlist.is_empty() {
            return;
        }
        if self.current_index == 0 {
            self.current_index = self.playlist.len() - 1;
        } else {
            self.current_index -= 1;
        }
        let _ = self.load_current();
    }

    fn advance_to_next(&mut self) -> bool {
        if self.playlist.is_empty() {
            return false;
        }
        self.current_index = (self.current_index + 1) % self.playlist.len();
        self.load_current().is_ok()
    }

    fn replace_inputs(
        &mut self,
        inputs: &[std::path::PathBuf],
        sort: SortOrder,
        recursive: bool,
    ) -> Result<()> {
        let items = discover::discover(inputs, sort, recursive)?;
        let sources = items
            .iter()
            .map(|item| ModuleSource::load(&item.path))
            .collect::<Result<Vec<_>>>()?;
        self.playlist = expand_sources(sources)?;
        self.current_index = 0;
        self.current = None;
        self.paused = false;
        self.load_current()
    }

    fn load_current(&mut self) -> Result<()> {
        if self.playlist.is_empty() {
            anyhow::bail!("playlist is empty");
        }
        let entry = &self.playlist[self.current_index];
        let master = entry.source.open_subsong(entry.subsong_index)?;
        let channel_count = master.channel_count().max(1);
        let label = entry.label.clone();
        let filename = entry.filename.clone();
        let song_info = format_song_info(entry.playlist_index, entry.playlist_len, &label);
        let mut isolated = Vec::with_capacity(channel_count);
        for channel in 0..channel_count {
            let mut handle = entry.source.open_subsong(entry.subsong_index)?;
            handle.mute_all_except(channel)?;
            isolated.push(handle);
        }

        let histories = (0..channel_count)
            .map(|_| SampleHistory::new(self.max_history_samples))
            .collect::<Vec<_>>();
        let isolated_scratch = (0..channel_count).map(|_| Vec::new()).collect::<Vec<_>>();

        self.current = Some(PlaybackState {
            local_time_seconds: 0.0,
            master,
            isolated,
            histories,
            channel_panning: None,
            channel_labels: vec![String::new(); channel_count],
            channel_effects: vec![String::new(); channel_count],
            master_scratch: Vec::new(),
            isolated_scratch,
        });
        if let Some(current) = &mut self.current {
            current.channel_panning = current.master.channel_panning_snapshot();
            snapshot_isolated_channel_annotations(
                &current.isolated,
                &mut current.channel_labels,
                &mut current.channel_effects,
            );
        }
        {
            let mut shared = self.shared.lock();
            shared.label = label;
            shared.filename = filename;
            shared.song_info = Arc::from(song_info);
        }
        self.frames_since_snapshot = self.snapshot_interval_frames;
        self.publish_state();
        Ok(())
    }

    fn publish_time_only(&self) {
        let Some(current) = &self.current else {
            return;
        };
        let mut shared = self.shared.lock();
        shared.local_time_seconds = current.local_time_seconds;
    }

    fn publish_state(&self) {
        let Some(current) = &self.current else {
            return;
        };
        let mut shared = self.shared.lock();
        shared.local_time_seconds = current.local_time_seconds;
        shared.sample_rate = self.sample_rate;
        shared.max_history_samples = self.max_history_samples;
        shared.channel_samples = current
            .histories
            .iter()
            .map(SampleHistory::snapshot)
            .collect::<Vec<_>>()
            .into();
        shared.channel_panning = current.channel_panning.clone().map(Arc::from);
        shared.channel_labels = current.channel_labels.clone().into();
        shared.channel_effects = current.channel_effects.clone().into();
    }
}

fn format_song_info(index: usize, total: usize, label: &str) -> String {
    if total > 1 {
        format!("{}/{} {}", index + 1, total, label)
    } else {
        label.to_owned()
    }
}
