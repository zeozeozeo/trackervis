use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use hound::{SampleFormat, WavSpec, WavWriter};
use parking_lot::Mutex;
use rayon::prelude::*;
use tempfile::tempdir;

use crate::chapters::format_chapter_lines;
use crate::cli::RenderArgs;
use crate::discover;
use crate::openmpt::{ModuleSource, snapshot_isolated_channel_annotations};
use crate::playlist::{PlaylistEntry, expand_sources};
use crate::render_host::VelloImageRenderer;
use crate::visualizer::{FrameModule, FrameView};

const NVENC_VIDEO_BITRATE: &str = "10M";
const FRAME_QUEUE_DEPTH: usize = 16;

#[derive(Debug, Clone)]
struct RenderedTrackInfo {
    duration_seconds: f64,
    channel_samples: Vec<Vec<f32>>,
    song_info: String,
    frame_panning: Option<Vec<Vec<f32>>>,
    frame_labels: Vec<Vec<String>>,
    frame_effects: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Copy)]
struct VideoRenderOptions {
    width: u32,
    height: u32,
    fps: u32,
    sample_rate: u32,
    history_ms: u32,
    show_song_info: bool,
}

enum VideoMessage {
    Frame(usize),
    Error(String),
}

pub fn run(args: RenderArgs) -> Result<()> {
    let items = discover::discover(&args.input.inputs, args.input.sort, args.input.recursive)?;
    let sources = items
        .iter()
        .map(|item| ModuleSource::load(&item.path))
        .collect::<Result<Vec<_>>>()?;
    let playlist = expand_sources(sources)?;

    let temp = tempdir().context("failed to create temp dir")?;
    let wav_path = temp.path().join("playlist.wav");
    let mut wav_writer = WavWriter::create(
        &wav_path,
        WavSpec {
            channels: 2,
            sample_rate: args.sample_rate,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        },
    )
    .context("failed to create temp wav")?;

    let mut track_infos = Vec::with_capacity(playlist.len());
    let mut chapter_entries = Vec::with_capacity(playlist.len());
    let mut playlist_elapsed = Duration::ZERO;

    for entry in &playlist {
        chapter_entries.push((playlist_elapsed, entry.label.clone()));
        let track = render_audio_track(entry, args.sample_rate, args.fps, &mut wav_writer)?;
        playlist_elapsed += Duration::from_secs_f64(track.duration_seconds);
        track_infos.push(track);
    }

    wav_writer.finalize().context("failed to finalize wav")?;

    if let Some(chapters_path) = &args.chapters {
        std::fs::write(chapters_path, format_chapter_lines(&chapter_entries))
            .with_context(|| format!("failed to write {}", chapters_path.display()))?;
    }

    encode_video(&args, playlist, track_infos, &wav_path)?;
    Ok(())
}

