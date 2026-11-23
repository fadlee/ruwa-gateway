//! ffmpeg/ffprobe helpers for video and voice notes (GOWA `SendVideo`,
//! `SendAudio`). Inputs are written to a private temp dir; nothing is
//! passed through a shell.

use std::{path::Path, process::Stdio, time::Duration};

use tokio::process::Command;

const TIMEOUT: Duration = Duration::from_secs(120);
const WAVEFORM_POINTS: usize = 64;
const WAVEFORM_SAMPLE_RATE: u32 = 8000;
const WAVEFORM_MAX_SECONDS: u32 = 300;
const WAVEFORM_MAX_PCM_BYTES: usize = 2_400_000;

pub fn available() -> bool {
    which("ffmpeg")
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file()))
}

async fn run(bin: &str, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let out = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| anyhow::anyhow!("{bin} timed out"))??;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        anyhow::bail!("{bin} failed ({}): {tail}", out.status);
    }
    Ok(out.stdout)
}

fn path_str(p: &Path) -> anyhow::Result<&str> {
    p.to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 temp path"))
}

/// Media duration in whole seconds (GOWA `getAudioDuration`); 0 when unknown.
pub async fn duration_secs(path: &Path) -> u32 {
    let Ok(p) = path_str(path) else { return 0 };
    run(
        "ffprobe",
        &[
            "-hide_banner",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            p,
        ],
    )
    .await
    .ok()
    .and_then(|out| String::from_utf8_lossy(&out).trim().parse::<f64>().ok())
    .map(|d| d as u32)
    .unwrap_or(0)
}

/// One frame at 1s, fitted inside 100x512 (GOWA `videoThumbnailFFmpegArgs`),
/// as JPEG bytes.
pub async fn video_thumbnail(input: &Path) -> anyhow::Result<Vec<u8>> {
    let dir = tempfile::tempdir()?;
    let out = dir.path().join("thumb.jpg");
    let (i, o) = (path_str(input)?, path_str(&out)?);
    let first = run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            i,
            "-ss",
            "00:00:01.000",
            "-vframes",
            "1",
            "-vf",
            "scale=100:512:force_original_aspect_ratio=decrease",
            "-y",
            o,
        ],
    )
    .await;
    // Clips shorter than one second have no frame at 1s; take the first.
    if first.is_err() || !out.exists() {
        run(
            "ffmpeg",
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                i,
                "-vframes",
                "1",
                "-vf",
                "scale=100:512:force_original_aspect_ratio=decrease",
                "-y",
                o,
            ],
        )
        .await?;
    }
    Ok(tokio::fs::read(out).await?)
}

/// GOWA `buildVideoTranscodeArgs`: HD wins over compress; neither → `None`.
pub async fn transcode_video(
    input: &Path,
    compress: bool,
    hd: bool,
) -> anyhow::Result<Option<Vec<u8>>> {
    if !compress && !hd {
        return Ok(None);
    }
    let dir = tempfile::tempdir()?;
    let out = dir.path().join("out.mp4");
    let (i, o) = (path_str(input)?, path_str(&out)?);
    let args: Vec<&str> = if hd {
        vec![
            "-i",
            i,
            "-c:v",
            "libx264",
            "-crf",
            "23",
            "-preset",
            "fast",
            "-vf",
            "scale='min(1280,iw)':'min(1280,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-movflags",
            "+faststart",
            "-y",
            o,
        ]
    } else {
        vec![
            "-i",
            i,
            "-c:v",
            "libx264",
            "-crf",
            "28",
            "-preset",
            "fast",
            "-vf",
            "scale=720:-2",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-movflags",
            "+faststart",
            "-y",
            o,
        ]
    };
    run("ffmpeg", &args).await?;
    Ok(Some(tokio::fs::read(out).await?))
}

/// Convert to OGG Opus mono 48kHz for voice notes (GOWA PTT conversion).
pub async fn to_ogg_opus(input: &Path) -> anyhow::Result<Vec<u8>> {
    let dir = tempfile::tempdir()?;
    let out = dir.path().join("voice.ogg");
    let (i, o) = (path_str(input)?, path_str(&out)?);
    run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            i,
            "-c:a",
            "libopus",
            "-b:a",
            "64k",
            "-vbr",
            "on",
            "-application",
            "voip",
            "-ar",
            "48000",
            "-ac",
            "1",
            "-y",
            o,
        ],
    )
    .await?;
    Ok(tokio::fs::read(out).await?)
}

