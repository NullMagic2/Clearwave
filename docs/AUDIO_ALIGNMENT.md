# Waveform alignment and quiet-speech investigation

The supported feature graph is complete, but waveform equivalence and subjective
voice quality remain unverified. This investigation separates frame alignment,
startup state, input-level conditioning, and frequency-dependent speech loss.
The implementation percentage remains 97%; measurements add no implementation
credit.

## Frame and startup evidence

The reference is the local 0.5.12.6 runtime identified in `MODEL_GRAPH.md`.
Addresses below are relative virtual addresses. No vendor library was executed,
and no model or runtime files accompany this document.

| Behavior | Static evidence | Clearwave behavior |
| --- | --- | --- |
| Waveform dimensions | `0x1633b..0x16383`: hop 480, transform 1920, spectrum 961, features 320, context 7 and radius 3 | The existing dimensions are retained. |
| Spectrum history | `0x16124..0x16175` passes `7 - 3 = 4` to the spectral constructor; `0x1ba2a..0x1ba51` creates four spectrum slots | Three frames of spectral history are required. |
| Mask alignment | `0x1ea45..0x1ea80` advances the ring index after selecting the current write slot; `0x16b85..0x16ba3` selects the next slot for masking | The mask applies three frames behind the latest feature arrival, at context index 3. The earlier steady-state alignment was correct. |
| First inference | `0x16b19..0x16b4d` processes input features then invokes the model for each arriving hop; there is no initial context-fill gate | Inference now starts with the first feature arrival. Three predictions for virtual spectra are discarded while their recurrent states are retained. |
| Waveform level conditioning | `0x12120..0x1214f` supplies peak thresholds 0.2/0.3 and tracker parameters; `0x13cc0..0x13fd8` conditionally scales input; `0x13b00..0x13cb7` compensates the output | The complete reference stateful conditioner remains unimplemented. A fixed input boost is not substituted for it. |

Both offline and streaming processing now use the same bounded `FrameContext`.
It begins with six zero feature frames and appends each arriving real frame.
The first three inference results address virtual spectra before frame zero;
the fourth result masks the first real spectrum. This preserves recurrent state
that the previous adapter skipped. It does not add samples or output gain.

The first emitted waveform hop still follows six input hops of algorithmic delay
(2,880 samples / 60 ms). Device and scheduling delays are additional. End-of-file
flush preserves the original sample count. The raw feature-inference CLI remains
a separate zero-state feature-sequence interface; waveform startup priming is
part of the audio adapter.

## Measured changes

`startup-speech-check.json` compares the corrected startup with the previous
adapter on the first six seconds of two public LibriSpeech voices, without added
lead-in silence. Speech is scaled to RMS 0.006; controlled static, hum, and clicks
are mixed at 10 dB SNR. No output gain is added. The reports include separate
80–1,000 Hz, 1–4 kHz, and 4–8 kHz measurements.

| Clip / condition | Earlier SI-SDR | Corrected SI-SDR | Corrected voice level |
| --- | ---: | ---: | ---: |
| Voice 1, clean | 22.82 dB | 22.85 dB | −0.06 dB |
| Voice 1, noisy | 15.41 dB | 15.81 dB | −0.42 dB |
| Voice 2, clean | 17.14 dB | 17.10 dB | −0.25 dB |
| Voice 2, noisy | 14.25 dB | 14.34 dB | −0.49 dB |

These are small, mixed changes, not a claim that muffling is fixed. In the noisy
cases, the corrected 4–8 kHz voice projection loses 5.73 dB and 2.39 dB despite
less than 0.5 dB of overall voice-level loss. That supports investigating speech
detail separately from loudness. SI-SDR and band projections are reference-based
measurements, not listening or intelligibility scores.

`input-level-study.json` records an earlier exploratory global level adjustment:
scale quiet input toward a 0.3 peak before inference, then exactly undo the scale
on output. It slightly improved noisy-speech SI-SDR in the sampled cases, but
increased clean-speech distortion and residual tail noise. It was not adopted.
This does not evaluate the full reference conditioner, which is stateful and
conditional on signal activity.

To repeat the startup comparison with existing local public clips:

```sh
python3 tools/benchmark_speech.py --binary bin/clearwave \
  --baseline /path/to/clearwave-before-startup-fix \
  --model /path/to/models --clean /path/to/voice1.ogg /path/to/voice2.ogg \
  --rms 0.006 --seconds 6 --lead-in-seconds 0 \
  --adapter 7900 --output /path/to/new-comparison-directory
```

`startup-stream-check.json` also records sample-identical offline and raw
streaming AMD output for 336,000 public quiet-speech samples.

All 39 Linux unit tests pass, including state priming, exact context progression,
offline/streaming agreement with ragged chunks, unity reconstruction at file
boundaries, unchanged streaming delay, and failure handling during priming.
The graph's 30 AMD checks are recorded separately and do not validate waveform
parity. No private microphone recording was made for this investigation.

The Windows executable was rebuilt with the same adapter change. All 35
Windows-target tests pass under Wine 11.20, and Clippy passes for Linux and
Windows with warnings denied. Physical Windows audio validation remains pending.

## Follow-up: stateful conditioning

The stateful input conditioner has now been mapped and evaluated as an explicit
streaming diagnostic. It reduced residual tail noise but worsened clean-speech
distortion and noisy high-frequency speech preservation. It is not enabled in
the application, and the waveform engine is unchanged. See
[INPUT_CONDITIONING.md](INPUT_CONDITIONING.md) for the static state-machine
contract, reproducible command, scope limitations and rejected measurements.
