//! Benchmarks for the `dumpster` garbage collection library.

use std::{
    fmt::Display,
    rc::Rc,
    sync::{Arc, Mutex},
    thread::{self, available_parallelism, scope},
    time::{Duration, Instant},
};

use dumpster_bench::{
    ArcMultiref, BaconRajanMultiref, DumpsterSyncMultiref, DumpsterUnsyncMultiref, GcMultiref,
    Multiref, RcMultiref, ShredderMultiref, ShredderSyncMultiref, SyncMultiref,
};

struct BenchmarkData {
    name: &'static str,
    test: &'static str,
    n_threads: usize,
    n_ops: usize,
    duration: Duration,
}

impl Display for BenchmarkData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{},{},{},{},{}",
            self.name,
            self.test,
            self.n_threads,
            self.n_ops,
            self.duration.as_micros()
        )
    }
}

fn unsync_never_collect(_: &dumpster::unsync::CollectInfo) -> bool {
    false
}

fn sync_never_collect(_: &dumpster::sync::CollectInfo) -> bool {
    false
}

fn main() {
    const N_ITERS: usize = 1_000_000;
    for _ in 0..100 {
        dumpster::unsync::set_collect_condition(dumpster::unsync::default_collect_condition);
        println!(
            "{}",
            single_threaded::<dumpster::unsync::Gc<DumpsterUnsyncMultiref>>(
                "dumpster (unsync)",
                N_ITERS,
            )
        );
        dumpster::unsync::set_collect_condition(unsync_never_collect);
        println!(
            "{}",
            single_threaded::<dumpster::unsync::Gc<DumpsterUnsyncMultiref>>(
                "dumpster(unsync + manual trigger)",
                N_ITERS,
            )
        );
        dumpster::sync::set_collect_condition(dumpster::sync::default_collect_condition);
        println!(
            "{}",
            single_threaded::<dumpster::sync::Gc<DumpsterSyncMultiref>>("dumpster (sync)", N_ITERS)
        );
        dumpster::sync::set_collect_condition(sync_never_collect);
        println!(
            "{}",
            single_threaded::<dumpster::sync::Gc<DumpsterSyncMultiref>>(
                "dumpster (sync + manual trigger)",
                N_ITERS
            )
        );
        println!(
            "{}",
            single_threaded::<Arc<ArcMultiref>>("std::sync::Arc", N_ITERS)
        );
        println!("{}", single_threaded::<gc::Gc<GcMultiref>>("gc", N_ITERS));
        println!(
            "{}",
            single_threaded::<bacon_rajan_cc::Cc<BaconRajanMultiref>>("bacon-rajan-cc", N_ITERS)
        );
        println!(
            "{}",
            single_threaded::<shredder::Gc<ShredderMultiref>>("shredder", N_ITERS)
        );

        for n_threads in 1..=available_parallelism().unwrap().get() {
            // println!("--- {n_threads} threads");
            dumpster::sync::set_collect_condition(dumpster::sync::default_collect_condition);
            println!(
                "{}",
                multi_threaded::<dumpster::sync::Gc<DumpsterSyncMultiref>>(
                    "dumpster (sync)",
                    N_ITERS,
                    n_threads,
                )
            );

            dumpster::sync::set_collect_condition(sync_never_collect);
            println!(
                "{}",
                multi_threaded::<dumpster::sync::Gc<DumpsterSyncMultiref>>(
                    "dumpster (sync + manual trigger)",
                    N_ITERS,
                    n_threads,
                )
            );
            println!(
                "{}",
                multi_threaded::<shredder::Gc<ShredderSyncMultiref>>(
                    "shredder", N_ITERS, n_threads
                )
            );
        }
    }

    for _ in 0..100 {
        println!(
            "{}",
            single_threaded::<Rc<RcMultiref>>("std::rc::Rc", N_ITERS)
        );
        for n_threads in 1..available_parallelism().unwrap().get() {
            println!(
                "{}",
                multi_threaded::<Arc<ArcMultiref>>("std::sync::Arc", N_ITERS, n_threads)
            );
        }
    }
}