fn render_audio_track(
    entry: &PlaylistEntry,
    sample_rate: u32,
    fps: u32,
    wav_writer: &mut WavWriter<std::io::BufWriter<std::fs::File>>,
) -> Result<RenderedTrackInfo> {
    let mut master = entry.source.open_subsong(entry.subsong_index)?;
    let channel_count = master.channel_count().max(1);
    let mut isolated = Vec::with_capacity(channel_count);
    for channel in 0..channel_count {
        let mut handle = entry.source.open_subsong(entry.subsong_index)?;
        handle.mute_all_except(channel)?;
        isolated.push(handle);
    }

    let mut master_scratch = Vec::new();
    let mut isolated_scratch = (0..channel_count).map(|_| Vec::new()).collect::<Vec<_>>();
    let mut channel_samples = vec![Vec::new(); channel_count];
    let mut total_frames = 0usize;
    let chunk_frames = 1_024usize;
    let mut next_frame_time = 0.0f64;
    let frame_step = 1.0 / fps.max(1) as f64;
    let mut frame_panning = master
        .channel_panning_snapshot()
        .map(|snapshot| vec![snapshot]);
    let mut running_labels = vec![String::new(); channel_count];
    let mut running_effects = vec![String::new(); channel_count];
    snapshot_isolated_channel_annotations(&isolated, &mut running_labels, &mut running_effects);
    let mut frame_labels = vec![running_labels.clone()];
    let mut frame_effects = vec![running_effects.clone()];

    loop {
        let rendered = master.read_stereo(sample_rate, chunk_frames, &mut master_scratch);
        if rendered == 0 {
            break;
        }
        total_frames += rendered;
        for sample in &master_scratch {
            wav_writer
                .write_sample(*sample)
                .context("failed to write wav data")?;
        }

        channel_samples
            .par_iter_mut()
            .zip(isolated.par_iter_mut())
            .zip(isolated_scratch.par_iter_mut())
            .for_each(|((samples, handle), scratch)| {
                let isolated_frames = handle.read_stereo(sample_rate, rendered, scratch);
                samples.reserve(rendered);
                for frame in scratch[..isolated_frames * 2].chunks_exact(2) {
                    samples.push((frame[0] + frame[1]) * 0.5);
                }
                for _ in isolated_frames..rendered {
                    samples.push(0.0);
                }
            });

        let now = total_frames as f64 / sample_rate as f64;
        while next_frame_time + frame_step <= now {
            next_frame_time += frame_step;
            if let Some(snapshot) = master.channel_panning_snapshot()
                && let Some(frames) = &mut frame_panning
            {
                frames.push(snapshot);
            }
            snapshot_isolated_channel_annotations(
                &isolated,
                &mut running_labels,
                &mut running_effects,
            );
            frame_labels.push(running_labels.clone());
            frame_effects.push(running_effects.clone());
        }
    }

    let frame_count = ((total_frames as f64 / sample_rate as f64) * fps as f64).ceil() as usize;
    if let Some(frames) = &mut frame_panning
        && let Some(last) = frames.last().cloned()
    {
        while frames.len() < frame_count.max(1) {
            frames.push(last.clone());
        }
    }
    if let Some(last) = frame_labels.last().cloned() {
        while frame_labels.len() < frame_count.max(1) {
            frame_labels.push(last.clone());
        }
    }
    if let Some(last) = frame_effects.last().cloned() {
        while frame_effects.len() < frame_count.max(1) {
            frame_effects.push(last.clone());
        }
    }

    Ok(RenderedTrackInfo {
        duration_seconds: total_frames as f64 / sample_rate as f64,
        channel_samples,
        song_info: format_song_info(entry.playlist_index, entry.playlist_len, &entry.label),
        frame_panning,
        frame_labels,
        frame_effects,
    })
}

fn encode_video(
    args: &RenderArgs,
    playlist: Vec<PlaylistEntry>,
    tracks: Vec<RenderedTrackInfo>,
    wav_path: &PathBuf,
) -> Result<()> {
    if tracks.is_empty() {
        bail!("no tracks were rendered");
    }

    let mut ffmpeg = Command::new("ffmpeg");
    ffmpeg
        .arg("-y")
        .arg("-f")
        .arg("rawvideo")
        .arg("-pixel_format")
        .arg("rgba")
        .arg("-video_size")
        .arg(format!("{}x{}", args.width, args.height))
        .arg("-framerate")
        .arg(args.fps.to_string())
        .arg("-i")
        .arg("-")
        .arg("-i")
        .arg(wav_path);

    if args.nvenc {
        ffmpeg
            .arg("-c:v")
            .arg("h264_nvenc")
            .arg("-preset")
            .arg("p4")
            .arg("-rc")
            .arg("cbr")
            .arg("-b:v")
            .arg(NVENC_VIDEO_BITRATE)
            .arg("-maxrate")
            .arg(NVENC_VIDEO_BITRATE)
            .arg("-bufsize")
            .arg("4M")
            .arg("-bf")
            .arg("0")
            .arg("-g")
            .arg((args.fps.max(1) * 2).to_string());
    } else {
        ffmpeg
            .arg("-c:v")
            .arg("libx264")
            .arg("-preset")
            .arg("slow")
            .arg("-crf")
            .arg("8");
    }

    let mut ffmpeg = ffmpeg
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("320k")
        .arg("-movflags")
        .arg("+faststart")
        .arg(&args.output)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to spawn ffmpeg")?;

    let mut stdin = ffmpeg.stdin.take().context("failed to open ffmpeg stdin")?;
    let render_options = VideoRenderOptions {
        width: args.width,
        height: args.height,
        fps: args.fps,
        sample_rate: args.sample_rate,
        history_ms: args.history_ms,
        show_song_info: args.show_song_info,
    };
    let (tx, rx) = mpsc::sync_channel(FRAME_QUEUE_DEPTH);
    let buffer_size = args.width.max(1) as usize * args.height.max(1) as usize * 4;
    let frame_buffers = Arc::new(
        (0..FRAME_QUEUE_DEPTH)
            .map(|_| Mutex::new(vec![0u8; buffer_size]))
            .collect::<Vec<_>>(),
    );
    let (free_tx, free_rx) = mpsc::sync_channel(FRAME_QUEUE_DEPTH);
    for index in 0..FRAME_QUEUE_DEPTH {
        free_tx
            .send(index)
            .expect("failed to seed frame buffer pool");
    }
    let render_buffers = Arc::clone(&frame_buffers);
    let render_handle =
        thread::spawn(move || render_video_frames(render_options, playlist, tracks, tx, free_rx, render_buffers));

    let mut pipeline_error: Option<anyhow::Error> = None;
    while let Ok(message) = rx.recv() {
        match message {
            VideoMessage::Frame(index) => {
                let buffer = frame_buffers[index].lock();
                if let Err(error) = stdin.write_all(buffer.as_slice()).with_context(|| {
                    format!(
                        "failed to stream raw video to ffmpeg for {}",
                        args.output.display()
                    )
                }) {
                    pipeline_error = Some(error);
                    break;
                }
                drop(buffer);
                if free_tx.send(index).is_err() {
                    pipeline_error = Some(anyhow!("frame buffer pool closed unexpectedly"));
                    break;
                }
            }
            VideoMessage::Error(error) => {
                pipeline_error = Some(anyhow::anyhow!(error));
                break;
            }
        }
    }

    if pipeline_error.is_some() {
        drop(free_tx);
        let _ = ffmpeg.kill();
    }
    drop(stdin);
    drop(rx);

    if let Some(error) = pipeline_error {
        let _ = render_handle.join();
        let _ = ffmpeg.wait();
        return Err(error);
    }

    render_handle
        .join()
        .map_err(|_| anyhow!("video render thread panicked"))??;

    let status = ffmpeg.wait().context("failed waiting for ffmpeg")?;
    if !status.success() {
        bail!("ffmpeg exited with status {status}");
    }
    Ok(())
}

