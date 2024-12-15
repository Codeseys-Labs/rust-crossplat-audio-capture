use tokio::test;
use audio_capture::{AsyncAudioCapture, AudioCaptureConfig, AudioError};
use std::time::Duration;

mod common;
use common::{create_test_signal, verify_audio_similarity};

struct MockAsyncCapture {
    config: AudioCaptureConfig,
    is_capturing: bool,
    test_data: Vec<f32>,
}

impl MockAsyncCapture {
    fn new(config: AudioCaptureConfig) -> Self {
        Self {
            config,
            is_capturing: false,
            test_data: create_test_signal(440.0, 1000, config.sample_rate()),
        }
    }
}

#[async_trait::async_trait]
impl AsyncAudioCapture for MockAsyncCapture {
    async fn start(&mut self) -> Result<(), AudioError> {
        if self.is_capturing {
            return Err(AudioError::CaptureError("Already capturing".into()));
        }
        self.is_capturing = true;
        Ok(())
    }
    
    async fn stop(&mut self) -> Result<(), AudioError> {
        if !self.is_capturing {
            return Err(AudioError::CaptureError("Not capturing".into()));
        }
        self.is_capturing = false;
        Ok(())
    }
    
    async fn capture_stream(&mut self) -> Result<tokio::sync::mpsc::Receiver<Vec<f32>>, AudioError> {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let test_data = self.test_data.clone();
        
        tokio::spawn(async move {
            let chunk_size = 1024;
            for chunk in test_data.chunks(chunk_size) {
                if tx.send(chunk.to_vec()).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        
        Ok(rx)
    }
}

#[test]
async fn test_async_capture_lifecycle() {
    let config = AudioCaptureConfig::new();
    let mut capture = MockAsyncCapture::new(config);
    
    capture.start().await.expect("Failed to start capture");
    assert!(capture.is_capturing);
    
    capture.stop().await.expect("Failed to stop capture");
    assert!(!capture.is_capturing);
}

#[test]
async fn test_async_capture_stream() {
    let config = AudioCaptureConfig::new();
    let mut capture = MockAsyncCapture::new(config.clone());
    
    capture.start().await.expect("Failed to start capture");
    
    let mut stream = capture.capture_stream().await.expect("Failed to get stream");
    let mut received_data = Vec::new();
    
    while let Some(chunk) = stream.recv().await {
        received_data.extend(chunk);
        if received_data.len() >= config.sample_rate() as usize {
            break;
        }
    }
    
    let expected_data = create_test_signal(440.0, 1000, config.sample_rate());
    assert!(verify_audio_similarity(&received_data[..expected_data.len()], &expected_data, 0.001));
    
    capture.stop().await.expect("Failed to stop capture");
}

#[test]
async fn test_async_capture_error_handling() {
    let config = AudioCaptureConfig::new();
    let mut capture = MockAsyncCapture::new(config);
    
    // Try to stop before starting
    let result = capture.stop().await;
    assert!(matches!(result, Err(AudioError::CaptureError(_))));
    
    // Try to start twice
    capture.start().await.expect("Failed to start capture");
    let result = capture.start().await;
    assert!(matches!(result, Err(AudioError::CaptureError(_))));
}

#[test]
async fn test_async_stream_cancellation() {
    let config = AudioCaptureConfig::new();
    let mut capture = MockAsyncCapture::new(config);
    
    capture.start().await.expect("Failed to start capture");
    let mut stream = capture.capture_stream().await.expect("Failed to get stream");
    
    // Create a task that receives from the stream
    let receive_task = tokio::spawn(async move {
        let mut count = 0;
        while let Some(_) = stream.recv().await {
            count += 1;
            if count >= 10 {
                break;
            }
        }
        count
    });
    
    // Wait for some data to be received
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    // Cancel the task
    receive_task.abort();
    
    // Ensure the capture can still be stopped
    capture.stop().await.expect("Failed to stop capture");
}