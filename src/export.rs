use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use hound::{SampleFormat, WavSpec, WavWriter};
use tempfile::tempdir;

use crate::chapters::format_chapter_lines;
use crate::cli::RenderArgs;
use crate::discover;
use crate::openmpt::{ModuleMetadata, ModuleSource};
use crate::visualizer::{FrameModule, FrameView, RasterImage, render_to_image};

#[derive(Debug, Clone)]
struct RenderedTrackInfo {
    duration_seconds: f64,
    channel_count: usize,
    song_info: String,
    frame_panning: Option<Vec<Vec<f32>>>,
    frame_labels: Vec<Vec<String>>,
    frame_effects: Vec<Vec<String>>,
}

pub fn run(args: RenderArgs) -> Result<()> {
    let items = discover::discover(&args.input.inputs, args.input.sort, args.input.recursive)?;
    let sources = items
        .iter()
        .map(|item| ModuleSource::load(&item.path))
        .collect::<Result<Vec<_>>>()?;

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

    let mut track_infos = Vec::new();
    let mut chapter_entries = Vec::new();
    let mut playlist_elapsed = Duration::ZERO;

    for source in &sources {
        let metadata = source.metadata()?;
        chapter_entries.push((playlist_elapsed, metadata.label.clone()));
        let track = render_audio_track(
            source,
            &metadata,
            args.sample_rate,
            args.fps,
            track_infos.len(),
            sources.len(),
            &mut wav_writer,
        )?;
        playlist_elapsed += Duration::from_secs_f64(track.duration_seconds);
        track_infos.push(track);
    }

    wav_writer.finalize().context("failed to finalize wav")?;

    if let Some(chapters_path) = &args.chapters {
        std::fs::write(chapters_path, format_chapter_lines(&chapter_entries))
            .with_context(|| format!("failed to write {}", chapters_path.display()))?;
    }

    encode_video(&args, &sources, &track_infos, &wav_path)?;
    Ok(())
}

fn render_audio_track(
    source: &ModuleSource,
    metadata: &ModuleMetadata,
    sample_rate: u32,
    fps: u32,
    playlist_index: usize,
    playlist_len: usize,
    wav_writer: &mut WavWriter<std::io::BufWriter<std::fs::File>>,
) -> Result<RenderedTrackInfo> {
    let mut master = source.open()?;
    let mut scratch = Vec::new();
    let mut total_frames = 0usize;
    let chunk_frames = 256usize;
    let mut next_frame_time = 0.0f64;
    let frame_step = 1.0 / fps.max(1) as f64;
    let mut frame_panning = master
        .channel_panning_snapshot()
        .map(|snapshot| vec![snapshot]);
    let mut running_labels =
        master.pattern_sample_labels(master.current_pattern(), master.current_row());
    let mut frame_labels = vec![running_labels.clone()];
    let mut frame_effects =
        vec![master.pattern_effect_labels(master.current_pattern(), master.current_row())];

    loop {
        let rendered = master.read_stereo(sample_rate, chunk_frames, &mut scratch);
        if rendered == 0 {
            break;
        }
        total_frames += rendered;
        for sample in &scratch {
            wav_writer
                .write_sample(*sample)
                .context("failed to write wav data")?;
        }

        let now = total_frames as f64 / sample_rate as f64;
        while next_frame_time + frame_step <= now {
            next_frame_time += frame_step;
            if let Some(snapshot) = master.channel_panning_snapshot()
                && let Some(frames) = &mut frame_panning
            {
                frames.push(snapshot);
            }
            let labels =
                master.pattern_sample_labels(master.current_pattern(), master.current_row());
            merge_channel_labels(&mut running_labels, labels);
            frame_labels.push(running_labels.clone());
            frame_effects
                .push(master.pattern_effect_labels(master.current_pattern(), master.current_row()));
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
        channel_count: metadata.channel_count.max(1),
        song_info: format_song_info(playlist_index, playlist_len, &metadata.label),
        frame_panning,
        frame_labels,
        frame_effects,
    })
}

fn render_channel_samples(
    source: &ModuleSource,
    sample_rate: u32,
    channel_count: usize,
) -> Result<Vec<Vec<f32>>> {
    let mut result = Vec::with_capacity(channel_count);
    for channel in 0..channel_count {
        let mut handle = source.open()?;
        handle.mute_all_except(channel)?;

        let mut scratch = Vec::new();
        let mut samples = Vec::new();
        loop {
            let rendered = handle.read_stereo(sample_rate, 2_048, &mut scratch);
            if rendered == 0 {
                break;
            }
            samples.reserve(rendered);
            for frame in scratch.chunks_exact(2) {
                samples.push((frame[0] + frame[1]) * 0.5);
            }
        }
        result.push(samples);
    }
    Ok(result)
}

fn encode_video(
    args: &RenderArgs,
    sources: &[ModuleSource],
    tracks: &[RenderedTrackInfo],
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
            .arg("p5")
            .arg("-tune")
            .arg("hq")
            .arg("-cq")
            .arg("18")
            .arg("-b:v")
            .arg("0");
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
    let mut frame_image = RasterImage::new(args.width, args.height);
    let max_history_samples = ((args.sample_rate as u64 * args.history_ms.clamp(120, 500) as u64)
        / 1_000)
        .max(1_024) as usize;

    for (source, track) in sources.iter().zip(tracks.iter()) {
        let channel_samples =
            render_channel_samples(source, args.sample_rate, track.channel_count)?;
        let frame_count = (track.duration_seconds * args.fps as f64).ceil() as usize;
        for frame_index in 0..frame_count {
            let local_time = frame_index as f64 / args.fps as f64;
            let frame = FrameView {
                width: args.width,
                height: args.height,
                max_history_samples,
                module: FrameModule {
                    local_time_seconds: local_time,
                    sample_rate: args.sample_rate,
                    channels: &channel_samples,
                    channel_panning: track
                        .frame_panning
                        .as_ref()
                        .and_then(|frames| frames.get(frame_index))
                        .map(Vec::as_slice),
                    channel_labels: track.frame_labels.get(frame_index).map(Vec::as_slice),
                    channel_effects: track.frame_effects.get(frame_index).map(Vec::as_slice),
                    song_info: args.show_song_info.then_some(track.song_info.as_str()),
                },
            };
            render_to_image(&mut frame_image, &frame);
            stdin
                .write_all(frame_image.as_raw())
                .context("failed to stream raw video to ffmpeg")?;
        }
    }
    drop(stdin);

    let status = ffmpeg.wait().context("failed waiting for ffmpeg")?;
    if !status.success() {
        bail!("ffmpeg exited with status {status}");
    }
    Ok(())
}

fn merge_channel_labels(slots: &mut Vec<String>, updates: Vec<String>) {
    if slots.len() < updates.len() {
        slots.resize(updates.len(), String::new());
    }
    for (index, update) in updates.into_iter().enumerate() {
        if !update.is_empty() {
            slots[index] = update;
        }
    }
}

fn format_song_info(index: usize, total: usize, label: &str) -> String {
    if total > 1 {
        format!("{}/{} {}", index + 1, total, label)
    } else {
        label.to_owned()
    }
}
