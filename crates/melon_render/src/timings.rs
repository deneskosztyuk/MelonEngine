//! GPU pass timings: timestamps between the passes of `render_scene`,
//! resolved into a ring of readback buffers that map after their submit
//! (never waited on), averaged over about a second into `RenderTimings`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use melon_core::RenderTimings;

/// Timestamp written after each stage of `render_scene`, in order.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Mark {
    Start,
    Shadows,
    /// Depth/normal prepass + SSAO + blur.
    Ao,
    Gi,
    /// The 3D pass (MSAA meshes, sky, lines, resolve).
    Scene,
    Bloom,
    /// After tonemap and overlay.
    End,
}

const MARKS: u32 = Mark::End as u32 + 1;
const BYTES: u64 = MARKS as u64 * 8;
/// Readback buffers in flight: the frame recording, plus up to two whose
/// results the GPU has not finished.
const RING: usize = 3;

const FREE: u8 = 0;
const PENDING: u8 = 1;
const READY: u8 = 2;

/// Which optional passes ran in a timed frame.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Ran {
    pub ao: bool,
    /// The prepass ran for SSGI alone (AO off): its cost counts as GI.
    pub prepass_for_gi: bool,
    pub gi: bool,
    pub bloom: bool,
}

/// One frame's pass times in ms: gi, ao, shadows, aa, bloom, total.
type Sample = [Option<f32>; 6];

struct Slot {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    state: Arc<AtomicU8>,
    ran: Ran,
}

/// Timestamp queries around the passes of one view per `render_scene`.
pub(crate) struct GpuTimer {
    slots: Vec<Slot>,
    /// Slot of the frame being recorded.
    recording: Option<usize>,
    /// Nanoseconds per timestamp tick.
    period: f32,
    average: TimingAverage,
}

impl GpuTimer {
    /// The features a timer needs: timestamps between passes.
    pub(crate) const FEATURES: wgpu::Features =
        wgpu::Features::TIMESTAMP_QUERY.union(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);

    /// `None` unless the device has [`Self::FEATURES`].
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(Self::FEATURES) {
            return None;
        }
        let slots = (0..RING)
            .map(|_| Slot {
                queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("gpu timer queries"),
                    ty: wgpu::QueryType::Timestamp,
                    count: MARKS,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("gpu timer resolve"),
                    size: BYTES,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("gpu timer readback"),
                    size: BYTES,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(FREE)),
                ran: Ran::default(),
            })
            .collect();
        Some(Self {
            slots,
            recording: None,
            period: queue.get_timestamp_period(),
            average: TimingAverage::default(),
        })
    }

    /// Collects finished readbacks, then starts timing this frame in a free
    /// slot (none free: this frame goes untimed).
    pub(crate) fn begin(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let now = Instant::now();
        for slot in &mut self.slots {
            if slot.state.load(Ordering::Acquire) != READY {
                continue;
            }
            let ticks: Vec<u64> = bytemuck::pod_collect_to_vec(&slot.readback.get_mapped_range(..));
            slot.readback.unmap();
            slot.state.store(FREE, Ordering::Release);
            let ms = |a: Mark, b: Mark| {
                let d = ticks[b as usize].wrapping_sub(ticks[a as usize]);
                d as f32 * self.period / 1e6
            };
            let ran = slot.ran;
            let prepass = if ran.prepass_for_gi {
                ms(Mark::Shadows, Mark::Ao)
            } else {
                0.0
            };
            self.average.push(
                now,
                [
                    ran.gi.then(|| ms(Mark::Ao, Mark::Gi) + prepass),
                    ran.ao.then(|| ms(Mark::Shadows, Mark::Ao)),
                    Some(ms(Mark::Start, Mark::Shadows)),
                    Some(ms(Mark::Gi, Mark::Scene)),
                    ran.bloom.then(|| ms(Mark::Scene, Mark::Bloom)),
                    Some(ms(Mark::Start, Mark::End)),
                ],
            );
        }
        self.recording = self
            .slots
            .iter()
            .position(|s| s.state.load(Ordering::Acquire) == FREE);
        self.mark(encoder, Mark::Start);
    }

    pub(crate) fn mark(&self, encoder: &mut wgpu::CommandEncoder, mark: Mark) {
        if let Some(i) = self.recording {
            encoder.write_timestamp(&self.slots[i].queries, mark as u32);
        }
    }

    /// Writes the end mark and queues the resolve and the readback, mapped
    /// once the GPU has run this encoder.
    // ponytail: an encoder dropped without submit keeps its slot pending for
    // good; after RING of them timing stops. Fine while every caller submits.
    pub(crate) fn finish(&mut self, encoder: &mut wgpu::CommandEncoder, ran: Ran) {
        self.mark(encoder, Mark::End);
        let Some(i) = self.recording.take() else {
            return;
        };
        let slot = &mut self.slots[i];
        slot.ran = ran;
        encoder.resolve_query_set(&slot.queries, 0..MARKS, &slot.resolve, 0);
        encoder.copy_buffer_to_buffer(&slot.resolve, 0, &slot.readback, 0, BYTES);
        slot.state.store(PENDING, Ordering::Release);
        let state = slot.state.clone();
        encoder.map_buffer_on_submit(&slot.readback, wgpu::MapMode::Read, .., move |res| {
            // A failed map frees the slot unread.
            state.store(if res.is_ok() { READY } else { FREE }, Ordering::Release);
        });
    }

    /// The averaged pass times (`budget_ms` is the host's).
    pub(crate) fn timings(&self) -> RenderTimings {
        let [gi, ao, shadows, aa, bloom, total] = self.average.current();
        RenderTimings {
            gi,
            ao,
            shadows,
            aa,
            bloom,
            total,
            budget_ms: None,
            msaa: None,
        }
    }
}

