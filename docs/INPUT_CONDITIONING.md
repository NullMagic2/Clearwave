# Stateful input-level conditioning investigation

Status: **diagnostic prototype rejected for default processing**. Clearwave's
waveform engine and output gain are unchanged. The prototype reduced residual
noise in these fixtures but introduced clean-speech distortion and worsened the
high-frequency speech loss that motivated this investigation. It does not
establish waveform equivalence or subjective voice quality.

## Static contract

The reference is the local 0.5.12.6 runtime identified in `MODEL_GRAPH.md`.
Addresses are relative virtual addresses. No vendor library was executed, and no
runtime, model or audio asset accompanies this document.

| Behavior | Evidence | Observed operation |
| --- | --- | --- |
| Constants | `0x12120..0x1214f` | Peak candidate capacity 4; history limits 7,500 and 1,000 updates; activation threshold 0.2; conditioned peak 0.3. |
| Tracker configuration | `0x16242..0x16278` | Each tracker receives its history limit and capacity. Its elapsed counter begins at the history limit. |
| Current input peak | `0x13d03..0x13d74` | Maximum absolute input sample; both trackers update before the scaling decision. |
| Candidate retention | `0x14430..0x14529`, `0x15dc0..0x15e9d` | Retain up to four candidates. At capacity, admit a strictly greater peak after removing the smallest. Candidate admission precedes expired-candidate cleanup. |
| Tracked maximum | `0x12930..0x12972` | Read the greatest retained peak; an empty tracker falls back to 1.0. |
| Activation | `0x13df7..0x13e4a` | Activate only when the previous processed-output activity flag is true and either tracked maximum is below 0.2. While active, a long-tracker maximum of at least 0.2 disables conditioning. |
| History replacement | `0x13e15..0x13e34`, `0x17510..0x17666` | On activation, replace the long tracker with the recent tracker if the current long maximum exceeds 0.2. Copy surviving candidates and their remaining lifetimes. The cached maximum for this block changes only on the next input update. |
| Input scaling | `0x13e75..0x13fb3` | While active, divide each sample by the cached long maximum, then multiply by 0.3. |
| Output activity | `0x15d30..0x15db7`, `0x15ea0..0x15fd2` | Maintain 15 block sums of absolute processed output. Divide the running sum by `15 * block_samples`; activity is strictly greater than 0.0001. Initial block sums are zero. |
| Output ordering | `0x16c55..0x16c82` | Compute output activity after reconstruction and before undoing conditioning. Use that flag on the following input call. |
| Output compensation | `0x13b00..0x13cb7` | While active, multiply output by the cached current input maximum, then divide by 0.3. |

The history limits count calls. With 480-sample calls at 48 kHz they correspond
to 75 seconds and 10 seconds; output activity spans 150 ms. The tracker retains a
bounded set of candidates rather than every historical input peak. A copied
recent candidate keeps its remaining lifetime, rather than gaining another
75 seconds.

The compensation uses current-call input state even though reconstructed audio
has spectral and model lookahead. That ordering matters during gain transitions.
Changing the compensation to an earlier input gain could be a different design,
but the inspected ordering does not justify presenting it as a correction.

## Reproducible experiment

`tools/benchmark_conditioning.py` implements the mapped mathematical state
machine around the current raw streaming executable. It reuses
`tools/benchmark_speech.py` for decoding, writing WAVs and reference measurements.
It is an explicit diagnostic tool, not a new processing option. Bounded-state
fixtures check candidate expiration, lifetime-preserving replacement, activity
feedback and reciprocal gain.

The comparison uses the first six seconds of the same two existing public
LibriSpeech clips as `AUDIO_ALIGNMENT.md`, RMS 0.006, no lead-in, and one second
of trailing silence. Controlled static, hum and clicks are mixed at 10 dB SNR.
Both baseline and conditioned paths use the same AMD streaming executable and
model. No output loudness enhancement is added.

The diagnostic pads a final partial input hop and crops it after processing.
Its EOF tail uses the final cached gain; finite-stream tail equivalence is not
claimed. The reported active-speech measurements exclude that tail. Mathematical
state and operation order are reproduced; no claim of bitwise vendor floating-
point arithmetic or a running-vendor comparison is made. A zero active peak
causes a diagnostic failure rather than introducing an undocumented gain floor.

```sh
python3 tools/benchmark_conditioning.py --binary bin/clearwave \
  --model /path/to/models --clean /path/to/voice1.ogg /path/to/voice2.ogg \
  --adapter 7900 --seconds 6 --output /path/to/new-comparison-directory
```

The measurements are preserved in `stateful-conditioning-study.json`. Speech
clarity and intelligibility require separate listening or validated perceptual
measurements; SI-SDR and frequency-band projections are useful diagnostics only.

## Result and next boundary

The run used AMD Radeon RX 7900 XTX through RADV Vulkan. The shared graph and
waveform implementation were identical in both paths.

| Clip / condition | Baseline SI-SDR | Conditioned SI-SDR | Baseline 4–8 kHz voice level | Conditioned 4–8 kHz voice level |
| --- | ---: | ---: | ---: | ---: |
| 198, clean | 17.10 dB | 14.06 dB | −0.61 dB | −1.96 dB |
| 5703, clean | 22.85 dB | 13.85 dB | −0.32 dB | −0.20 dB |
| 198, noisy | 14.34 dB | 14.11 dB | −2.39 dB | −3.02 dB |
| 5703, noisy | 15.81 dB | 15.66 dB | −5.73 dB | −8.86 dB |

Noisy-tail RMS fell from −67.91 to −77.74 dBFS for clip 198 and from −60.18
to −80.78 dBFS for clip 5703. Those quieter tails accompany worse speech
preservation, so they are not sufficient evidence of improved quality.

The conditioner is not adopted because it fails the clean-speech preservation
check and does not improve noisy speech detail consistently. Reduced tail noise
alone does not meet the user's request to preserve their voice. This investigation
adds no implementation-percentage credit.

The remaining useful boundary is an independent waveform reference or a verified
frontend discrepancy. CPU/AMD agreement and quieter silence cannot establish
that boundary. Applying an undocumented voice equalizer or gain boost would not
resolve the missing evidence.
