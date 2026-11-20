//! The event loop: one thread, waiting on `epoll`.
//!
//! Every source the compositor watches is a descriptor registered here with a
//! [`Token`] that says what it is. The loop hands back the tokens that are
//! ready, and the compositor decides what each means. No source holds a
//! callback, so nothing here borrows the compositor while it runs.
//!
//! Timers live in the loop itself, as deadlines in a heap. The loop waits
//! until the next one, so a still compositor wakes on nothing.

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashSet, VecDeque},
    io,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rustix::{
    event::{EventfdFlags, Timespec, epoll, eventfd},
    io::Errno,
};

/// What a ready descriptor is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Token {
    /// A path socket clients connect to, by its index.
    Listener(usize),
    /// A connected client.
    Client(u64),
    /// Work handed to the compositor from another thread.
    Waker,
    /// A descriptor the backend watches, by the backend's own number.
    Backend(u64),
    /// The shell's control channel's listener.
    ControlListener,
    /// A control connection.
    Control(u64),
    /// A surface's commit held for its acquire point.
    Held(u64),
    /// The unlock authority's listener (see `lock.rs`).
    AuthorityListener,
}

impl Token {
    fn encode(self) -> u64 {
        let (kind, value) = match self {
            Token::Listener(index) => (1, index as u64),
            Token::Client(id) => (2, id),
            Token::Waker => (5, 0),
            Token::Backend(source) => (6, source),
            Token::ControlListener => (7, 0),
            Token::Control(id) => (8, id),
            Token::Held(surface) => (9, surface),
            Token::AuthorityListener => (10, 0),
        };
        debug_assert!(value < 1 << 56);
        (kind << 56) | value
    }

    fn decode(data: u64) -> Option<Self> {
        let value = data & ((1 << 56) - 1);
        Some(match data >> 56 {
            1 => Token::Listener(value as usize),
            2 => Token::Client(value),
            5 => Token::Waker,
            6 => Token::Backend(value),
            7 => Token::ControlListener,
            8 => Token::Control(value),
            9 => Token::Held(value),
            10 => Token::AuthorityListener,
            _ => return None,
        })
    }
}

/// What a descriptor is ready for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Readiness {
    pub readable: bool,
    pub writable: bool,
    /// The peer hung up, or the descriptor failed.
    pub hung_up: bool,
}

/// A timer the compositor set. It names what to do when it runs out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimerKey {
    /// Listen again on a path socket that could not accept.
    RetryListener(usize),
    /// One frame time after the draw with this number on this output, for a
    /// backend that does not hear when the display shows a frame.
    FrameWait(u64, u64),
    /// The session may have been idle long enough to lock (see `lock.rs`).
    IdleLock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimerId(u64);

/// What one wait found: the ready descriptors, and the timers that ran out.
pub type Woken = (Vec<(Token, Readiness)>, Vec<TimerKey>);

pub struct EventLoop {
    epoll: OwnedFd,
    events: Vec<epoll::Event>,
    timers: BinaryHeap<Reverse<(Instant, TimerId)>>,
    keys: std::collections::HashMap<TimerId, TimerKey>,
    cancelled: HashSet<TimerId>,
    next_timer: u64,
    waker: Arc<Waker>,
}

/// Work from another thread, run on the compositor's.
pub type Job<S> = Box<dyn FnOnce(&mut S) + Send>;

/// Wakes the loop from another thread. Only tests and signal handling need
/// it.
pub struct Waker {
    fd: OwnedFd,
}

impl Waker {
    pub fn wake(&self) {
        let _ = rustix::io::write(&self.fd, &1u64.to_ne_bytes());
    }

    fn drain(&self) {
        let mut buffer = [0u8; 8];
        let _ = rustix::io::read(&self.fd, &mut buffer);
    }
}

/// Jobs for the compositor's thread, and the waker that tells it they wait.
pub struct Remote<S> {
    jobs: Arc<Mutex<VecDeque<Job<S>>>>,
    waker: Arc<Waker>,
}

