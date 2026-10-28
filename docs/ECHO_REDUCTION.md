# Optional echo reduction

The main window includes **Echo reduction (experimental)** directly below noise removal, from 0% to 100%. The default
is 0%, which bypasses this stage exactly and allocates no predictor state. Existing
settings and live-control JSON files load with echo reduction off. The selection
is saved automatically and changes during capture without restarting the model.
Noise-removal strength remains independent.

This stage targets late room reverberation recorded by a single microphone.
It cannot identify every microphone effect, undo clipped input, or cancel
loudspeaker feedback without a playback reference. A high setting can alter
sustained speech, so increase it gradually and compare a saved recording.
100% means the maximum supported mix, not a promise of removing all echoes.

## Processing

The shared Rust stage uses frame-online weighted prediction error (WPE), based on
[Eqs. 11–17 in Drude et al., NARA-WPE (2018)](https://groups.uni-paderborn.de/nt/pubs/2018/ITG_2018_Drude_Paper.pdf).
It estimates a delayed spectral component with eight complex prediction taps and
a five-hop prediction delay (50 ms). The predictor uses recursive weighted least
squares, a forgetting factor of 0.995, and a causal smoothed input-power estimate.
The delay protects short correlations but does not guarantee speech preservation.

Prediction uses raw spectra in chronological order; model features and hidden
states are unchanged. The predicted residual magnitude produces an attenuation
mask capped at 70%. The mask is averaged over three frequency bins and smoothed
between frames (0.75 previous / 0.25 current), then scaled by the echo slider and
low-voice protection. The original complex spectrum is multiplied by a real gain
between zero and one, preserving its phase. This replaces direct complex
subtraction, which could rotate noise and speech bins independently across frames.
It prevents per-bin amplification, not every possible change in reconstructed
waveform peaks. Invalid covariance state resets the affected bin.
Starting with 0.7.1-dev.5, cancellation is additionally weighted to 25% below
250 Hz, rising to full strength at 600 Hz, to limit removal of sustained low
voice tones. Default output also receives the shared
[microphone voicing stage](VOICE_BALANCE.md) after reconstruction.

Linux and Windows run this stage in their existing audio worker, outside the UI
thread and device callbacks. It reuses the existing 40 ms transform, 10 ms hop,
and overlap reconstruction. It adds no lookahead or output samples. Predictor
state is bounded by the fixed frequency-bin and tap counts, and is discarded
when switched off. Turning it on again starts a new adaptation period.

## Offline comparison

The optional final CLI argument is a fraction, unlike the UI's percentage:

```sh
clearwave denoise MODEL INPUT.wav NEW_OUTPUT.wav 0.5 AMD 0.25
clearwave denoise-cpu MODEL INPUT.wav NEW_OUTPUT.wav 0.5 0.25
```

These select 50% noise removal and 25% echo reduction. Omitting the final
argument keeps echo reduction off. Live desktop control files store
`{"strength":0.5,"echo_reduction":0.25}` atomically.

## Verification and limits

Checks cover exact bypass, settings/control compatibility and bounds, an actual
Windows trackbar message and saved preference, streaming/offline agreement with
partial final chunks, finite output, prediction of a synthetic delayed
reflection, phase preservation on changing complex input, and bounded per-bin gain. They do not establish perceptual quality or performance on every
microphone. No claim of NVIDIA dereverberation equivalence is made.