/// Averages samples over windows of [`WINDOW`]: the last full window's
/// average, or the running one until the first window is full.
#[derive(Default)]
struct TimingAverage {
    start: Option<Instant>,
    sums: [f32; 6],
    counts: [u32; 6],
    last: Option<Sample>,
}

const WINDOW: Duration = Duration::from_secs(1);

impl TimingAverage {
    fn push(&mut self, now: Instant, sample: Sample) {
        let start = *self.start.get_or_insert(now);
        for (i, ms) in sample.iter().enumerate() {
            if let Some(ms) = ms {
                self.sums[i] += ms;
                self.counts[i] += 1;
            }
        }
        if now.duration_since(start) >= WINDOW {
            self.last = Some(self.running());
            *self = Self {
                start: Some(now),
                last: self.last,
                ..Default::default()
            };
        }
    }

    /// Per pass: mean of the window's samples where it ran, else `None`.
    fn running(&self) -> Sample {
        std::array::from_fn(|i| (self.counts[i] > 0).then(|| self.sums[i] / self.counts[i] as f32))
    }

    fn current(&self) -> Sample {
        self.last.unwrap_or_else(|| self.running())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(gi: Option<f32>, total: f32) -> Sample {
        [gi, Some(1.0), Some(2.0), Some(3.0), None, Some(total)]
    }

    #[test]
    fn running_average_until_the_first_window_is_full() {
        let t0 = Instant::now();
        let mut avg = TimingAverage::default();
        assert_eq!(avg.current(), [None; 6]);
        avg.push(t0, sample(Some(1.0), 10.0));
        avg.push(t0 + Duration::from_millis(100), sample(Some(3.0), 20.0));
        assert_eq!(
            avg.current(),
            [Some(2.0), Some(1.0), Some(2.0), Some(3.0), None, Some(15.0)]
        );
    }

    #[test]
    fn full_window_is_kept_while_the_next_fills() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut avg = TimingAverage::default();
        avg.push(ms(0), sample(Some(1.0), 10.0));
        avg.push(ms(1000), sample(Some(3.0), 30.0));
        assert_eq!(avg.current()[5], Some(20.0), "window of 2 samples");
        avg.push(ms(1500), sample(None, 100.0));
        assert_eq!(avg.current()[5], Some(20.0), "next window not full yet");
        avg.push(ms(2000), sample(None, 200.0));
        let next = avg.current();
        assert_eq!(next[5], Some(150.0));
        assert_eq!(next[0], None, "GI did not run in that window");
    }

    #[test]
    fn marks_are_in_frame_order() {
        let marks = [
            Mark::Start,
            Mark::Shadows,
            Mark::Ao,
            Mark::Gi,
            Mark::Scene,
            Mark::Bloom,
            Mark::End,
        ];
        assert!(marks.iter().enumerate().all(|(i, &m)| m as usize == i));
        assert_eq!(MARKS, 7);
    }
}
