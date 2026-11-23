//! Host hooks: every source of nondeterminism the engine uses comes from
//! the embedder (D-008, D-041), so a deterministic run is a matter of
//! configuration.
//!
//! - [`Host::random`] backs `Math.random`.
//! - [`Host::now`] backs `Date.now()` and `new Date()`;
//!   [`Host::local_offset`] backs local-time conversions (LocalTZA).
//! - [`Host::monotonic_now`] backs `performance.now()`-style timers.
//! - [`Host::enqueue_job`] and [`Host::next_job`] decide the order in
//!   which queued jobs (promise reactions, task 1.14) run. The engine keeps
//!   the jobs themselves (they hold heap references); the host only sees
//!   opaque [`JobId`]s.
//!
//! [`SystemHost`] is the default: system clocks, a randomly seeded
//! generator and first-in-first-out jobs. [`DeterministicHost`] gives the
//! same results every run: a seeded generator, clocks that start at a
//! fixed time and advance by a fixed step per reading, UTC, FIFO jobs.

use std::collections::VecDeque;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// An opaque handle to a job the engine keeps.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct JobId(pub u64);

/// What the engine asks of its embedder.
pub trait Host {
    /// A number in `[0, 1)`, for `Math.random`. Need not be
    /// cryptographically secure (ECMA-262 §21.3.2.27).
    fn random(&mut self) -> f64;

    /// The current time in milliseconds since 1970-01-01T00:00:00Z
    /// (a time value, ECMA-262 §21.4.1.1), for `Date`.
    fn now(&mut self) -> f64;

    /// The local time zone's offset from UTC in milliseconds at time value
    /// `t` (UTC), daylight saving included: LocalTZA(t, true).
    fn local_offset(&mut self, t: f64) -> f64 {
        let _ = t;
        0.0
    }

    /// Milliseconds since an arbitrary fixed point, never going backward,
    /// for timers.
    fn monotonic_now(&mut self) -> f64;

    /// Queues a job (HostEnqueuePromiseJob and friends).
    fn enqueue_job(&mut self, job: JobId);

    /// The job to run next, if any. The default hosts run jobs in the
    /// order they were queued.
    fn next_job(&mut self) -> Option<JobId>;
}

/// xorshift128+: fast, small, and plenty for `Math.random`. The output
/// takes the top 53 bits, so every value is a multiple of 2^-53 in
/// `[0, 1)`.
#[derive(Clone, Debug)]
pub struct XorShift128Plus {
    s0: u64,
    s1: u64,
}

impl XorShift128Plus {
    /// A generator from a seed (any value; zero is fixed up, since the
    /// all-zero state would only produce zeros).
    pub fn new(seed: u64) -> XorShift128Plus {
        // splitmix64 spreads the seed over both state words.
        let mut z = seed;
        let mut next = || {
            z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            x ^ (x >> 31)
        };
        let (s0, s1) = (next(), next());
        if s0 == 0 && s1 == 0 {
            XorShift128Plus { s0: 1, s1: 0 }
        } else {
            XorShift128Plus { s0, s1 }
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.s0;
        let y = self.s1;
        self.s0 = y;
        x ^= x << 23;
        x ^= x >> 17;
        x ^= y ^ (y >> 26);
        self.s1 = x;
        x.wrapping_add(y)
    }

    /// A number in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

/// The default host: system clocks, a generator seeded from the system's
/// hashing entropy, FIFO jobs, and a zero local offset (time zones come
/// with the `Date` built-in, Phase 2).
#[derive(Debug)]
pub struct SystemHost {
    rng: XorShift128Plus,
    start: Instant,
    jobs: VecDeque<JobId>,
}

impl SystemHost {
    pub fn new() -> SystemHost {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        SystemHost {
            rng: XorShift128Plus::new(h.finish()),
            start: Instant::now(),
            jobs: VecDeque::new(),
        }
    }
}

impl Default for SystemHost {
    fn default() -> SystemHost {
        SystemHost::new()
    }
}

impl Host for SystemHost {
    fn random(&mut self) -> f64 {
        self.rng.next_f64()
    }

    fn now(&mut self) -> f64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            // Whole milliseconds, like other engines' Date.now().
            Ok(d) => d.as_millis() as f64,
            Err(e) => -(e.duration().as_millis() as f64),
        }
    }

    fn monotonic_now(&mut self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    fn enqueue_job(&mut self, job: JobId) {
        self.jobs.push_back(job);
    }

    fn next_job(&mut self) -> Option<JobId> {
        self.jobs.pop_front()
    }
}

/// A host that gives the same results on every run: for tests, the
/// differential harness and reproducing bugs.
#[derive(Clone, Debug)]
pub struct DeterministicHost {
    rng: XorShift128Plus,
    time: f64,
    step: f64,
    monotonic: f64,
    jobs: VecDeque<JobId>,
}

impl DeterministicHost {
    /// Random numbers from `seed`; `now` starts at `start` (a time value)
    /// and every clock reading advances both clocks by `step` ms.
    pub fn new(seed: u64, start: f64, step: f64) -> DeterministicHost {
        DeterministicHost {
            rng: XorShift128Plus::new(seed),
            time: start,
            step,
            monotonic: 0.0,
            jobs: VecDeque::new(),
        }
    }
}

impl Default for DeterministicHost {
    /// Seed 0, starting at 2000-01-01T00:00:00Z, 1 ms per reading.
    fn default() -> DeterministicHost {
        DeterministicHost::new(0, 946_684_800_000.0, 1.0)
    }
}

impl Host for DeterministicHost {
    fn random(&mut self) -> f64 {
        self.rng.next_f64()
    }

    fn now(&mut self) -> f64 {
        let t = self.time;
        self.time += self.step;
        t
    }

    fn monotonic_now(&mut self) -> f64 {
        let t = self.monotonic;
        self.monotonic += self.step;
        t
    }

    fn enqueue_job(&mut self, job: JobId) {
        self.jobs.push_back(job);
    }

    fn next_job(&mut self) -> Option<JobId> {
        self.jobs.pop_front()
    }
}
