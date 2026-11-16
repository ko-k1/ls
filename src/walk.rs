//! Parallel directory traversal with serial, in-order output.
//!
//! Listing a directory (reading, stat'ing, sorting and rendering it) only
//! depends on that directory, so worker threads list directories ahead of
//! the main thread. The main thread visits directories in exactly the order
//! a serial `ls -R` would, takes each finished listing, and writes it. The
//! output is therefore identical to a serial traversal.
//!
//! Scheduling:
//! - Every directory waiting to be listed is keyed by its path of child
//!   indices from the roots (`[0, 3, 1]`). Sorted, these keys are depth-first
//!   preorder, so workers always pick the pending directory the main thread
//!   will need soonest.
//! - At most `limit` directories are claimed by workers but not yet written;
//!   a worker waits rather than run further ahead, bounding memory.
//! - When the main thread reaches a directory nobody has started, it lists it
//!   itself. It only ever waits on a directory a worker is actively listing,
//!   so the traversal cannot deadlock.
//! - Worker threads start lazily, no more of them than there is pending
//!   work, so small trees stay single-threaded.

use std::collections::BTreeMap;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::lister::{list_dir, DirListing, Settings};

/// Listings each worker may hold ahead of the main thread.
const AHEAD_PER_WORKER: usize = 8;

/// List `roots` (and with `-R`, everything below them), passing each listing
/// to `emit` in depth-first order. Stops early when `emit` returns `false`.
pub fn walk(
    settings: &Settings,
    roots: Vec<PathBuf>,
    mut emit: impl FnMut(&Path, DirListing) -> bool,
) {
    let max_workers = thread::available_parallelism().map_or(1, |count| count.get());
    let walk = Walk {
        settings,
        queue: Mutex::new(Queue::default()),
        wake: Condvar::new(),
        limit: max_workers * AHEAD_PER_WORKER,
    };

    let roots: Vec<Arc<Node>> = (0..)
        .zip(roots)
        .map(|(index, path)| Node::new(path, vec![index], true))
        .collect();
    walk.enqueue(&roots);

    thread::scope(|scope| {
        let mut workers = 0;
        // With several operands, start workers on all but the first before
        // the main thread lists it, so a large first directory (operands are
        // sorted, so it is not necessarily the first given) does not delay
        // the others.
        while workers < max_workers && workers + 1 < roots.len() {
            scope.spawn(|| walk.work());
            workers += 1;
        }
        let mut stack: Vec<Arc<Node>> = roots.into_iter().rev().collect();
        while let Some(node) = stack.pop() {
            let (listing, children) = walk.take(&node);

            // Start workers for queued work before writing this listing, so
            // they list ahead while the main thread is busy with output.
            let pending = walk.lock_queue().pending.len();
            while workers < max_workers && workers < pending {
                scope.spawn(|| walk.work());
                workers += 1;
            }

            if !emit(&node.path, listing) {
                break;
            }
            stack.extend(children.into_iter().rev());
        }

        walk.lock_queue().stop = true;
        walk.wake.notify_all();
    });
}

struct Walk<'s> {
    settings: &'s Settings,
    queue: Mutex<Queue>,
    /// Signalled when work is queued, a listing is written, or on stop.
    wake: Condvar,
    limit: usize,
}

#[derive(Default)]
struct Queue {
    /// Directories waiting to be listed, in depth-first order. Entries the
    /// main thread has since listed itself may linger; workers skip them.
    pending: BTreeMap<Vec<u32>, Arc<Node>>,
    /// Directories claimed by workers and not yet taken by the main thread.
    ahead: usize,
    stop: bool,
}

struct Node {
    path: PathBuf,
    /// Position in depth-first order (see the module documentation).
    key: Vec<u32>,
    /// Command-line operands report failures as serious (exit status 2).
    serious: bool,
    state: Mutex<State>,
    ready: Condvar,
}

enum State {
    Pending,
    Listing,
    Ready {
        listing: DirListing,
        children: Vec<Arc<Node>>,
    },
    Taken,
}

impl Node {
    fn new(path: PathBuf, key: Vec<u32>, serious: bool) -> Arc<Self> {
        Arc::new(Self {
            path,
            key,
            serious,
            state: Mutex::new(State::Pending),
            ready: Condvar::new(),
        })
    }
}

impl Walk<'_> {
    fn lock_queue(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().expect("walk queue poisoned")
    }

    fn enqueue(&self, nodes: &[Arc<Node>]) {
        if nodes.is_empty() {
            return;
        }
        let mut queue = self.lock_queue();
        for node in nodes {
            queue.pending.insert(node.key.clone(), Arc::clone(node));
        }
        drop(queue);
        self.wake.notify_all();
    }

    /// List one directory and queue its subdirectories.
    fn list(&self, node: &Node) -> (DirListing, Vec<Arc<Node>>) {
        let mut listing = list_dir(self.settings, &node.path, node.serious);
        let children: Vec<Arc<Node>> = (0..)
            .zip(mem::take(&mut listing.subdirs))
            .map(|(index, path)| {
                let mut key = node.key.clone();
                key.push(index);
                Node::new(path, key, false)
            })
            .collect();
        self.enqueue(&children);
        (listing, children)
    }

    /// Main thread: get `node`'s listing, listing it here if no worker has
    /// started it, or waiting for the worker that has.
    fn take(&self, node: &Node) -> (DirListing, Vec<Arc<Node>>) {
        let mut state = node.state.lock().expect("walk node poisoned");
        loop {
            match mem::replace(&mut *state, State::Taken) {
                State::Pending => {
                    *state = State::Listing;
                    drop(state);
                    self.lock_queue().pending.remove(&node.key);
                    let result = self.list(node);
                    *node.state.lock().expect("walk node poisoned") = State::Taken;
                    return result;
                }
                State::Listing => {
                    *state = State::Listing;
                    state = node.ready.wait(state).expect("walk node poisoned");
                }
                State::Ready { listing, children } => {
                    drop(state);
                    self.lock_queue().ahead -= 1;
                    self.wake.notify_all();
                    return (listing, children);
                }
                State::Taken => unreachable!("directory listed twice"),
            }
        }
    }

    /// Worker thread: list pending directories, nearest first, until stopped.
    fn work(&self) {
        loop {
            let node = {
                let mut queue = self.lock_queue();
                loop {
                    if queue.stop {
                        return;
                    }
                    if queue.ahead < self.limit {
                        if let Some((_, node)) = queue.pending.pop_first() {
                            queue.ahead += 1;
                            break node;
                        }
                    }
                    queue = self.wake.wait(queue).expect("walk queue poisoned");
                }
            };

            let claimed = {
                let mut state = node.state.lock().expect("walk node poisoned");
                let pending = matches!(*state, State::Pending);
                if pending {
                    *state = State::Listing;
                }
                pending
            };
            if !claimed {
                // The main thread got to it first.
                self.lock_queue().ahead -= 1;
                self.wake.notify_all();
                continue;
            }

            let (listing, children) = self.list(&node);
            *node.state.lock().expect("walk node poisoned") = State::Ready { listing, children };
            node.ready.notify_all();
        }
    }
}
