//! Running independent work at once: the exporter JVMs of each side, the asset copy, hashing.
//! `HORIZON_PREPARE_JOBS=1` runs everything one step after another, as before.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

/// How many things run at once: HORIZON_PREPARE_JOBS, else the processors (at most 8).
pub(crate) fn jobs() -> usize {
    std::env::var("HORIZON_PREPARE_JOBS")
        .ok()
        .and_then(|n| n.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .min(8)
        })
}

/// `work` over every item on up to `jobs()` threads, each with its own `state` (an open jar),
/// the results in the items' order. The first item that fails, in that order, gives the error;
/// the threads stop taking items once one failed.
pub(crate) fn map<T: Sync, S, R: Send>(
    items: &[T],
    state: impl Fn() -> Result<S, String> + Sync,
    work: impl Fn(&mut S, &T) -> Result<R, String> + Sync,
) -> Result<Vec<R>, String> {
    let threads = jobs().min(items.len());
    if threads <= 1 {
        let mut state = match items.is_empty() {
            true => return Ok(Vec::new()),
            false => state()?,
        };
        return items.iter().map(|item| work(&mut state, item)).collect();
    }
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let done = Mutex::new(Vec::with_capacity(items.len()));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut results = Vec::new();
                match state() {
                    Ok(mut state) => {
                        while !failed.load(Ordering::Relaxed) {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(item) = items.get(i) else { break };
                            let result = work(&mut state, item);
                            failed.fetch_or(result.is_err(), Ordering::Relaxed);
                            results.push((i, result));
                        }
                    }
                    Err(e) => {
                        failed.store(true, Ordering::Relaxed);
                        results.push((0, Err(e)));
                    }
                }
                done.lock().unwrap().extend(results);
            });
        }
    });
    let mut done = done.into_inner().unwrap();
    done.sort_by_key(|(i, result)| (*i, result.is_ok()));
    done.into_iter().map(|(_, result)| result).collect()
}

/// One sequence of steps that needs nothing from the others.
pub(crate) type Lane<'a, R> = Box<dyn FnOnce() -> Result<R, String> + Send + 'a>;

/// Runs every lane to its end, at once unless `jobs()` is 1; the first lane's error, in the
/// lanes' order, wins. A lane that panics fails the preparation instead of the program.
pub(crate) fn lanes<R: Send>(lanes: Vec<Lane<'_, R>>) -> Result<Vec<R>, String> {
    let stopped = |_| Err("A preparation step stopped unexpectedly".to_string());
    if jobs() <= 1 {
        return lanes
            .into_iter()
            .map(|lane| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(lane)).unwrap_or_else(stopped)
            })
            .collect();
    }
    std::thread::scope(|scope| {
        let handles: Vec<_> = lanes.into_iter().map(|lane| scope.spawn(lane)).collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(stopped))
            .collect();
        results.into_iter().collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_keep_the_items_order_and_the_first_failure_wins() {
        let items: Vec<u32> = (0..500).collect();
        let squares = map(&items, || Ok(()), |_, n| Ok(n * n)).unwrap();
        assert_eq!(squares, items.iter().map(|n| n * n).collect::<Vec<_>>());
        let failure = map(
            &items,
            || Ok(()),
            |_, n| {
                if n % 7 == 3 {
                    Err(format!("bad {n}"))
                } else {
                    Ok(*n)
                }
            },
        );
        assert_eq!(failure, Err("bad 3".into()));
        assert_eq!(
            map(&[1], || Err::<(), _>("no jar".into()), |_, n| Ok(*n)),
            Err("no jar".into())
        );
        assert_eq!(
            map(
                &[] as &[u32],
                || Err::<(), _>("unused".into()),
                |_, n| Ok(*n)
            ),
            Ok(vec![])
        );
    }

    #[test]
    fn every_lane_finishes_and_the_first_error_is_reported() {
        let finished = AtomicUsize::new(0);
        let lane = |result: Result<u8, String>| -> Lane<'_, u8> {
            Box::new(|| {
                finished.fetch_add(1, Ordering::Relaxed);
                result
            })
        };
        assert_eq!(lanes(vec![lane(Ok(1)), lane(Ok(2))]), Ok(vec![1, 2]));
        let result = lanes(vec![
            lane(Ok(1)),
            lane(Err("second".into())),
            lane(Err("third".into())),
            lane(Ok(4)),
        ]);
        assert_eq!(result, Err("second".into()));
        assert_eq!(finished.load(Ordering::Relaxed), 6);
        let panics: Lane<'_, u8> = Box::new(|| panic!("exporter thread"));
        assert!(lanes(vec![lane(Ok(1)), panics]).is_err());
    }
}
