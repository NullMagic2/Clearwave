#!/usr/bin/env python3
"""Evaluate an explicit diagnostic input conditioner around Clearwave's raw stream.

This is a rejected quality experiment, not an application mode or parity claim.
It reads existing local speech and never captures, plays, downloads or uploads it.
The shared speech benchmark supplies decoding, output files and measurements.
"""
import argparse
from collections import deque
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import subprocess

import numpy as np

import benchmark_speech as benchmark

HOP = 480
ALGORITHMIC_DELAY_HOPS = 6
PEAK_CAPACITY = 4
LONG_PEAK_HOPS = 7500
RECENT_PEAK_HOPS = 1000
ACTIVITY_HOPS = 15
ACTIVITY_THRESHOLD = np.float32(0.0001)
CONDITION_THRESHOLD = np.float32(0.2)
INFERENCE_PEAK = np.float32(0.3)
VOICE_RMS = 0.006
NOISE_SNR_DB = 10.0
NOISE_SEED = 27102026
TAIL_SECONDS = 1
CLICK_FIRST_SAMPLE = 10000
CLICK_INTERVAL = 24000
CLICK_DECAY_SAMPLES = 90
CLICK_AMPLITUDE = 3.0
HUM_FREQUENCIES = [(60, 0.8), (120, 0.3)]
FLOAT_BYTES = np.dtype('<f4').itemsize
MAX_SECONDS = 60


@dataclass
class Peak:
    value: float
    remaining: int


class PeakTracker:
    """Keep four peak candidates with independent remaining lifetimes.

    Admission precedes expiration in the reference update. Copying candidates
    preserves remaining lifetimes, including a shorter tracker's expiry times.
    """

    def __init__(self, window):
        self.window = window
        self.entries = []

    def update(self, value):
        if len(self.entries) == PEAK_CAPACITY:
            least = min(range(len(self.entries)), key=lambda i: self.entries[i].value)
            if value > self.entries[least].value:
                self.entries.pop(least)
                self.entries.append(Peak(value, self.window))
        else:
            self.entries.append(Peak(value, self.window))

        self.entries = [entry for entry in self.entries if entry.remaining > 0]
        for entry in self.entries:
            entry.remaining -= 1
        return max((entry.value for entry in self.entries), default=1.0)

    def replace(self, other):
        self.entries = [Peak(entry.value, entry.remaining) for entry in other.entries]


class Conditioner:
    """Track input peaks and delayed processed-output activity.

    Compensation uses the current input peak, as the inspected call order does.
    This interaction with spectral lookahead remains a waveform-level concern.
    """

    def __init__(self):
        self.long = PeakTracker(LONG_PEAK_HOPS)
        self.recent = PeakTracker(RECENT_PEAK_HOPS)
        self.activity = deque([np.float32(0)] * ACTIVITY_HOPS, maxlen=ACTIVITY_HOPS)
        self.activity_sum = np.float32(0)
        self.active = False
        self.gate = False
        self.maximum = np.float32(1)

    def input(self, hop):
        peak = float(np.max(np.abs(hop)))
        self.maximum = np.float32(self.long.update(peak))
        recent = self.recent.update(peak)

        if not self.active and self.gate and (
                self.maximum < CONDITION_THRESHOLD or recent < CONDITION_THRESHOLD):
            self.active = True
            if self.maximum > CONDITION_THRESHOLD:
                self.long.replace(self.recent)
        elif self.active and self.maximum >= CONDITION_THRESHOLD:
            self.active = False

        if self.active:
            if self.maximum <= 0:
                raise ValueError('Diagnostic conditioner reached zero active peak; '
                                 'no undocumented gain floor is substituted')
            return (hop / self.maximum) * INFERENCE_PEAK
        return hop

    def output(self, hop):
        block_sum = np.cumsum(np.abs(hop), dtype=np.float32)[-1]
        self.activity_sum = np.float32(self.activity_sum - self.activity[0])
        self.activity.append(block_sum)
        self.activity_sum = np.float32(self.activity_sum + block_sum)
        self.gate = self.activity_sum / np.float32(ACTIVITY_HOPS * HOP) > ACTIVITY_THRESHOLD

        if self.active:
            return (hop * self.maximum) / INFERENCE_PEAK
        return hop


def read_block(source):
    payload = source.read(HOP * FLOAT_BYTES)
    if len(payload) != HOP * FLOAT_BYTES:
        raise RuntimeError('The raw stream ended before a complete output hop; inspect its log')
    return np.frombuffer(payload, dtype='<f4').copy()


