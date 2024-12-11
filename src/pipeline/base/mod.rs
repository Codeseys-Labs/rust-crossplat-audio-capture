use color_eyre::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Base configuration trait for pipeline components
pub trait ComponentConfig: Send + Sync {
    fn validate(&self) -> Result<()>;
}

/// Audio chunk containing samples and timing information
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub samples: Vec<f32>,
    pub timestamp: f64,
    pub sample_rate: u32,
}

impl AudioChunk {
    pub fn new(samples: Vec<f32>, timestamp: f64, sample_rate: u32) -> Self {
        Self {
            samples,
            timestamp,
            sample_rate,
        }
    }
}

/// Base trait for pipeline components
#[async_trait::async_trait]
pub trait PipelineComponent: Send + Sync {
    type Input: Send + Sync;
    type Output: Send + Sync;
    type Config: ComponentConfig;

    /// Initialize the component with given configuration
    async fn initialize(config: Self::Config) -> Result<Self>
    where
        Self: Sized;

    /// Process input data and produce output
    async fn process(&mut self, input: Self::Input) -> Result<Self::Output>;

    /// Reset the component's state
    async fn reset(&mut self) -> Result<()>;
}

/// Pipeline stage for connecting components
pub struct PipelineStage<C: PipelineComponent>
where
    C::Input: Send + Sync + 'static,
    C::Output: Send + Sync + 'static,
{
    component: C,
    input_rx: mpsc::Receiver<C::Input>,
    output_tx: mpsc::Sender<C::Output>,
}

impl<C: PipelineComponent> PipelineStage<C>
where
    C::Input: Send + Sync + 'static,
    C::Output: Send + Sync + 'static,
{
    pub fn new(
        component: C,
        input_rx: mpsc::Receiver<C::Input>,
        output_tx: mpsc::Sender<C::Output>,
    ) -> Self {
        Self {
            component,
            input_rx,
            output_tx,
        }
    }

    /// Run the pipeline stage
    pub async fn run(&mut self) -> Result<()> {
        while let Some(input) = self.input_rx.recv().await {
            let output = self.component.process(input).await?;
            self.output_tx.send(output).await?;
        }
        Ok(())
    }
}

/// Builder for constructing pipelines
#[derive(Default)]
pub struct PipelineBuilder {
    stages: Vec<Box<dyn std::any::Any + Send + Sync>>,
}

impl PipelineBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_stage<C: PipelineComponent + 'static>(mut self, config: C::Config) -> Result<Self>
    where
        C::Input: Send + Sync + 'static,
        C::Output: Send + Sync + 'static,
    {
        config.validate()?;
        self.stages.push(Box::new(config));
        Ok(self)
    }

    pub async fn build(self) -> Result<Pipeline> {
        Ok(Pipeline {
            stages: Arc::new(self.stages),
        })
    }
}

/// Main pipeline that coordinates all components
pub struct Pipeline {
    stages: Arc<Vec<Box<dyn std::any::Any + Send + Sync>>>,
}

impl Pipeline {
    pub fn builder() -> PipelineBuilder {
        PipelineBuilder::new()
    }

    /// Start processing with the pipeline
    pub async fn run(&self) -> Result<()> {
        // Implementation will coordinate all stages
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_chunk() {
        let chunk = AudioChunk::new(vec![0.0; 1024], 0.0, 16000);
        assert_eq!(chunk.samples.len(), 1024);
        assert_eq!(chunk.sample_rate, 16000);
    }
}
