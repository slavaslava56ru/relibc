// Used design from https://www.remlab.net/op/futex-condvar.shtml

use crate::{
    error::Errno,
    header::{
        errno::{EINVAL, ETIMEDOUT},
        pthread::{PTHREAD_PROCESS_PRIVATE, RlctMutex},
        time::{CLOCK_MONOTONIC, CLOCK_REALTIME, timespec, timespec_realtime_to_monotonic},
    },
    platform::types::clockid_t,
};

use core::sync::atomic::{AtomicU32 as AtomicUint, Ordering};

#[derive(Clone, Copy)]
pub struct CondAttr {
    pub clock: clockid_t,
    pub pshared: i32,
}

impl Default for CondAttr {
    fn default() -> Self {
        Self {
            // defaults according to POSIX
            clock: CLOCK_REALTIME,            // for timedwait
            pshared: PTHREAD_PROCESS_PRIVATE, // TODO
        }
    }
}

pub struct Cond {
    cur: AtomicUint,
    /// Sequence number seen by the last waiter; the top bit instead records that timed waits
    /// use CLOCK_MONOTONIC (pthread_cond_t has no room for another field).
    prev: AtomicUint,
}

const MONOTONIC_BIT: u32 = 1 << 31;
const SEQ_MASK: u32 = !MONOTONIC_BIT;

type Result<T, E = Errno> = core::result::Result<T, E>;

impl Default for Cond {
    fn default() -> Self {
        Self::new()
    }
}

impl Cond {
    pub fn new() -> Self {
        Self::with_clock(CLOCK_REALTIME)
    }
    pub fn with_clock(clock: clockid_t) -> Self {
        Self {
            cur: AtomicUint::new(0),
            prev: AtomicUint::new(if clock == CLOCK_MONOTONIC {
                MONOTONIC_BIT
            } else {
                0
            }),
        }
    }
    fn clock(&self) -> clockid_t {
        if self.prev.load(Ordering::Relaxed) & MONOTONIC_BIT != 0 {
            CLOCK_MONOTONIC
        } else {
            CLOCK_REALTIME
        }
    }
    fn wake(&self, count: i32) -> Result<(), Errno> {
        // This is formally correct as long as we don't have more than 2^31 threads.
        let prev = self.prev.load(Ordering::Relaxed) & SEQ_MASK;
        self.cur
            .store(prev.wrapping_add(1) & SEQ_MASK, Ordering::Relaxed);

        crate::sync::futex_wake(&self.cur, count);
        Ok(())
    }
    pub fn broadcast(&self) -> Result<(), Errno> {
        self.wake(i32::MAX)
    }
    pub fn signal(&self) -> Result<(), Errno> {
        self.broadcast()
        //self.wake(1)
    }
    pub fn clockwait(
        &self,
        mutex: &RlctMutex,
        timeout: &timespec,
        clock_id: clockid_t,
    ) -> Result<(), Errno> {
        let relative = match clock_id {
            // FUTEX expect monotonic clock
            CLOCK_MONOTONIC => timeout.clone(),
            CLOCK_REALTIME => timespec_realtime_to_monotonic(timeout)?,
            _ => return Err(Errno(EINVAL)),
        };

        self.wait_inner(mutex, Some(&relative))
    }
    pub fn timedwait(&self, mutex: &RlctMutex, timeout: &timespec) -> Result<(), Errno> {
        self.clockwait(mutex, timeout, self.clock())
    }
    fn wait_inner(&self, mutex: &RlctMutex, timeout: Option<&timespec>) -> Result<(), Errno> {
        self.wait_inner_generic(|| mutex.unlock(), || mutex.lock(), timeout)
    }
    pub fn wait_inner_typedmutex<'lock, T>(
        &self,
        guard: crate::sync::MutexGuard<'lock, T>,
    ) -> crate::sync::MutexGuard<'lock, T> {
        let mut newguard = None;
        let lock = guard.mutex;
        self.wait_inner_generic(
            move || {
                drop(guard);
                Ok(())
            },
            || {
                newguard = Some(lock.lock());
                Ok(())
            },
            None,
        )
        .unwrap();
        newguard.unwrap()
    }
    // TODO: FUTEX_REQUEUE
    fn wait_inner_generic(
        &self,
        unlock: impl FnOnce() -> Result<()>,
        lock: impl FnOnce() -> Result<()>,
        deadline: Option<&timespec>,
    ) -> Result<(), Errno> {
        // TODO: Error checking for certain types (i.e. robust and errorcheck) of mutexes, e.g. if the
        // mutex is not locked.
        let current = self.cur.load(Ordering::Relaxed);
        let clock_bit = self.prev.load(Ordering::Relaxed) & MONOTONIC_BIT;
        self.prev
            .store((current & SEQ_MASK) | clock_bit, Ordering::Relaxed);

        unlock()?;
        let futex_r = crate::sync::futex_wait(&self.cur, current, deadline);
        lock()?;

        match futex_r {
            super::FutexWaitResult::Waited => Ok(()),
            super::FutexWaitResult::Stale => Ok(()),
            super::FutexWaitResult::TimedOut => Err(Errno(ETIMEDOUT)),
        }
    }
    pub fn wait(&self, mutex: &RlctMutex) -> Result<(), Errno> {
        self.wait_inner(mutex, None)
    }
}
