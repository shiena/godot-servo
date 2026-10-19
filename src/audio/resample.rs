//! Stereo sample-rate conversion to the mixer's rate.

use speexdsp_resampler::State;

/// Speex's quality scale runs 0 to 10; 5 is its suggestion for desktop use.
const QUALITY: usize = 5;

/// Convert a whole channel at once, keeping its length in time: the result
/// starts where the input does and has `len * out_rate / in_rate` samples.
pub fn resample_whole(input: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    if in_rate == out_rate || input.is_empty() {
        return input.to_vec();
    }
    let Ok(mut state) = State::new(1, in_rate as usize, out_rate as usize, QUALITY) else {
        return input.to_vec();
    };
    let expected = (input.len() as u64 * out_rate as u64 / in_rate as u64) as usize;

    // The filter delays its output; skip_zeros drops that delay at the start,
    // and the zeros appended here push the last samples out at the end.
    state.skip_zeros();
    let mut padded = input.to_vec();
    padded.resize(input.len() + state.get_input_latency(), 0.0);

    let mut output = vec![0.0; expected + state.get_output_latency() + 64];
    let (mut read, mut written) = (0, 0);
    while read < padded.len() && written < output.len() {
        match state.process_float(0, &padded[read..], &mut output[written..]) {
            Ok((0, 0)) | Err(_) => break,
            Ok((consumed, produced)) => {
                read += consumed;
                written += produced;
            }
        }
    }
    output.truncate(written.min(expected));
    output.resize(expected, 0.0);
    output
}

pub struct StereoResampler {
    /// `None` when the rates match and the samples pass through.
    state: Option<State>,
    ratio: f64,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl StereoResampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let state = (in_rate != out_rate)
            .then(|| State::new(2, in_rate as usize, out_rate as usize, QUALITY).ok())
            .flatten();
        Self {
            state,
            ratio: out_rate as f64 / in_rate as f64,
            left: Vec::new(),
            right: Vec::new(),
        }
    }

    /// Convert planar `left` and `right`, handing the result to `emit` in one
    /// or more pieces.
    pub fn process(&mut self, left: &[f32], right: &[f32], mut emit: impl FnMut(&[f32], &[f32])) {
        let Some(state) = &mut self.state else {
            emit(left, right);
            return;
        };
        let capacity = (left.len() as f64 * self.ratio).ceil() as usize + 16;
        self.left.resize(capacity, 0.0);
        self.right.resize(capacity, 0.0);

        let mut offset = 0;
        while offset < left.len() {
            // Both channels see the same lengths, so they consume and produce
            // the same counts.
            let Ok((read, written)) = state.process_float(0, &left[offset..], &mut self.left)
            else {
                return;
            };
            if state
                .process_float(1, &right[offset..], &mut self.right)
                .is_err()
            {
                return;
            }
            emit(&self.left[..written], &self.right[..written]);
            if read == 0 && written == 0 {
                return;
            }
            offset += read;
        }
    }
}
