//! Untrusted source text through the whole engine (task 1.9): the
//! embedding API compiles it and runs it on the real interpreter with a
//! small heap and stack and a `print` host function. Every outcome is
//! fine except a panic or a hang: a watchdog sets the interrupt flag after
//! 100 ms, so infinite loops end as `Terminated`.
#![no_main]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ferrojs::{Engine, JsValue, Options};
use libfuzzer_sys::fuzz_target;

/// The running input's interrupt flag and start time.
type Watched = Mutex<Option<(Arc<AtomicBool>, Instant)>>;

fn watchdog() -> &'static Watched {
    static W: OnceLock<Watched> = OnceLock::new();
    W.get_or_init(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_millis(10));
            if let Some((flag, start)) = &*watchdog().lock().unwrap() {
                if start.elapsed() > Duration::from_millis(100) {
                    flag.store(true, Ordering::Relaxed);
                }
            }
        });
        Mutex::new(None)
    })
}

fuzz_target!(|data: &[u8]| {
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    let mut engine = Engine::with_options(Options {
        heap_size: 8 * 256 * 1024,
        stack_slots: 1 << 14,
        gc_stress: false,
    })
    .expect("engine");
    engine
        .register("print", |args| {
            // Exercise the conversion without writing anything.
            let _ = args.first().map(JsValue::to_string);
            Ok(JsValue::Undefined)
        })
        .expect("print");
    *watchdog().lock().unwrap() = Some((engine.interrupt_handle(), Instant::now()));
    let _ = engine.run(source);
    *watchdog().lock().unwrap() = None;
});
