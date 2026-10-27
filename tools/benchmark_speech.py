#!/usr/bin/env python3
"""Compare clean speech and controlled noise through local Clearwave executables.

Requires Python 3, NumPy and FFmpeg. Takes existing local audio; never records,
plays, downloads or uploads audio. Use a new output directory outside releases.
"""
import argparse
import hashlib
import json
import struct
import subprocess
from pathlib import Path

import numpy as np

RATE = 48000


def read_audio(path, seconds=62):
    result = subprocess.run(
        ['ffmpeg', '-v', 'error', '-i', str(path), '-t', str(seconds), '-ar', str(RATE),
         '-ac', '1', '-f', 'f32le', 'pipe:1'], capture_output=True, check=True)
    return np.frombuffer(result.stdout, dtype='<f4').astype('float64')


def write_audio(path, values):
    payload = values.astype('<f4').tobytes()
    header = struct.pack('<4sI4s4sIHHIIHH4sI', b'RIFF', 36 + len(payload),
                         b'WAVE', b'fmt ', 16, 3, 1, RATE, RATE * 4, 4, 32,
                         b'data', len(payload))
    with path.open('xb') as output:
        output.write(header + payload)


def measures(reference, output, active):
    speech, signal = reference[active], output[active]
    gain = np.dot(speech, signal) / np.dot(speech, speech)
    projected = gain * speech
    error = signal - projected
    return {
        'voice_projection_gain_db': float(20 * np.log10(max(abs(gain), 1e-12))),
        'si_sdr_db': float(10 * np.log10(max(np.mean(projected ** 2), 1e-24)
                                       / max(np.mean(error ** 2), 1e-24))),
        'tail_rms_dbfs': float(20 * np.log10(max(
            np.sqrt(np.mean(output[-40000:] ** 2)), 1e-12))),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--model', required=True, type=Path)
    parser.add_argument('--clean', required=True, nargs='+', type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--adapter')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    binaries = {'candidate': args.binary.resolve(strict=True)}
    if args.baseline:
        binaries['baseline'] = args.baseline.resolve(strict=True)
    report = {'noise_seed': 27102026, 'cases': {}, 'scope':
              'Local clean-reference comparison, first 60 seconds of each clip. '
              'No automatic gain is added after denoising. Not vendor equivalence.'}
    for index, source in enumerate(args.clean):
        voice = read_audio(source, seconds=60)
        if not len(voice) or not np.isfinite(voice).all() or np.mean(voice ** 2) < 1e-12:
            raise ValueError('Clean reference must contain finite, non-silent audio')
        voice *= 0.06 / np.sqrt(np.mean(voice ** 2))
        clean = np.pad(voice, (RATE, RATE))
        active = slice(RATE, RATE + len(voice))
        rng = np.random.default_rng(report['noise_seed'])
        time = np.arange(len(clean)) / RATE
        noise = (rng.standard_normal(len(clean)) + 0.8 * np.sin(2 * np.pi * 60 * time)
                 + 0.3 * np.sin(2 * np.pi * 120 * time))
        for offset in range(10000, len(clean) - 500, 24000):
            noise[offset:offset + 480] += (3 * rng.standard_normal(480)
                                          * np.exp(-np.arange(480) / 90))
        noise *= 0.06 / np.sqrt(np.mean(noise[active] ** 2))
        for case, mixture in [('clean', clean), ('snr10-click-hum-static',
                                               clean + noise * 10 ** (-10 / 20))]:
            name = f'{index}-{case}'
            input_path = args.output / (name + '.wav')
            write_audio(input_path, mixture)
            result = {'source': source.name,
                      'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
                      'input': measures(clean, mixture, active)}
            for label, binary in binaries.items():
                output_path = args.output / (name + '-' + label + '.wav')
                command = [str(binary), 'denoise', str(args.model), str(input_path),
                           str(output_path), '1']
                if args.adapter:
                    command.append(args.adapter)
                subprocess.run(command, capture_output=True, check=True, timeout=120)
                output = read_audio(output_path)
                if len(output) != len(clean) or not np.isfinite(output).all():
                    raise ValueError('Processed output length or finiteness differs')
                result[label] = measures(clean, output, active)
            report['cases'][name] = result
            print(json.dumps({name: result}, indent=2), flush=True)
    (args.output / 'comparison.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    main()
