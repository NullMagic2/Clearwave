# Default microphone voice balance

Version 0.7.1-dev.5 uses `balanced-v1` voicing for microphone output on Linux and
Windows, and for the regular `denoise` / `denoise-cpu` commands. It is automatic;
there is no new setting. Existing preferences stay compatible.

The shared safe Rust stage applies a modest +2.5 dB peaking EQ at 180 Hz, Q 0.8,
after denoising, waveform reconstruction and Broadcast output compensation.
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
nonfinite input handling. A sustained-harmonic counterexample checks the echo
protection bound. Both model profiles are checked for offline/live agreement,
including enabled echo reduction and a partial final chunk. These checks
establish processing behavior; listening quality remains subjective.