impl<S> Clone for Remote<S> {
    fn clone(&self) -> Self {
        Remote {
            jobs: self.jobs.clone(),
            waker: self.waker.clone(),
        }
    }
}

impl<S> Remote<S> {
    pub fn new(event_loop: &EventLoop) -> Self {
        Remote {
            jobs: Arc::default(),
            waker: event_loop.waker.clone(),
        }
    }

    pub fn send(&self, job: Job<S>) {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_back(job);
        self.waker.wake();
    }

    /// The jobs that wait, taken.
    pub fn take(&self) -> VecDeque<Job<S>> {
        std::mem::take(&mut *self.jobs.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
    }
}

impl EventLoop {
    pub fn new() -> io::Result<Self> {
        let epoll = epoll::create(epoll::CreateFlags::CLOEXEC)?;
        let waker = Arc::new(Waker {
            fd: eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)?,
        });
        let event_loop = EventLoop {
            epoll,
            events: Vec::with_capacity(64),
            timers: BinaryHeap::new(),
            keys: Default::default(),
            cancelled: HashSet::new(),
            next_timer: 0,
            waker,
        };
        event_loop.add(event_loop.waker.fd.as_fd(), Token::Waker, false)?;
        Ok(event_loop)
    }

    fn flags(writable: bool) -> epoll::EventFlags {
        let mut flags = epoll::EventFlags::IN | epoll::EventFlags::RDHUP;
        if writable {
            flags |= epoll::EventFlags::OUT;
        }
        flags
    }

    /// Watches a descriptor. It is reported readable, and writable too if
    /// `writable`.
    pub fn add(&self, fd: BorrowedFd<'_>, token: Token, writable: bool) -> io::Result<()> {
        epoll::add(
            &self.epoll,
            fd,
            epoll::EventData::new_u64(token.encode()),
            Self::flags(writable),
        )?;
        Ok(())
    }

    /// Changes whether a watched descriptor is reported writable.
    pub fn set_writable(&self, fd: BorrowedFd<'_>, token: Token, writable: bool) -> io::Result<()> {
        epoll::modify(
            &self.epoll,
            fd,
            epoll::EventData::new_u64(token.encode()),
            Self::flags(writable),
        )?;
        Ok(())
    }

    /// Stops watching a descriptor.
    pub fn remove(&self, fd: BorrowedFd<'_>) {
        let _ = epoll::delete(&self.epoll, fd);
    }

    /// Sets a timer that runs out at `deadline`.
    pub fn set_timer(&mut self, deadline: Instant, key: TimerKey) -> TimerId {
        self.next_timer += 1;
        let id = TimerId(self.next_timer);
        self.timers.push(Reverse((deadline, id)));
        self.keys.insert(id, key);
        id
    }

    pub fn cancel_timer(&mut self, id: TimerId) {
        if self.keys.remove(&id).is_some() {
            self.cancelled.insert(id);
        }
    }

    /// The timers that have run out, earliest first.
    fn due_timers(&mut self, now: Instant) -> Vec<TimerKey> {
        let mut due = Vec::new();
        while let Some(Reverse((deadline, id))) = self.timers.peek().copied() {
            if deadline > now {
                break;
            }
            self.timers.pop();
            if self.cancelled.remove(&id) {
                continue;
            }
            if let Some(key) = self.keys.remove(&id) {
                due.push(key);
            }
        }
        due
    }

    fn next_deadline(&mut self) -> Option<Instant> {
        while let Some(Reverse((deadline, id))) = self.timers.peek().copied() {
            if self.cancelled.remove(&id) {
                self.timers.pop();
                continue;
            }
            return Some(deadline);
        }
        None
    }

