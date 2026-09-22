//! Bus load: how much of a CAN bus's capacity the traffic on it is using.
//!
//! Load is the share of wire time frames occupy, so it is computed from *bits*, not from a
//! frame count — a bus carrying a hundred eight-byte extended frames a second is far busier
//! than one carrying a hundred remote frames, and counting frames says they are the same.
//!
//! What this can and cannot know is worth stating plainly, because a load figure invites more
//! trust than it deserves. Frame lengths here are nominal: bit stuffing adds bits that depend
//! on the payload's bit pattern and on a CRC these frame records do not carry, so a measured
//! load is a **floor**. The worst-case stuffing is tracked alongside it, which turns one
//! number that is subtly wrong into a range that is honestly right.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::VecDeque;

use crate::CanFrame;

/// How long each bucket of the series covers.
pub const c_f64BucketSeconds: f64 = 1.0;

/// How many buckets to keep. Two minutes at one second each — long enough to see a flash
/// transfer's shape, short enough to stay cheap to hold and to draw.
pub const c_uMaxBuckets: usize = 120;

/// One second of bus activity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BusLoadSample {
    /// When this bucket began, in the same clock the frames are stamped with.
    pub m_f64AtSec: f64,
    /// Frames counted in it.
    pub m_uFrames: usize,
    /// Share of the bus the traffic occupied at minimum, 0.0 to 1.0, stuffing excluded.
    pub m_f64LoadNominal: f64,
    /// The same with every frame stuffed as heavily as the standard allows — the other end of
    /// the range the real figure lies in.
    pub m_f64LoadWorstCase: f64,
}

/// Accumulates frames into per-second buckets and reports the recent history.
///
/// Fed from the bridge, which sees every frame crossing the link in both directions. That is
/// the honest scope: it measures the traffic this engine can observe, which on a real adapter
/// is the whole bus and on a virtual link is only what crosses it.
#[derive(Debug)]
pub struct BusLoadMeter {
    m_u32BitrateBps: u32,
    m_dequeBuckets: VecDeque<Bucket>,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    m_f64StartedAtSec: f64,
    m_uFrames: usize,
    m_uBitsNominal: usize,
    m_uBitsWorstCase: usize,
}

impl BusLoadMeter {
    /// A meter for a bus running at this bitrate.
    pub fn New(u32BitrateBps: u32) -> Self {
        BusLoadMeter {
            m_u32BitrateBps: u32BitrateBps,
            m_dequeBuckets: VecDeque::with_capacity(c_uMaxBuckets),
        }
    }

    /// The bitrate load is measured against.
    pub fn BitrateBps(&self) -> u32 {
        self.m_u32BitrateBps
    }

    /// Count one frame at the time it crossed the bus.
    pub fn Record(&mut self, frame: &CanFrame) {
        let uNominal = frame.BitsOnWire();
        let uWorstCase = uNominal + frame.MaxStuffBits();
        let f64BucketStart =
            (frame.m_f64TimestampSec / c_f64BucketSeconds).floor() * c_f64BucketSeconds;

        // Frames arrive in time order, so the bucket being filled is the last one. A frame
        // stamped earlier than that — a clock that stepped back, a replayed capture — is
        // counted into the current bucket rather than reopening a closed one, which would put
        // a spike in the middle of a series already drawn.
        match self.m_dequeBuckets.back_mut() {
            Some(bucket) if bucket.m_f64StartedAtSec >= f64BucketStart => {
                bucket.m_uFrames += 1;
                bucket.m_uBitsNominal += uNominal;
                bucket.m_uBitsWorstCase += uWorstCase;
                return;
            }
            _ => {}
        }

        self.m_dequeBuckets.push_back(Bucket {
            m_f64StartedAtSec: f64BucketStart,
            m_uFrames: 1,
            m_uBitsNominal: uNominal,
            m_uBitsWorstCase: uWorstCase,
        });
        while self.m_dequeBuckets.len() > c_uMaxBuckets {
            self.m_dequeBuckets.pop_front();
        }
    }

    /// The recent history, oldest first.
    pub fn Samples(&self) -> Vec<BusLoadSample> {
        self.m_dequeBuckets
            .iter()
            .map(|bucket| self.SampleOf(bucket))
            .collect()
    }

    /// The bucket currently being filled, or `None` when nothing has crossed the bus yet.
    ///
    /// Reported separately because it is incomplete by definition: a bucket a tenth of a second
    /// old reads as a tenth of the load it will finish at, and showing that next to finished
    /// seconds as though it were one of them makes every live reading look like a collapse.
    pub fn Current(&self) -> Option<BusLoadSample> {
        self.m_dequeBuckets
            .back()
            .map(|bucket| self.SampleOf(bucket))
    }

