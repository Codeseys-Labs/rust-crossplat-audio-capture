use crate::core::config::AudioFormat;
use crate::core::error::AudioResult;
use crate::core::interface::AudioBuffer;

#[derive(Debug, Clone)]
pub struct VecAudioBuffer<S: Copy + Default + std::fmt::Debug> {
    pub samples: Vec<S>,
    pub format: AudioFormat,
    pub frames: usize,
}

impl<S: Copy + Default + std::fmt::Debug> VecAudioBuffer<S> {
    pub fn new(samples: Vec<S>, format: AudioFormat, frames: usize) -> Self {
        Self {
            samples,
            format,
            frames,
        }
    }
}

impl<S: Copy + Default + std::fmt::Debug + Send + Sync + 'static> AudioBuffer
    for VecAudioBuffer<S>
{
    type Sample = S;

    fn as_slice(&self) -> &[Self::Sample] {
        &self.samples
    }

    fn as_mut_slice(&mut self) -> &mut [Self::Sample] {
        &mut self.samples
    }

    fn read_frames(
        &self,
        offset_frames: usize,
        destination: &mut [Self::Sample],
        frames_to_read: usize,
    ) -> AudioResult<usize> {
        if self.format.channels == 0 {
            return Ok(0);
        }
        let samples_per_frame = self.format.channels as usize;
        let read_start_sample_index = offset_frames * samples_per_frame;
        let max_readable_samples_in_self = self.frames * samples_per_frame;

        if read_start_sample_index >= max_readable_samples_in_self {
            return Ok(0); // Offset is beyond the valid frames
        }

        let num_samples_to_attempt_read = frames_to_read * samples_per_frame;
        let available_samples_in_self_from_offset =
            max_readable_samples_in_self - read_start_sample_index;

        let num_samples_can_read_from_self = std::cmp::min(
            num_samples_to_attempt_read,
            available_samples_in_self_from_offset,
        );

        let num_samples_can_write_to_dest = destination.len();
        let actual_samples_to_copy = std::cmp::min(
            num_samples_can_read_from_self,
            num_samples_can_write_to_dest,
        );

        if actual_samples_to_copy > 0 {
            destination[..actual_samples_to_copy].copy_from_slice(
                &self.samples
                    [read_start_sample_index..read_start_sample_index + actual_samples_to_copy],
            );
        }
        Ok(actual_samples_to_copy / samples_per_frame)
    }

    fn write_frames(
        &mut self,
        offset_frames: usize,
        source: &[Self::Sample],
        frames_to_write: usize,
    ) -> AudioResult<usize> {
        if self.format.channels == 0 {
            return Ok(0); // Cannot write if channels is zero
        }
        let samples_per_frame = self.format.channels as usize;
        let write_start_sample_index = offset_frames * samples_per_frame;
        let num_samples_to_attempt_write = frames_to_write * samples_per_frame;

        // Determine how many samples we can actually write from the source
        let actual_samples_to_write = std::cmp::min(num_samples_to_attempt_write, source.len());

        if actual_samples_to_write == 0 {
            return Ok(0);
        }

        let required_len_for_samples = write_start_sample_index + actual_samples_to_write;

        if required_len_for_samples > self.samples.len() {
            self.samples.resize(required_len_for_samples, S::default());
        }

        self.samples[write_start_sample_index..required_len_for_samples]
            .copy_from_slice(&source[..actual_samples_to_write]);

        // Update the number of valid frames
        let end_frame_index =
            (write_start_sample_index + actual_samples_to_write + samples_per_frame - 1)
                / samples_per_frame; // ceiling division
        self.frames = std::cmp::max(self.frames, end_frame_index);

        Ok(actual_samples_to_write / samples_per_frame)
    }

    fn get_length_frames(&self) -> usize {
        self.frames
    }

    fn get_capacity_frames(&self) -> usize {
        if self.format.channels == 0 {
            0
        } else {
            self.samples.capacity() / (self.format.channels as usize)
        }
    }

    fn get_format(&self) -> AudioFormat {
        self.format.clone()
    }

    fn convert_to_format(
        &self,
        _target_format: &AudioFormat,
    ) -> AudioResult<Box<dyn AudioBuffer<Sample = Self::Sample>>> {
        // This is a complex operation and depends on the sample type S.
        // For f32, it would involve resampling and channel conversion.
        // For now, it's a placeholder.
        todo!("Implement format conversion for VecAudioBuffer. This requires resampling and channel/format adjustments.")
    }

    fn clear(&mut self) {
        self.samples.clear(); // Vec::clear also sets its length to 0
        self.frames = 0;
    }

    fn resize_length(&mut self, new_length_frames: usize) -> AudioResult<()> {
        if self.format.channels == 0 {
            if new_length_frames > 0 {
                // Or return an error: AudioError::InvalidOperation("Cannot resize with 0 channels to non-zero frames")
                return Ok(()); // Or perhaps an error
            } else {
                self.samples.clear(); // Ensure samples vec is also empty
                self.frames = 0;
                return Ok(());
            }
        }
        let samples_per_frame = self.format.channels as usize;
        let new_sample_length = new_length_frames * samples_per_frame;
        self.samples.resize(new_sample_length, S::default());
        self.frames = new_length_frames;
        Ok(())
    }
}