fn render_video_frames(
    options: VideoRenderOptions,
    playlist: Vec<PlaylistEntry>,
    tracks: Vec<RenderedTrackInfo>,
    tx: SyncSender<VideoMessage>,
    free_rx: mpsc::Receiver<usize>,
    frame_buffers: Arc<Vec<Mutex<Vec<u8>>>>,
) -> Result<()> {
    let result = (|| -> Result<()> {
        let mut frame_renderer =
            pollster::block_on(VelloImageRenderer::new(options.width, options.height))
                .context("failed to initialize export renderer")?;
        let max_history_samples =
            ((options.sample_rate as u64 * options.history_ms.clamp(120, 500) as u64) / 1_000)
                .max(1_024) as usize;

        for (entry, track) in playlist.iter().zip(tracks.iter()) {
            let frame_count = (track.duration_seconds * options.fps as f64).ceil() as usize;
            for frame_index in 0..frame_count {
                let local_time = frame_index as f64 / options.fps as f64;
                let frame = FrameView {
                    width: options.width,
                    height: options.height,
                    max_history_samples,
                    module: FrameModule {
                        local_time_seconds: local_time,
                        sample_rate: options.sample_rate,
                        channels: &track.channel_samples,
                        channel_panning: track
                            .frame_panning
                            .as_ref()
                            .and_then(|frames| frames.get(frame_index))
                            .map(Vec::as_slice),
                        channel_labels: track.frame_labels.get(frame_index).map(Vec::as_slice),
                        channel_effects: track.frame_effects.get(frame_index).map(Vec::as_slice),
                        song_info: options.show_song_info.then_some(track.song_info.as_str()),
                    },
                };
                let index = free_rx
                    .recv()
                    .map_err(|_| anyhow!("frame buffer pool closed unexpectedly"))?;
                let mut buffer = frame_buffers[index].lock();
                frame_renderer
                    .render_into(&frame, buffer.as_mut_slice())
                    .with_context(|| {
                    format!(
                        "failed to render video frame for {}",
                        entry.source.path.display()
                    )
                })?;
                drop(buffer);
                tx.send(VideoMessage::Frame(index))
                    .map_err(|_| anyhow!("video frame queue closed unexpectedly"))?;
            }
        }

        Ok(())
    })();

    if let Err(ref error) = result {
        let _ = tx.send(VideoMessage::Error(format!("{error:#}")));
    }

    result
}

fn format_song_info(index: usize, total: usize, label: &str) -> String {
    if total > 1 {
        format!("{}/{} {}", index + 1, total, label)
    } else {
        label.to_owned()
    }
}