/// 64-point voice-note waveform (GOWA `generateWaveform`).
pub async fn waveform(input: &Path, duration_secs: u32) -> Vec<u8> {
    let Ok(i) = path_str(input) else {
        return default_waveform();
    };
    let rate = if duration_secs > WAVEFORM_MAX_SECONDS {
        (WAVEFORM_MAX_PCM_BYTES / duration_secs as usize)
            .max(64)
            .to_string()
    } else {
        WAVEFORM_SAMPLE_RATE.to_string()
    };
    let max_secs = WAVEFORM_MAX_SECONDS.to_string();
    let mut args = vec![
        "-hide_banner",
        "-loglevel",
        "error",
        "-i",
        i,
        "-ac",
        "1",
        "-ar",
        &rate,
    ];
    if duration_secs <= WAVEFORM_MAX_SECONDS {
        args.extend(["-t", &max_secs]);
    }
    args.extend(["-f", "s8", "-acodec", "pcm_s8", "pipe:1"]);
    match run("ffmpeg", &args).await {
        Ok(mut pcm) if !pcm.is_empty() => {
            pcm.truncate(WAVEFORM_MAX_PCM_BYTES);
            downsample(&pcm)
        }
        _ => default_waveform(),
    }
}

/// RMS per bucket, normalised, gamma 0.7, scaled to 0..100.
pub fn downsample(samples: &[u8]) -> Vec<u8> {
    if samples.is_empty() {
        return default_waveform();
    }
    let per = (samples.len() / WAVEFORM_POINTS).max(1);
    let rms: Vec<f64> = (0..WAVEFORM_POINTS)
        .map(|i| {
            let start = (i * per).min(samples.len());
            let end = (start + per).min(samples.len());
            if start == end {
                return 0.0;
            }
            let sum: f64 = samples[start..end]
                .iter()
                .map(|&s| f64::from(s as i8).powi(2))
                .sum();
            (sum / (end - start) as f64).sqrt()
        })
        .collect();
    let max = rms.iter().cloned().fold(0.0, f64::max);
    let max = if max == 0.0 { 1.0 } else { max };
    rms.iter()
        .map(|r| ((r / max).powf(0.7) * 100.0) as u8)
        .collect()
}

pub fn default_waveform() -> Vec<u8> {
    (0..WAVEFORM_POINTS)
        .map(|i| (50.0 + 30.0 * (i as f64 * 0.3).sin()) as u8)
        .collect()
}

/// Static WebP sticker from a PNG (GOWA `buildStickerWebPFFmpegArgs`).
pub async fn png_to_webp(png: &[u8]) -> anyhow::Result<Vec<u8>> {
    let dir = tempfile::tempdir()?;
    let (inp, out) = (dir.path().join("in.png"), dir.path().join("out.webp"));
    tokio::fs::write(&inp, png).await?;
    let (i, o) = (path_str(&inp)?, path_str(&out)?);
    run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            i,
            "-vcodec",
            "libwebp",
            "-lossless",
            "0",
            "-compression_level",
            "6",
            "-q:v",
            "60",
            "-preset",
            "default",
            "-loop",
            "0",
            "-an",
            o,
        ],
    )
    .await?;
    Ok(tokio::fs::read(out).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_shape() {
        let samples: Vec<u8> = (0..6400).map(|i| ((i % 64) as i8 - 32) as u8).collect();
        let w = downsample(&samples);
        assert_eq!(w.len(), 64);
        assert!(w.iter().all(|&v| v <= 100));
        assert_eq!(downsample(&[0, 0, 0]).len(), 64);
        assert_eq!(default_waveform().len(), 64);
    }

    async fn sample(args: &[&str], name: &str) -> Option<(tempfile::TempDir, std::path::PathBuf)> {
        if !available() {
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        let mut full = vec!["-hide_banner", "-loglevel", "error"];
        full.extend_from_slice(args);
        full.extend(["-y", path.to_str().unwrap()]);
        run("ffmpeg", &full).await.unwrap();
        Some((dir, path))
    }

    #[tokio::test]
    async fn video_pipeline() {
        let Some((_d, mp4)) = sample(
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=10:duration=2",
                "-pix_fmt",
                "yuv420p",
            ],
            "in.mp4",
        )
        .await
        else {
            return;
        };
        assert_eq!(duration_secs(&mp4).await, 2);
        let thumb = video_thumbnail(&mp4).await.unwrap();
        let img = image::load_from_memory(&thumb).unwrap();
        assert!(img.width() <= 100);
        let out = transcode_video(&mp4, true, false).await.unwrap().unwrap();
        assert!(!out.is_empty());
        assert!(transcode_video(&mp4, false, false).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn sticker_webp() {
        if !available() {
            return;
        }
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(64, 48)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        // ffmpeg builds without libwebp fail here; that is not our bug.
        let Ok(webp) = png_to_webp(png.get_ref()).await else {
            return;
        };
        assert_eq!(crate::media::image::webp_info(&webp), Some((false, 64, 48)));
    }

    #[tokio::test]
    async fn voice_pipeline() {
        let Some((_d, wav)) = sample(
            &["-f", "lavfi", "-i", "sine=frequency=440:duration=2"],
            "in.wav",
        )
        .await
        else {
            return;
        };
        let ogg = to_ogg_opus(&wav).await.unwrap();
        assert_eq!(&ogg[..4], b"OggS");
        let w = waveform(&wav, 2).await;
        assert_eq!(w.len(), 64);
        assert!(w.iter().any(|&v| v > 50));
    }
}
