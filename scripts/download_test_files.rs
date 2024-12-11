use color_eyre::Result;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest;
use std::fs;
use std::path::Path;
use std::process::Command;

async fn download_file(url: &str, path: &Path, description: &str) -> Result<()> {
    if !path.exists() {
        println!("Downloading {}...", description);
        let response = reqwest::get(url).await?;
        let total_size = response.content_length().unwrap_or(0);

        let pb = ProgressBar::new(total_size);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")?
            .progress_chars("#>-"));

        let bytes = response.bytes().await?;
        pb.finish_with_message("Download complete");

        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(path, bytes)?;
        println!("Saved to {}", path.display());
    } else {
        println!("{} already exists at {}", description, path.display());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Download whisper model
    let whisper_url = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin";
    let whisper_path = Path::new("models/whisper-base.bin");
    download_file(whisper_url, whisper_path, "whisper-base.bin").await?;

    // Download sample podcast
    let podcast_url =
        "https://github.com/mozilla/DeepSpeech/raw/master/data/smoke_test/smoke_test.wav";
    let podcast_path = Path::new("podcast.wav");
    download_file(podcast_url, podcast_path, "sample podcast").await?;

    // Download and extract sherpa-onnx segmentation model
    let segmentation_url = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2";
    let segmentation_archive = Path::new("models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2");
    download_file(segmentation_url, segmentation_archive, "segmentation model").await?;

    // Extract the tar.bz2 file
    println!("\nExtracting segmentation model...");
    let models_dir = Path::new("models");
    if !models_dir.exists() {
        fs::create_dir_all(models_dir)?;
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("tar")
            .args(&[
                "xf",
                segmentation_archive.to_str().unwrap(),
                "-C",
                models_dir.to_str().unwrap(),
            ])
            .output()?;
    }

    #[cfg(not(target_os = "windows"))]
    {
        Command::new("tar")
            .args(&[
                "xf",
                segmentation_archive.to_str().unwrap(),
                "-C",
                models_dir.to_str().unwrap(),
            ])
            .output()?;
    }

    // Clean up the archive
    fs::remove_file(segmentation_archive)?;

    // Download sherpa-onnx speaker embedding model
    let embedding_url = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx";
    let embedding_path =
        Path::new("models/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx");
    download_file(embedding_url, embedding_path, "speaker embedding model").await?;

    println!("\nAll files ready!");
    Ok(())
}
