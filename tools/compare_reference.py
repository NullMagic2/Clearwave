#!/usr/bin/env python3
"""Compare existing local reference and candidate audio without gain normalization.

Delays are explicit sample counts. No recording, playback, network or model loading
occurs here. Reuses the speech benchmark's decoder, rate and speech measurements.
"""
import argparse
import hashlib
import json
from pathlib import Path

import numpy as np

import benchmark_speech as benchmark

ENERGY_FLOOR = 1e-24
MAX_SECONDS = 60


def read(path):
    if path.suffix == '.f32':
        samples = np.fromfile(path, dtype='<f4').astype('float64')
    else:
        samples = benchmark.read_audio(path, seconds=MAX_SECONDS)
    if not len(samples) or not np.isfinite(samples).all():
        raise ValueError('Audio must be nonempty and finite')
    return samples


def snr(reference, actual):
    return float(10 * np.log10(max(np.mean(reference ** 2), ENERGY_FLOOR)
                              / max(np.mean((actual - reference) ** 2), ENERGY_FLOOR)))


def identity(path):
    return {'filename': path.name, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reference', required=True, type=Path)
    parser.add_argument('--candidate', required=True, type=Path)
    parser.add_argument('--reference-delay', type=int, default=0, help='Samples to remove')
    parser.add_argument('--candidate-delay', type=int, default=0, help='Samples to remove')
    parser.add_argument('--clean', type=Path)
    parser.add_argument('--input', type=Path)
    parser.add_argument('--min-output-snr-db', type=float)
    parser.add_argument('--min-correlation', type=float)
    parser.add_argument('--max-band-gain-difference-db', type=float)
    parser.add_argument('--reference-context', required=True,
                        help='How the reference was produced, including runtime/hardware')
    parser.add_argument('--json', required=True, type=Path)
    args = parser.parse_args()
    if min(args.reference_delay, args.candidate_delay) < 0:
        parser.error('Delays must be nonnegative sample counts')
    if args.input and not args.clean:
        parser.error('--input also requires --clean')
    if args.max_band_gain_difference_db is not None and not args.clean:
        parser.error('Voice-band acceptance requires --clean')
    criteria = {key: value for key, value in {
        'min_output_snr_db': args.min_output_snr_db,
        'min_correlation': args.min_correlation,
        'max_band_gain_difference_db': args.max_band_gain_difference_db,
    }.items() if value is not None}
    if any(not np.isfinite(value) for value in criteria.values()):
        parser.error('Acceptance limits must be finite')
    if args.min_correlation is not None and not -1 <= args.min_correlation <= 1:
        parser.error('Correlation must be between -1 and 1')
    if args.max_band_gain_difference_db is not None and args.max_band_gain_difference_db < 0:
        parser.error('The voice-band difference limit must be nonnegative')

    audio = {'reference': read(args.reference)[args.reference_delay:],
             'candidate': read(args.candidate)[args.candidate_delay:]}
    if args.clean:
        audio['clean'] = read(args.clean)
    if args.input:
        audio['input'] = read(args.input)
    length = min(map(len, audio.values()))
    if not length:
        parser.error('Delays leave no overlapping audio')
    lengths = {key: len(value) for key, value in audio.items()}
    audio = {key: value[:length] for key, value in audio.items()}
    reference, candidate = audio['reference'], audio['candidate']
    if min(np.var(reference), np.var(candidate)) <= ENERGY_FLOOR:
        raise ValueError('Both outputs must contain a non-silent signal')

    result = {
        'scope': 'Fixed-delay waveform comparison without gain normalization; '
                 'one fixture cannot establish general output equivalence.',
        'reference_context': args.reference_context,
        'reference': identity(args.reference), 'candidate': identity(args.candidate),
        'sample_rate': benchmark.RATE, 'aligned_samples': length,
        'available_samples_after_delay': lengths,
        'reference_delay_samples': args.reference_delay,
        'candidate_delay_samples': args.candidate_delay,
        'output_correlation': float(np.corrcoef(reference, candidate)[0, 1]),
        'reference_to_candidate_snr_db': snr(reference, candidate),
    }
    if args.clean:
        clean = audio['clean']
        result['clean'] = identity(args.clean)
        result['clean_reference'] = {
            key: {'snr_db': snr(clean, value),
                  **benchmark.measures(clean, value, slice(None))}
            for key, value in audio.items() if key != 'clean'
        }
        reference_bands = result['clean_reference']['reference']['voice_bands']
        candidate_bands = result['clean_reference']['candidate']['voice_bands']
        result['max_band_gain_difference_db'] = max(
            abs(candidate_bands[band]['voice_projection_gain_db'] - values['voice_projection_gain_db'])
            for band, values in reference_bands.items() if values is not None)
    if criteria:
        passed = all([
            args.min_output_snr_db is None or result['reference_to_candidate_snr_db'] >= args.min_output_snr_db,
            args.min_correlation is None or result['output_correlation'] >= args.min_correlation,
            args.max_band_gain_difference_db is None or result['max_band_gain_difference_db'] <= args.max_band_gain_difference_db,
        ])
        result.update(criteria=criteria, passed=passed)
    with args.json.open('x') as output:
        json.dump(result, output, indent=2, allow_nan=False)
        output.write('\n')
    print(json.dumps(result, indent=2, allow_nan=False))
    if criteria and not result['passed']:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