def process(binary, model, adapter, values, enabled, log_path):
    command = [str(binary), 'stream', str(model), '1']
    if adapter:
        command.append(adapter)

    original_samples = len(values)
    values = np.pad(values, (0, (-original_samples) % HOP))
    conditioner = Conditioner()
    output = []
    states = []
    with log_path.open('x') as log:
        child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log)
        try:
            for index, start in enumerate(range(0, len(values), HOP)):
                hop = values[start:start + HOP].astype('<f4')
                conditioned = conditioner.input(hop) if enabled else hop
                child.stdin.write(conditioned.tobytes())
                child.stdin.flush()

                if index >= ALGORITHMIC_DELAY_HOPS:
                    signal = read_block(child.stdout)
                    output.extend(conditioner.output(signal) if enabled else signal)
                elif enabled:
                    conditioner.output(np.zeros(HOP, dtype='<f4'))

                states.append({'hop': index, 'active': conditioner.active,
                               'input_scale': float(INFERENCE_PEAK / conditioner.maximum)
                               if conditioner.active else 1.0})

            child.stdin.close()
            tail = np.frombuffer(child.stdout.read(), dtype='<f4').copy()
            if enabled and conditioner.active:
                tail = (tail * conditioner.maximum) / INFERENCE_PEAK
            output.extend(tail)
            if child.wait() != 0:
                raise RuntimeError(f'Clearwave failed; inspect {log_path}')
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            child.stdout.close()
            if not child.stdin.closed:
                child.stdin.close()
    return np.array(output[:original_samples]), states


def validate_tracker():
    tracker = PeakTracker(PEAK_CAPACITY)
    for value in [1, 2, 3, 4]:
        tracker.update(value)
    assert len(tracker.entries) == PEAK_CAPACITY
    assert tracker.update(0) == 4
    assert tracker.update(0) == 4
    assert tracker.update(0) == 4
    assert tracker.update(0) == 0

    source = PeakTracker(2)
    source.update(1)
    tracker.replace(source)
    assert tracker.update(0) == 1
    assert tracker.update(0) == 0

    conditioner = Conditioner()
    quiet = np.full(HOP, 0.01, dtype='<f4')
    assert np.array_equal(conditioner.input(quiet), quiet)
    conditioner.output(quiet)
    assert conditioner.gate
    assert np.allclose(conditioner.input(quiet), INFERENCE_PEAK)
    assert np.allclose(conditioner.output(np.full(HOP, INFERENCE_PEAK)), quiet)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--model', type=Path, required=True)
    parser.add_argument('--clean', type=Path, nargs='+', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--adapter')
    parser.add_argument('--seconds', type=float, default=6)
    args = parser.parse_args()
    if not np.isfinite(args.seconds) or not 0 < args.seconds <= MAX_SECONDS:
        parser.error(f'Duration must be in (0,{MAX_SECONDS}] seconds')

    validate_tracker()
    binary = args.binary.resolve(strict=True)
    model = args.model.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=False)
    report = {'status': 'diagnostic; rejected for default processing',
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'noise_seed': NOISE_SEED, 'speech_rms': VOICE_RMS,
              'seconds': args.seconds, 'cases': {},
              'scope': 'Static-contract prototype around the current raw stream. '
              'EOF tail uses the final gain, outside scored active speech. '
              'No vendor execution, waveform equivalence or listening validation.'}

    for index, source in enumerate(args.clean):
        voice = benchmark.read_audio(source, seconds=args.seconds)
        if not len(voice) or not np.isfinite(voice).all() or np.mean(voice ** 2) < 1e-12:
            raise ValueError('Reference must contain finite, non-silent speech')
        voice *= VOICE_RMS / np.sqrt(np.mean(voice ** 2))
        clean = np.pad(voice, (0, benchmark.RATE * TAIL_SECONDS))
        active = slice(0, len(voice))

        rng = np.random.default_rng(NOISE_SEED)
        time = np.arange(len(clean)) / benchmark.RATE
        noise = rng.standard_normal(len(clean))
        for frequency, amplitude in HUM_FREQUENCIES:
            noise += amplitude * np.sin(2 * np.pi * frequency * time)
        for offset in range(CLICK_FIRST_SAMPLE, len(clean) - HOP, CLICK_INTERVAL):
            noise[offset:offset + HOP] += (CLICK_AMPLITUDE * rng.standard_normal(HOP)
                                          * np.exp(-np.arange(HOP) / CLICK_DECAY_SAMPLES))
        noise *= VOICE_RMS / np.sqrt(np.mean(noise[active] ** 2))

        for name, mixture in [('clean', clean), ('noisy', clean + noise * 10 ** (-NOISE_SNR_DB / 20))]:
            key = f'{index}-{name}'
            result = {'source': source.name,
                      'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest()}
            for label, enabled in [('baseline', False), ('conditioned', True)]:
                output, states = process(binary, model, args.adapter, mixture, enabled,
                                         args.output / f'{key}-{label}.log')
                if len(output) != len(clean) or not np.isfinite(output).all():
                    raise ValueError('Processed output length or finiteness differs')
                result[label] = benchmark.measures(clean, output, active)
                result[label]['maximum_input_scale'] = max(state['input_scale'] for state in states)
                result[label]['conditioned_hops'] = sum(state['active'] for state in states)
                benchmark.write_audio(args.output / f'{key}-{label}.wav', output)
                (args.output / f'{key}-{label}-state.json').write_text(json.dumps(states) + '\n')
            report['cases'][key] = result
            (args.output / 'comparison.json').write_text(json.dumps(report, indent=2) + '\n')
            print(json.dumps({key: result}), flush=True)


if __name__ == '__main__':
    main()
