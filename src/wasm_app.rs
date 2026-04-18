use anyhow::{Result, anyhow};
use rfd::AsyncFileDialog;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use web_sys::{AudioBufferSourceNode, AudioContext};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::platform::web::{EventLoopExtWebSys, WindowAttributesExtWebSys, WindowExtWebSys};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::render_host::VelloSurfaceRenderer;
use crate::visualizer::{FrameModule, FrameView};
use crate::web_openmpt::{DecodedTrack, decode_module_stream};

const SAMPLE_RATE: u32 = 48_000;
const MAX_HISTORY_SAMPLES: usize = SAMPLE_RATE as usize * 3;

pub fn start() -> Result<()> {
    let event_loop = EventLoop::<AppUserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let proxy = event_loop.create_proxy();
    event_loop.spawn_app(WasmApp::new(proxy));
    Ok(())
}

enum AppUserEvent {
    RendererReady(Box<VelloSurfaceRenderer>),
    TrackDecoded(DecodedTrack),
    LoadFailed(String),
    DialogFinished,
}

struct PlaybackState {
    source: AudioBufferSourceNode,
    started_at: f64,
}

struct WasmApp {
    proxy: EventLoopProxy<AppUserEvent>,
    window: Option<&'static Window>,
    renderer: Option<VelloSurfaceRenderer>,
    playlist: Vec<DecodedTrack>,
    current_index: usize,
    audio_context: Option<AudioContext>,
    playback: Option<PlaybackState>,
    dialog_open: bool,
    loading_active: bool,
    loading_message: Option<String>,
    waiting_for_next_track: bool,
    show_song_info: bool,
    frozen_time_seconds: f64,
}

impl WasmApp {
    fn new(proxy: EventLoopProxy<AppUserEvent>) -> Self {
        Self {
            proxy,
            window: None,
            renderer: None,
            playlist: Vec::new(),
            current_index: 0,
            audio_context: None,
            playback: None,
            dialog_open: false,
            loading_active: false,
            loading_message: None,
            waiting_for_next_track: false,
            show_song_info: true,
            frozen_time_seconds: 0.0,
        }
    }

    fn open_file_dialog(&mut self) {
        if self.dialog_open {
            return;
        }
        self.dialog_open = true;
        self.loading_active = true;
        self.loading_message = Some("loading modules...".to_owned());
        self.waiting_for_next_track = false;
        self.stop_playback();
        self.playlist.clear();
        self.current_index = 0;
        self.frozen_time_seconds = 0.0;
        if let Ok(context) = self.ensure_audio_context() {
            let _ = context.resume();
        }
        let proxy = self.proxy.clone();
        spawn_local(async move {
            let handles = AsyncFileDialog::new()
                .set_title("Select Tracker Modules")
                .pick_files()
                .await;

            if let Some(handles) = handles {
                for handle in handles {
                    let filename = handle.file_name();
                    let bytes = handle.read().await;
                    let track_proxy = proxy.clone();
                    if let Err(error) =
                        decode_module_stream(bytes, filename, SAMPLE_RATE, move |track| {
                            let _ = track_proxy.send_event(AppUserEvent::TrackDecoded(track));
                        })
                        .await
                    {
                        let _ = proxy.send_event(AppUserEvent::LoadFailed(format!("{error:#}")));
                    }
                }
            }

            let _ = proxy.send_event(AppUserEvent::DialogFinished);
        });
    }

    fn ensure_audio_context(&mut self) -> Result<&AudioContext> {
        if self.audio_context.is_none() {
            self.audio_context = Some(AudioContext::new().map_err(js_error)?);
        }
        self.audio_context
            .as_ref()
            .ok_or_else(|| anyhow!("audio context is unavailable"))
    }

    #[allow(deprecated)]
    fn stop_playback(&mut self) {
        if let Some(playback) = self.playback.take() {
            let _ = playback.source.stop();
        }
        self.waiting_for_next_track = false;
    }

