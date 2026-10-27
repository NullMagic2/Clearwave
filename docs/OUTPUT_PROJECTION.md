# Output projection layout correction

Versions through 0.6.5 interpreted `noise_denoise_filter_1_weights` as an
output-major `[961, 1024]` matrix. Its stored layout is input-major
`[1024, 961]`. The mistaken interpretation scrambles the last learned projection;
valid finite masks and matching CPU/GPU results did not expose that error.

The supported 0.5.12.6 runtime's projection setup supplies input width 1024 and
output width 961. Its SGEMM call uses an untransposed input, a transposed weight
operand, `m = batch`, `n = 961`, `k = 1024`, and `ldb = 961`. Under
[cuBLAS column-major GEMM semantics](https://docs.nvidia.com/cuda/cublas/index.html#cublas-t-gemm),
the stored weight offset for input `i` and frequency bin `f` is `i*961 + f`.
The relevant runtime RVAs are `0x2d6c5..0x2d6e1` (dimensions),
`0x29294..0x2929e` (dimension fields), and `0x29c18..0x29c5b` (GEMM setup).
This static call analysis is not a comparison with a running NVIDIA effect.

The reference runtime SHA-256 is
`c3845f23a9e1859d8a7bb42eb683a2767ecfa3b7f241980d4202655f1c1a1e03`.
No runtime or model files accompany this document.

Clearwave now transposes this one matrix at network setup. The CPU path caches
execution rows; the Vulkan path uploads the corrected rows once and retains
them between frames. Original/exported weights are unchanged. A non-square
matrix regression checks the required input-to-output mapping.

## Controlled comparisons

`projection-quality-check.json` records generated speech with deterministic white
noise. `natural-speech-check.json` records two LibriSpeech readings with clean
references and with deterministic static, hum, and clicks at 10 dB input SNR.
Both use full processing strength, without output gain or volume normalization
after denoising. Input speech is scaled to RMS 0.06 before adding noise.

The natural voices are Garth Comira reading *The Ashiel Mystery* and Heather
Barnett reading *Sense and Sensibility*, provided as librosa examples from
[LibriSpeech SLR12](https://www.openslr.org/12/), CC BY 4.0. Their URLs and pinned
file hashes are in the report. Audio is not included in the source or packages.

The voice projection level is the least-squares gain of the clean reference in
the output. SI-SDR counts remaining noise and changes to speech as distortion,
while separating overall level. It is not an intelligibility score. Tail noise
is measured in the final 40,000 samples, after the reference voice ends.

To repeat with existing local reference clips, run from the project directory:

```sh
python3 tools/benchmark_speech.py --binary bin/clearwave \
  --baseline /path/to/clearwave-0.6.5 --model /path/to/models \
  --clean /path/to/clean-speech.ogg --output /path/to/new-benchmark-directory
```

The tool requires NumPy and FFmpeg. It never records, plays, uploads, or downloads
audio. It evaluates at most the first 60 seconds of each reference.

## Remaining limits

The corrected graph improves these comparisons and a local microphone sample,
but the latter still exhibits muffled speech and residual static. No general
speech-quality claim or equivalence to NVIDIA's output follows from this fix.
The complete feature contract and broader real-speech behavior remain open work.
The additional intensity rule below full strength is implemented from its static
contract, with dedicated spectral and streaming checks; vendor parity is pending.

Static analysis of the additional intensity rule shows that noise-mask scaling
is conditional on a threshold in predicted noise power. At full intensity the
scale factor is one, so the rule leaves `1 - mask` unchanged. That missing rule
cannot explain the remaining artifacts in the full-strength sample.
