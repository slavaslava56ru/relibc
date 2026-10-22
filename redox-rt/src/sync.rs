// TODO: Share code for simple futex-based mutex between relibc's Mutex<()> and this.

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU32, Ordering},
};

pub struct Mutex<T> {
    pub lockword: AtomicU32,
    pub inner: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    /// Represents an unlocked [Mutex].
    pub const UNLOCKED: u32 = 0;
    /// Represents a locked [Mutex].
    pub const LOCKED: u32 = 1;
    /// Represents a waiting [Mutex].
    pub const WAITING: u32 = 2;

    pub const fn new(t: T) -> Self {
        Self {
            lockword: AtomicU32::new(0),
            inner: UnsafeCell::new(t),
        }
    }
    pub fn lock(&self) -> MutexGuard<'_, T> {
        for _ in 0..100 {
            if self
                .lockword
                .compare_exchange(
                    Self::UNLOCKED,
                    Self::LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                return MutexGuard { lock: self };
            }
            core::hint::spin_loop();
        }
        // Contended: sleep in the kernel rather than spin. Spinning without ever entering the
        // kernel can deadlock against a holder that waits there for this CPU (TLB shootdown).
        while self.lockword.swap(Self::WAITING, Ordering::Acquire) != Self::UNLOCKED {
            // Spurious wakeups, EAGAIN (already unlocked) and EINTR all just retry.
            let _ = unsafe {
                syscall::syscall5(
                    syscall::SYS_FUTEX,
                    self.lockword.as_ptr() as usize,
                    syscall::FUTEX_WAIT,
                    Self::WAITING as usize,
                    0,
                    0,
                )
            };
        }
        MutexGuard { lock: self }
    }
}
pub struct MutexGuard<'l, T> {
    lock: &'l Mutex<T>,
}
impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.lock.inner.get() }
    }
}
impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.inner.get() }
    }
}
impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        if self
            .lock
            .lockword
            .swap(Mutex::<T>::UNLOCKED, Ordering::Release)
            == Mutex::<T>::WAITING
        {
            let _ = unsafe {
                syscall::syscall5(
                    syscall::SYS_FUTEX,
                    self.lock.lockword.as_ptr() as usize,
                    syscall::FUTEX_WAKE,
                    1,
                    0,
                    0,
                )
            };
        }
    }
}