/// Run a benchmark of a multi-threaded garbage collector.
fn single_threaded<M: Multiref>(name: &'static str, n_iters: usize) -> BenchmarkData {
    fastrand::seed(12345);
    let mut gcs = (0..50).map(|_| M::new(Vec::new())).collect::<Vec<_>>();

    // println!("{name}: running...");
    let tic = Instant::now();
    for _n in 0..n_iters {
        // println!("iter {_n}");
        if gcs.is_empty() {
            gcs.push(M::new(Vec::new()));
        } else {
            match fastrand::u8(0..4) {
                0 => {
                    // println!("create allocation");
                    // create new allocation
                    gcs.push(M::new(Vec::new()));
                }
                1 => {
                    // println!("add reference");
                    // add a reference
                    if gcs.len() > 1 {
                        let from = fastrand::usize(0..gcs.len());
                        let to = fastrand::usize(0..gcs.len());
                        let new_gc = gcs[to].clone();
                        gcs[from].apply(|v| v.push(new_gc));
                    }
                }
                2 => {
                    // println!("remove gc");
                    // destroy a reference owned by the vector
                    gcs.swap_remove(fastrand::usize(0..gcs.len()));
                }
                3 => {
                    // println!("remove reference");
                    // destroy a reference owned by some gc
                    let from = fastrand::usize(0..gcs.len());
                    gcs[from].apply(|v| {
                        if !v.is_empty() {
                            let to = fastrand::usize(0..v.len());
                            v.swap_remove(to);
                        }
                    })
                }
                _ => unreachable!(),
            }
        }
    }
    drop(gcs);
    M::collect();
    let toc = Instant::now();
    // println!("finished {name} in {:?}", (toc - tic));
    BenchmarkData {
        name,
        test: "single_threaded",
        n_threads: 1,
        n_ops: n_iters,
        duration: toc.duration_since(tic),
    }
}

fn multi_threaded<M: SyncMultiref>(
    name: &'static str,
    n_iters: usize,
    n_threads: usize,
) -> BenchmarkData {
    let vecs: Vec<Mutex<Vec<M>>> = (0..(n_threads * 10))
        .map(|_| Mutex::new((0..50).map(|_| M::new(Vec::new())).collect()))
        .collect();

    let tic = Instant::now();
    scope(|s| {
        for i in 0..n_threads {
            let vecs = &vecs;
            thread::Builder::new()
                .name(format!("multi_threaded{i}"))
                .spawn_scoped(s, move || {
                    fastrand::seed(12345 + i as u64);

                    for _n in 0..(n_iters / n_threads) {
                        let v1_id = fastrand::usize(0..vecs.len());
                        match fastrand::u8(0..4) {
                            // create
                            0 => vecs[v1_id].lock().unwrap().push(M::new(Vec::new())),
                            // add ref
                            1 => {
                                let v2_id = fastrand::usize(0..vecs.len());
                                if v1_id == v2_id {
                                    let g1 = vecs[v1_id].lock().unwrap();
                                    if g1.len() < 2 {
                                        continue;
                                    }
                                    let i1 = fastrand::usize(0..g1.len());
                                    let i2 = fastrand::usize(0..g1.len());
                                    let new_gc = g1[i2].clone();
                                    g1[i1].apply(|v| v.push(new_gc));
                                } else {
                                    // prevent deadlock by locking lower one first
                                    let (g1, g2) = if v1_id < v2_id {
                                        (vecs[v1_id].lock().unwrap(), vecs[v2_id].lock().unwrap())
                                    } else {
                                        let g2 = vecs[v2_id].lock().unwrap();
                                        (vecs[v1_id].lock().unwrap(), g2)
                                    };
                                    if g1.is_empty() || g2.is_empty() {
                                        continue;
                                    }
                                    let i1 = fastrand::usize(0..g1.len());
                                    let i2 = fastrand::usize(0..g2.len());
                                    let new_gc = g2[i2].clone();
                                    g1[i1].apply(|v| v.push(new_gc));
                                }
                            }
                            // destroy gc
                            2 => {
                                let mut guard = vecs[v1_id].lock().unwrap();
                                if guard.is_empty() {
                                    continue;
                                }
                                let idx = fastrand::usize(0..guard.len());
                                guard.swap_remove(idx);
                            }
                            // destroy ref
                            3 => {
                                let guard = vecs[v1_id].lock().unwrap();
                                if guard.is_empty() {
                                    continue;
                                }
                                guard[fastrand::usize(0..guard.len())].apply(|v| {
                                    if !v.is_empty() {
                                        v.swap_remove(fastrand::usize(0..v.len()));
                                    }
                                });
                            }
                            _ => unreachable!(),
                        };
                    }
                })
                .unwrap();
        }
    });
    let toc = Instant::now();
    M::collect(); // This op is single threaded and shouldn't count
    let duration = toc.duration_since(tic);

    // println!("finished {name} in {duration:?}");
    BenchmarkData {
        name,
        test: "multi_threaded",
        n_threads,
        n_ops: (n_iters / n_threads) * n_threads,
        duration,
    }
}