    fn play_current_track(&mut self) -> Result<()> {
        self.stop_playback();
        let track = match self.playlist.get(self.current_index).cloned() {
            Some(track) => track,
            None => return Ok(()),
        };

        let context = self.ensure_audio_context()?;
        let buffer = context
            .create_buffer(
                2,
                track.audio_left.len() as u32,
                track.audio_sample_rate as f32,
            )
            .map_err(js_error)?;
        buffer
            .copy_to_channel(&track.audio_left, 0)
            .map_err(js_error)?;
        buffer
            .copy_to_channel(&track.audio_right, 1)
            .map_err(js_error)?;

        let source = context.create_buffer_source().map_err(js_error)?;
        source.set_buffer(Some(&buffer));
        let source_node: &web_sys::AudioNode = source.unchecked_ref();
        let destination = context.destination();
        let destination_node: &web_sys::AudioNode = destination.unchecked_ref();
        source_node
            .connect_with_audio_node(destination_node)
            .map_err(js_error)?;
        let _ = context.resume();
        let started_at = context.current_time();
        source.start().map_err(js_error)?;
        self.playback = Some(PlaybackState { source, started_at });
        self.frozen_time_seconds = 0.0;
        Ok(())
    }

    fn append_track(&mut self, track: DecodedTrack) -> Result<()> {
        let was_empty = self.playlist.is_empty();
        self.playlist.push(track);
        if was_empty {
            self.current_index = 0;
            self.waiting_for_next_track = false;
            self.play_current_track()
        } else if self.waiting_for_next_track && self.current_index + 1 < self.playlist.len() {
            self.waiting_for_next_track = false;
            self.advance_to_next()
        } else {
            Ok(())
        }
    }

    fn advance_to_next(&mut self) -> Result<()> {
        if self.playlist.is_empty() {
            return Ok(());
        }
        self.current_index = (self.current_index + 1) % self.playlist.len();
        self.play_current_track()
    }

    fn advance_to_previous(&mut self) -> Result<()> {
        if self.playlist.is_empty() {
            return Ok(());
        }
        if self.current_index == 0 {
            self.current_index = self.playlist.len() - 1;
        } else {
            self.current_index -= 1;
        }
        self.play_current_track()
    }

    fn toggle_pause(&mut self) -> Result<()> {
        let Some(context) = &self.audio_context else {
            return Ok(());
        };
        match context.state() {
            web_sys::AudioContextState::Running => {
                let _ = context.suspend();
            }
            web_sys::AudioContextState::Suspended => {
                let _ = context.resume();
            }
            _ => {}
        }
        Ok(())
    }

    fn current_track(&self) -> Option<&DecodedTrack> {
        self.playlist.get(self.current_index)
    }

    fn current_time_seconds(&mut self) -> Result<f64> {
        let Some(track_duration) = self.current_track().map(|track| track.duration_seconds) else {
            return Ok(0.0);
        };

        let Some(playback) = &self.playback else {
            return Ok(self.frozen_time_seconds.min(track_duration));
        };
        let Some(context) = &self.audio_context else {
            return Ok(0.0);
        };
        let elapsed = (context.current_time() - playback.started_at).max(0.0);
        if elapsed >= track_duration {
            self.frozen_time_seconds = track_duration;
            if self.current_index + 1 < self.playlist.len() {
                self.advance_to_next()?;
                return self.current_time_seconds();
            }
            if self.loading_active {
                self.waiting_for_next_track = true;
                return Ok(self.frozen_time_seconds);
            }
            self.waiting_for_next_track = false;
            self.playback = None;
            return Ok(self.frozen_time_seconds);
        }
        Ok(elapsed)
    }

    fn placeholder_channels(&self) -> [Vec<f32>; 1] {
        [vec![0.0; 2]]
    }

