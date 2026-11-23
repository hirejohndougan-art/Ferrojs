//! Host hooks (task 1.11, D-008, D-041).

use ferrojs_vm::host::{DeterministicHost, Host, JobId, SystemHost, XorShift128Plus};
use ferrojs_vm::{Vm, VmOptions};

#[test]
fn random_numbers_are_in_range_and_spread_out() {
    let mut hosts: Vec<Box<dyn Host>> = vec![
        Box::new(SystemHost::new()),
        Box::new(DeterministicHost::default()),
    ];
    for h in &mut hosts {
        let mut buckets = [0u32; 10];
        for _ in 0..100_000 {
            let r = h.random();
            assert!((0.0..1.0).contains(&r), "{r}");
            assert_eq!((r * 2f64.powi(53)).fract(), 0.0, "a multiple of 2^-53");
            buckets[(r * 10.0) as usize] += 1;
        }
        // Each tenth gets 10% give or take 1% (a ~10-sigma margin).
        for (i, &b) in buckets.iter().enumerate() {
            assert!((9_000..11_000).contains(&b), "bucket {i}: {b}");
        }
    }
}

#[test]
fn deterministic_hosts_repeat_themselves() {
    let run = || {
        let mut h = DeterministicHost::new(42, 1_000.0, 5.0);
        let r: Vec<f64> = (0..5).map(|_| h.random()).collect();
        let t: Vec<f64> = (0..3).map(|_| h.now()).collect();
        let m: Vec<f64> = (0..3).map(|_| h.monotonic_now()).collect();
        (r, t, m)
    };
    let (r, t, m) = run();
    assert_eq!(run(), (r.clone(), t.clone(), m.clone()));
    assert_eq!(t, vec![1_000.0, 1_005.0, 1_010.0]);
    assert_eq!(m, vec![0.0, 5.0, 10.0]);
    // Different seeds differ.
    let mut other = DeterministicHost::new(43, 0.0, 1.0);
    assert_ne!(other.random(), r[0]);
    // The zero seed still produces varied numbers.
    let mut z = XorShift128Plus::new(0);
    let (a, b) = (z.next_u64(), z.next_u64());
    assert!(a != 0 && b != 0 && a != b);
}

#[test]
fn system_clocks_are_sane() {
    let mut h = SystemHost::new();
    let now = h.now();
    // After 2020-01-01 and a whole number of milliseconds.
    assert!(now > 1_577_836_800_000.0);
    assert_eq!(now.fract(), 0.0);
    let a = h.monotonic_now();
    let b = h.monotonic_now();
    assert!(b >= a && a >= 0.0);
    assert_eq!(h.local_offset(now), 0.0);
}

#[test]
fn jobs_run_in_the_order_the_host_picks() {
    let mut fifo = DeterministicHost::default();
    for i in 0..3 {
        fifo.enqueue_job(JobId(i));
    }
    let order: Vec<u64> = std::iter::from_fn(|| fifo.next_job())
        .map(|j| j.0)
        .collect();
    assert_eq!(order, [0, 1, 2]);

    // An embedder can choose another order, and the engine uses it.
    #[derive(Default)]
    struct Lifo(Vec<JobId>);
    impl Host for Lifo {
        fn random(&mut self) -> f64 {
            0.5
        }
        fn now(&mut self) -> f64 {
            0.0
        }
        fn monotonic_now(&mut self) -> f64 {
            0.0
        }
        fn enqueue_job(&mut self, job: JobId) {
            self.0.push(job);
        }
        fn next_job(&mut self) -> Option<JobId> {
            self.0.pop()
        }
    }
    let mut vm = Vm::with_host(VmOptions::default(), Box::new(Lifo::default())).unwrap();
    for i in 0..3 {
        vm.host.enqueue_job(JobId(i));
    }
    let order: Vec<u64> = std::iter::from_fn(|| vm.host.next_job())
        .map(|j| j.0)
        .collect();
    assert_eq!(order, [2, 1, 0]);
    assert_eq!(vm.host.random(), 0.5);
}
