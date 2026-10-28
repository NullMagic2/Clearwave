# Default microphone voice balance

Version 0.7.1-dev.7 exposes the existing `balanced-v1` voice EQ as a live **Bass**
slider on Linux and Windows. Its range is -6 to +6 dB in 0.5 dB steps, centred at
180 Hz with Q 0.8. The default stays +2.5 dB, matching earlier versions; 0 dB is
neutral EQ, while peak protection remains active. Saved preferences and worker
control messages include `bass_db`; old files without it use +2.5 dB.

The shared safe Rust stage runs after denoising, waveform reconstruction and
Broadcast output compensation. Changed filter coefficients interpolate over 960
samples (20 ms at 48 kHz), retaining filter history. Repeated unchanged control
messages do not restart the ramp. GUI controls do not perform audio calculations.
The regular `denoise` / `denoise-cpu` commands retain the default +2.5 dB profile.

It follows the peaking-filter equations in the
[W3C Audio EQ Cookbook](https://www.w3.org/TR/audio-eq-cookbook/).
DC and high frequencies have unity limiting response; unlike a low shelf, it
does not apply its full gain to sub-bass rumble. Filter state uses F64 precision.

A smooth peak guard acts only above 0.98 full scale, with output bounded to ±1.
It leaves quieter samples alone and uses no lookahead or additional buffering.
The filter has normal frequency-dependent phase response; no input samples are
added or removed. This is tonal balancing, not a learned voice-restoration model,
and cannot recreate bass that the microphone did not capture. Residual low-band
noise can also receive some boost.

The experimental echo stage now uses 25% of its former cancellation mix below
250 Hz, rising linearly to the full mix at 600 Hz. This protects low voice tones
that the delayed predictor can confuse with room reflections, at the cost of
weaker low-frequency reverberation reduction. The echo slider and its exact 0%
bypass remain. This frequency rule does not classify speech or prove speech
preservation in every room.

## Model comparisons

The learned graph, features, masks and model-specific strength curves are
unchanged. Default microphone output is deliberately voiced and is no longer a
neutral waveform candidate for the NVIDIA comparison. Use these diagnostic
commands with echo reduction off to compare the unchanged model path:

```sh
clearwave denoise-reference MODEL INPUT.wav NEW_OUTPUT.wav 1
clearwave denoise-reference-cpu MODEL INPUT.wav NEW_OUTPUT.wav 1
```

`audio::Processor` and the neutral streaming constructor remain available to
internal reference checks. Both desktop workers explicitly construct the shared
microphone stream. Output meters measure the final voiced/guarded samples.

## Checks

Tests measure the bass/treble/rumble response with actual tones, exact filter
state continuity across arbitrary chunk boundaries, peak bounds, silence and
nonfinite input handling. Additional checks measure signed bass gain, validate saved
settings and old-file defaults, and compare coefficient transitions across chunk
sizes. The Windows UI test exercises negative, zero and positive slider values. A sustained-harmonic counterexample checks the echo
protection bound. Both model profiles are checked for offline/live agreement,
including enabled echo reduction and a partial final chunk. These checks
establish processing behavior; listening quality remains subjective.