    fn render_frame(&mut self) -> Result<()> {
        let local_time_seconds = self.current_time_seconds()?;
        let placeholder = self.placeholder_channels();
        let current_index = self.current_index;
        let show_song_info = self.show_song_info;
        let (channels, song_info, title) = if let Some(track) = self.playlist.get(current_index) {
            let title = format!("trackervis - {} ({})", track.label, track.filename);
            let info = show_song_info.then_some(track.label.as_str());
            (&track.channel_samples[..], info, title)
        } else if self.loading_active {
            let status = self
                .loading_message
                .as_deref()
                .unwrap_or("loading modules...");
            (
                &placeholder[..],
                Some(status),
                format!("trackervis - {status}"),
            )
        } else {
            (
                &placeholder[..],
                Some("click anywhere to open modules"),
                "trackervis".to_owned(),
            )
        };

        if let Some(window) = self.window {
            window.set_title(&title);
        }

        let Some(renderer) = &mut self.renderer else {
            return Ok(());
        };
        let frame = FrameView {
            width: renderer.size.width.max(1),
            height: renderer.size.height.max(1),
            max_history_samples: MAX_HISTORY_SAMPLES,
            module: FrameModule {
                local_time_seconds,
                sample_rate: self
                    .playlist
                    .get(current_index)
                    .map(|track| track.scope_sample_rate)
                    .unwrap_or(SAMPLE_RATE),
                channels,
                channel_panning: None,
                channel_labels: None,
                channel_effects: None,
                song_info,
            },
        };
        renderer.render(&frame)
    }
}

impl ApplicationHandler<AppUserEvent> for WasmApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let window = event_loop
            .create_window(
                WindowAttributes::default()
                    .with_title("trackervis")
                    .with_inner_size(PhysicalSize::new(1400, 900))
                    .with_append(true),
            )
            .expect("failed to create browser window");
        let window: &'static Window = Box::leak(Box::new(window));
        if let Some(canvas) = window.canvas() {
            let _ = canvas.set_attribute(
                "style",
                "display:block;width:100vw;height:100vh;outline:none;background:#000;",
            );
        }

        let proxy = self.proxy.clone();
        spawn_local(async move {
            let result = VelloSurfaceRenderer::new(window, vello::wgpu::PresentMode::AutoVsync)
                .await
                .map_err(|error| format!("{error:#}"));
            match result {
                Ok(renderer) => {
                    let _ = proxy.send_event(AppUserEvent::RendererReady(Box::new(renderer)));
                }
                Err(error) => {
                    let _ = proxy.send_event(AppUserEvent::LoadFailed(error));
                }
            }
        });

        self.window = Some(window);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppUserEvent) {
        match event {
            AppUserEvent::RendererReady(renderer) => {
                self.renderer = Some(*renderer);
            }
            AppUserEvent::TrackDecoded(track) => {
                if let Err(error) = self.append_track(track) {
                    web_sys::console::error_1(&JsValue::from_str(&format!("{error:#}")));
                }
            }
            AppUserEvent::LoadFailed(error) => {
                web_sys::console::error_1(&JsValue::from_str(&error));
            }
            AppUserEvent::DialogFinished => {
                self.dialog_open = false;
                self.loading_active = false;
                self.loading_message = None;
                self.waiting_for_next_track = false;
            }
        }
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
            WindowEvent::MouseInput { state, button, .. }
                if state == ElementState::Pressed && button == MouseButton::Left =>
            {
                self.open_file_dialog();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if !event.state.is_pressed() {
                    return;
                }
                match event.logical_key {
                    Key::Named(NamedKey::ArrowRight) => {
                        if let Err(error) = self.advance_to_next() {
                            web_sys::console::error_1(&JsValue::from_str(&format!("{error:#}")));
                        }
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        if let Err(error) = self.advance_to_previous() {
                            web_sys::console::error_1(&JsValue::from_str(&format!("{error:#}")));
                        }
                    }
                    Key::Named(NamedKey::Space) => {
                        let _ = self.toggle_pause();
                    }
                    Key::Character(ref text) if text.eq_ignore_ascii_case("n") => {
                        self.show_song_info = !self.show_song_info;
                    }
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.render_frame() {
                    web_sys::console::error_1(&JsValue::from_str(&format!("{error:#}")));
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

fn js_error(error: wasm_bindgen::JsValue) -> anyhow::Error {
    if let Some(text) = error.as_string() {
        anyhow!(text)
    } else {
        anyhow!(format!("{error:?}"))
    }
}
