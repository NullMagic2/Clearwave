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

Prediction uses raw spectra in chronological order. The predicted component is
subtracted before applying the existing denoiser mask; model features and hidden
states are unchanged. The slider scales this subtraction, not overall volume.
Per-bin subtraction is capped at 70% of observed magnitude; a spectral ceiling
prevents amplification. Invalid covariance state resets the affected bin.

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
partial final chunks, finite output, and prediction of a synthetic delayed
reflection. They do not establish perceptual quality or performance on every
microphone. No claim of NVIDIA dereverberation equivalence is made.
