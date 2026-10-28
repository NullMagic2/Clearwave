//! Preallocated realtime bridge with bounded queues and time-aligned dry fallback.
//! The callback never waits for IPC or GPU work and never opens an audio device.
//! Missing, late and invalid worker packets cannot be replayed as current audio.
use super::protocol::{HOP, LATENCY, MAX_CHANNELS, NO_AUDIO, Packet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

const QUEUE_PACKETS: usize = 32;
const HISTORY: usize = LATENCY + HOP * QUEUE_PACKETS;
const FADE_SAMPLES: f32 = HOP as f32;

struct Slot {
    sequence: AtomicU64,
    samples: [AtomicU32; HOP],
}
pub struct Queue {
    slots: Box<[Slot]>,
    read: AtomicUsize,
    write: AtomicUsize,
}
impl Default for Queue {
    fn default() -> Self {
        Self {
            slots: (0..QUEUE_PACKETS)
                .map(|_| Slot {
                    sequence: AtomicU64::new(NO_AUDIO),
                    samples: std::array::from_fn(|_| AtomicU32::new(0)),
                })
                .collect(),
            read: AtomicUsize::new(0),
            write: AtomicUsize::new(0),
        }
    }
}
impl Queue {
    pub fn push(&self, packet: &Packet) -> bool {
        let write = self.write.load(Ordering::Relaxed);
        if write.wrapping_sub(self.read.load(Ordering::Acquire)) >= QUEUE_PACKETS {
            return false;
        }
        let slot = &self.slots[write % QUEUE_PACKETS];
        slot.sequence.store(packet.sequence, Ordering::Relaxed);
        for (target, sample) in slot.samples.iter().zip(packet.samples) {
            target.store(sample.to_bits(), Ordering::Relaxed);
        }
        self.write.store(write.wrapping_add(1), Ordering::Release);
        true
    }
    pub fn discard_backlog(&self, packet: &mut Packet) {
        let count = self
            .write
            .load(Ordering::Acquire)
            .wrapping_sub(self.read.load(Ordering::Acquire));
        if count > LATENCY / HOP / 2 {
            for _ in 0..count.saturating_sub(1) {
                if !self.pop(packet) {
                    break;
                }
            }
        }
    }
    pub fn pop(&self, packet: &mut Packet) -> bool {
        let read = self.read.load(Ordering::Relaxed);
        if read == self.write.load(Ordering::Acquire) {
            return false;
        }
        let slot = &self.slots[read % QUEUE_PACKETS];
        packet.sequence = slot.sequence.load(Ordering::Relaxed);
        for (target, sample) in packet.samples.iter_mut().zip(&slot.samples) {
            *target = f32::from_bits(sample.load(Ordering::Relaxed));
        }
        self.read.store(read.wrapping_add(1), Ordering::Release);
        true
    }
}
pub struct Exchange {
    pub input: Queue,
    pub output: Queue,
    pub stop: AtomicBool,
    pub connected: AtomicBool,
}
impl Default for Exchange {
    fn default() -> Self {
        Self {
            input: Queue::default(),
            output: Queue::default(),
            stop: AtomicBool::new(false),
            connected: AtomicBool::new(false),
        }
    }
}
pub struct Bridge {
    channels: usize,
    cursor: u64,
    dry: Box<[[f32; MAX_CHANNELS]]>,
    wet: Box<[f32]>,
    stamps: Box<[u64]>,
    pending: Packet,
    pending_count: usize,
    blend: f32,
}
impl Bridge {
    pub fn new(channels: usize) -> Option<Self> {
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return None;
        }
        Some(Self {
            channels,
            cursor: 0,
            dry: vec![[0.0; MAX_CHANNELS]; HISTORY].into(),
            wet: vec![0.0; HISTORY].into(),
            stamps: vec![NO_AUDIO; HISTORY].into(),
            pending: Packet::default(),
            pending_count: 0,
            blend: 0.0,
        })
    }
    pub fn frame(
        &mut self,
        input: [f32; MAX_CHANNELS],
        exchange: &Exchange,
    ) -> [f32; MAX_CHANNELS] {
        // Each hop drains at most the queue capacity, bounding callback work.
        // Packet stamps identify the original input sample, not arrival time.
        // A late packet can only replace its own matching dry history sample.
        if self.pending_count == 0 {
            let mut packet = Packet::default();
            for _ in 0..QUEUE_PACKETS {
                if !exchange.output.pop(&mut packet) {
                    break;
                }
                if packet.sequence == NO_AUDIO
                    || packet.sequence > self.cursor
                    || packet.sequence.saturating_add(HOP as u64)
                        < self.cursor.saturating_sub(LATENCY as u64)
                {
                    continue;
                }
                for (offset, value) in packet.samples.iter().enumerate() {
                    let sequence = packet.sequence + offset as u64;
                    let slot = sequence as usize % HISTORY;
                    self.wet[slot] = *value;
                    self.stamps[slot] = sequence;
                }
            }
            self.pending.sequence = self.cursor;
        }
        let slot = self.cursor as usize % HISTORY;
        self.dry[slot] = input.map(|v| if v.is_finite() { v } else { 0.0 });
        self.pending.samples[self.pending_count] =
            self.dry[slot][..self.channels].iter().sum::<f32>() / self.channels as f32;
        self.pending_count += 1;
        if self.pending_count == HOP {
            if exchange.connected.load(Ordering::Acquire) {
                exchange.input.push(&self.pending);
            }
            self.pending_count = 0;
        }
        let mut result = [0.0; MAX_CHANNELS];
        if let Some(sequence) = self.cursor.checked_sub(LATENCY as u64) {
            let slot = sequence as usize % HISTORY;
            let available =
                self.stamps[slot] == sequence && exchange.connected.load(Ordering::Acquire);
            self.blend = if available {
                (self.blend + 1.0 / FADE_SAMPLES).min(1.0)
            } else {
                (self.blend - 1.0 / FADE_SAMPLES).max(0.0)
            };
            // Never hold an old waveform during an underrun: use aligned dry audio.
            // Crossfade into processed audio when current packets return.
            // The constant delay is also reported through the APO latency contract.
            for (channel, output) in result.iter_mut().enumerate().take(self.channels) {
                let dry = self.dry[slot][channel];
                let wet = if available { self.wet[slot] } else { dry };
                *output = dry + self.blend * (wet - dry);
            }
        }
        self.cursor += 1;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_failure_and_stale_packets_preserve_time_aligned_microphone() {
        let exchange = Exchange::default();
        let mut bridge = Bridge::new(2).unwrap();
        for i in 0..LATENCY * 3 {
            let input = [(i % 97) as f32 / 100.0, -0.1];
            let output = bridge.frame(input, &exchange);
            if i >= LATENCY {
                assert_eq!(output, [((i - LATENCY) % 97) as f32 / 100.0, -0.1]);
            } else {
                assert_eq!(output, [0.0; MAX_CHANNELS]);
            }
        }
        assert!(Bridge::new(MAX_CHANNELS + 1).is_none());
    }
    #[test]
    fn queue_is_bounded_and_processed_packets_replace_only_matching_samples() {
        let exchange = Exchange::default();
        exchange.connected.store(true, Ordering::Release);
        let mut bridge = Bridge::new(1).unwrap();
        let mut packet = Packet::default();
        for i in 0..LATENCY * 3 {
            let output = bridge.frame([0.2, 0.0], &exchange);
            if exchange.input.pop(&mut packet) {
                packet.samples.fill(0.05);
                assert!(exchange.output.push(&packet));
            }
            if i > LATENCY + HOP {
                assert!((output[0] - 0.05).abs() < 1e-6);
            }
        }
        let queue = Queue::default();
        for _ in 0..QUEUE_PACKETS {
            assert!(queue.push(&packet));
        }
        assert!(!queue.push(&packet));
    }
    #[test]
    fn disconnect_late_packets_and_restart_never_replay_old_audio() {
        let exchange = Exchange::default();
        let mut bridge = Bridge::new(1).unwrap();
        let mut packet = Packet::default();
        exchange.connected.store(true, Ordering::Release);
        for i in 0..LATENCY * 5 {
            let connected = !(LATENCY * 2..LATENCY * 3).contains(&i);
            exchange.connected.store(connected, Ordering::Release);
            if i == LATENCY * 3 {
                assert!(exchange.output.push(&Packet {
                    sequence: 0,
                    samples: [-9.0; HOP]
                }));
                assert!(exchange.output.push(&Packet {
                    sequence: u64::MAX - 1,
                    samples: [-9.0; HOP]
                }));
            }
            let output = bridge.frame([0.4, 0.0], &exchange)[0];
            if exchange.input.pop(&mut packet) {
                packet.samples.fill(0.1);
                assert!(exchange.output.push(&packet));
            }
            assert!((0.0..=0.400001).contains(&output));
            if !connected {
                assert!((output - 0.4).abs() < 1e-6);
            }
            if i > LATENCY * 4 + HOP {
                assert!((output - 0.1).abs() < 1e-6);
            }
        }
    }
    #[test]
    fn queue_retains_order_across_concurrent_wraparound() {
        let queue = std::sync::Arc::new(Queue::default());
        let writer = queue.clone();
        let worker = std::thread::spawn(move || {
            for i in 0..1_000 {
                let packet = Packet {
                    sequence: i,
                    samples: [i as f32; HOP],
                };
                while !writer.push(&packet) {
                    std::thread::yield_now();
                }
            }
        });
        let mut packet = Packet::default();
        for i in 0..1_000 {
            while !queue.pop(&mut packet) {
                std::thread::yield_now();
            }
            assert_eq!(packet.sequence, i);
            assert!(packet.samples.iter().all(|s| *s == i as f32));
        }
        worker.join().unwrap();
        for i in 0..QUEUE_PACKETS {
            assert!(queue.push(&Packet {
                sequence: i as u64,
                ..Packet::default()
            }));
        }
        queue.discard_backlog(&mut packet);
        assert!(queue.pop(&mut packet));
        assert_eq!(packet.sequence, (QUEUE_PACKETS - 1) as u64);
        assert!(!queue.pop(&mut packet));
    }
}