    /// The highest finished bucket in the history, which is what a sizing question asks about.
    pub fn Peak(&self) -> Option<BusLoadSample> {
        self.m_dequeBuckets
            .iter()
            .take(self.m_dequeBuckets.len().saturating_sub(1))
            .map(|bucket| self.SampleOf(bucket))
            .max_by(|left, right| {
                left.m_f64LoadNominal
                    .partial_cmp(&right.m_f64LoadNominal)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    fn SampleOf(&self, bucket: &Bucket) -> BusLoadSample {
        let f64Capacity = (self.m_u32BitrateBps as f64) * c_f64BucketSeconds;
        let Divide = |uBits: usize| {
            if f64Capacity <= 0.0 {
                0.0
            } else {
                (uBits as f64 / f64Capacity).min(1.0)
            }
        };

        BusLoadSample {
            m_f64AtSec: bucket.m_f64StartedAtSec,
            m_uFrames: bucket.m_uFrames,
            m_f64LoadNominal: Divide(bucket.m_uBitsNominal),
            m_f64LoadWorstCase: Divide(bucket.m_uBitsWorstCase),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn DataFrame(f64At: f64, u32CanId: u32, uBytes: usize) -> CanFrame {
        CanFrame::NewClassic(f64At, u32CanId, vec![0xAA; uBytes])
    }

    #[test]
    fn a_standard_eight_byte_frame_is_one_hundred_and_eleven_bits() {
        // ISO 11898-1 clause 10.4, counted field by field: 47 bits of frame around 64 of data.
        let frame = DataFrame(0.0, 0x7E0, 8);
        assert_eq!(frame.BitsOnWire(), 111);

        // Extended adds SRR, IDE, the 18-bit identifier extension and a reserved bit.
        let extended = DataFrame(0.0, 0x18DAF110, 8);
        assert_eq!(extended.BitsOnWire(), 131);
    }

    #[test]
    fn a_remote_frame_carries_a_length_but_no_data_bits() {
        let remote = CanFrame::NewRemote(0.0, 0x7E0, 8);
        assert_eq!(
            remote.BitsOnWire(),
            47,
            "a remote frame asks for eight bytes and carries none of them"
        );
        assert_eq!(
            remote.DataLengthCode(),
            8,
            "the length it asks for survives"
        );
        assert!(remote.m_vecData.is_empty());
    }

    #[test]
    fn load_is_the_share_of_wire_time_not_the_frame_count() {
        // 500 kbit/s. A hundred 8-byte standard frames is 11,100 bits; a hundred remote frames
        // is 4,700. Counting frames would call these the same bus.
        let mut meterData = BusLoadMeter::New(500_000);
        let mut meterRemote = BusLoadMeter::New(500_000);
        for uIndex in 0..100 {
            let f64At = uIndex as f64 * 0.001;
            meterData.Record(&DataFrame(f64At, 0x7E0, 8));
            meterRemote.Record(&CanFrame::NewRemote(f64At, 0x7E0, 8));
        }

        let data = meterData.Current().expect("a bucket");
        let remote = meterRemote.Current().expect("a bucket");
        assert_eq!(data.m_uFrames, remote.m_uFrames, "same number of frames");
        assert!(
            data.m_f64LoadNominal > remote.m_f64LoadNominal * 2.0,
            "and very different load: {:.4} against {:.4}",
            data.m_f64LoadNominal,
            remote.m_f64LoadNominal
        );
        // 11,100 bits of a 500,000-bit second.
        assert!((data.m_f64LoadNominal - 0.0222).abs() < 0.0005);
    }

    #[test]
    fn the_worst_case_bounds_the_nominal_rather_than_replacing_it() {
        // Stuffing cannot be computed from a decoded frame, so the answer is a range. It must
        // be a range that contains the truth: never below the nominal, never above capacity.
        let mut meter = BusLoadMeter::New(500_000);
        for uIndex in 0..1000 {
            meter.Record(&DataFrame(uIndex as f64 * 0.0001, 0x18DAF110, 8));
        }

        let sample = meter.Current().expect("a bucket");
        assert!(sample.m_f64LoadWorstCase > sample.m_f64LoadNominal);
        assert!(
            sample.m_f64LoadWorstCase <= 1.0,
            "load can never exceed the bus"
        );
    }

    #[test]
    fn buckets_are_one_second_and_the_history_is_bounded() {
        let mut meter = BusLoadMeter::New(500_000);
        // Three hundred seconds of traffic into a meter that keeps 120 buckets.
        for uSecond in 0..300 {
            meter.Record(&DataFrame(uSecond as f64 + 0.5, 0x7E0, 8));
        }

        let vecSamples = meter.Samples();
        assert_eq!(vecSamples.len(), c_uMaxBuckets, "the oldest are dropped");
        assert_eq!(
            vecSamples.last().expect("a sample").m_f64AtSec,
            299.0,
            "and the newest is kept"
        );
        assert!(vecSamples
            .windows(2)
            .all(|pair| pair[0].m_f64AtSec < pair[1].m_f64AtSec));
    }

    #[test]
    fn an_idle_bus_reads_as_idle_rather_than_as_nothing() {
        let meter = BusLoadMeter::New(500_000);
        assert!(meter.Samples().is_empty());
        assert_eq!(meter.Current(), None);
        assert_eq!(meter.Peak(), None);
    }
}