    /// Waits for descriptors or a timer, for at most `limit`. Returns the
    /// ready descriptors and the timers that ran out.
    pub fn wait(&mut self, limit: Option<Duration>) -> io::Result<Woken> {
        let now = Instant::now();
        let until_timer = self
            .next_deadline()
            .map(|deadline| deadline.saturating_duration_since(now));
        let timeout = match (limit, until_timer) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let timespec = timeout.map(|timeout| Timespec {
            tv_sec: timeout.as_secs() as _,
            tv_nsec: i64::from(timeout.subsec_nanos()) as _,
        });
        self.events.clear();
        // The kernel fills the vector's spare capacity, and the vector's
        // length is then the number of events.
        match epoll::wait(
            &self.epoll,
            rustix::buffer::spare_capacity(&mut self.events),
            timespec.as_ref(),
        ) {
            Ok(_) => {}
            Err(Errno::INTR) => {}
            Err(errno) => return Err(errno.into()),
        }
        let mut ready = Vec::with_capacity(self.events.len());
        for event in &self.events {
            let flags = event.flags;
            let Some(token) = Token::decode(event.data.u64()) else {
                continue;
            };
            if token == Token::Waker {
                self.waker.drain();
            }
            ready.push((
                token,
                Readiness {
                    readable: flags.contains(epoll::EventFlags::IN),
                    writable: flags.contains(epoll::EventFlags::OUT),
                    hung_up: flags
                        .intersects(epoll::EventFlags::HUP | epoll::EventFlags::ERR | epoll::EventFlags::RDHUP),
                },
            ));
        }
        let timers = self.due_timers(Instant::now());
        Ok((ready, timers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn tokens_survive_the_kernel() {
        for token in [Token::Listener(3), Token::Client(1 << 40), Token::Held(7), Token::Waker] {
            assert_eq!(Token::decode(token.encode()), Some(token));
        }
        assert_eq!(Token::decode(0), None);
    }

    #[test]
    fn a_readable_socket_is_reported() {
        let mut event_loop = EventLoop::new().unwrap();
        let (a, b) = UnixStream::pair().unwrap();
        event_loop.add(b.as_fd(), Token::Client(9), false).unwrap();
        use std::io::Write;
        (&a).write_all(b"x").unwrap();
        let (ready, _) = event_loop.wait(Some(Duration::from_secs(1))).unwrap();
        assert!(
            ready
                .iter()
                .any(|(token, readiness)| *token == Token::Client(9) && readiness.readable)
        );
    }

    #[test]
    fn timers_run_out_in_order_and_cancelled_ones_do_not() {
        let mut event_loop = EventLoop::new().unwrap();
        let now = Instant::now();
        event_loop.set_timer(now + Duration::from_millis(20), TimerKey::RetryListener(2));
        let cancelled = event_loop.set_timer(now + Duration::from_millis(10), TimerKey::RetryListener(9));
        event_loop.set_timer(now + Duration::from_millis(5), TimerKey::RetryListener(1));
        event_loop.cancel_timer(cancelled);
        let mut seen = Vec::new();
        while seen.len() < 2 {
            let (_, timers) = event_loop.wait(Some(Duration::from_secs(1))).unwrap();
            seen.extend(timers);
        }
        assert_eq!(seen, [TimerKey::RetryListener(1), TimerKey::RetryListener(2)]);
    }

    #[test]
    fn a_still_loop_waits_for_its_limit() {
        let mut event_loop = EventLoop::new().unwrap();
        let started = Instant::now();
        let (ready, timers) = event_loop.wait(Some(Duration::from_millis(30))).unwrap();
        assert!(ready.is_empty() && timers.is_empty());
        assert!(started.elapsed() >= Duration::from_millis(25));
    }

    #[test]
    fn work_from_another_thread_wakes_the_loop() {
        let mut event_loop = EventLoop::new().unwrap();
        let remote: Remote<u32> = Remote::new(&event_loop);
        let sender = remote.clone();
        std::thread::spawn(move || sender.send(Box::new(|value| *value += 1)));
        let (ready, _) = event_loop.wait(Some(Duration::from_secs(5))).unwrap();
        assert!(ready.iter().any(|(token, _)| *token == Token::Waker));
        let mut value = 0;
        for job in remote.take() {
            job(&mut value);
        }
        assert_eq!(value, 1);
    }
}
