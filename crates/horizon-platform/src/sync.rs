//! Shared locks for synchronous Rust work in browser Workers and the window.
//!
//! The window cannot call Atomics.wait. It retries short critical sections
//! while Workers use the standard blocking implementation. A lock must never
//! be held while waiting for a promise or work dispatched to the window: that
//! would deadlock a synchronous caller, with or without this adapter.
use std::fmt;
pub use std::sync::*;

#[cfg(target_arch = "wasm32")]
std::thread_local! {
    // JS inspection happens once per execution context, not for every lock.
    static IN_WORKER: bool = wasm_thread::is_web_worker_thread();
}

fn in_worker() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        IN_WORKER.with(|worker| *worker)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    } // Native unit tests exercise the browser window's retry path.
}

fn retry<T>(mut attempt: impl FnMut() -> TryLockResult<T>) -> LockResult<T> {
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(TryLockError::Poisoned(error)) => return Err(error),
            Err(TryLockError::WouldBlock) => std::hint::spin_loop(),
        }
    }
}

/// A standard mutex whose contended window-side acquisition does not wait on
/// a futex. Standard guards preserve compatibility with Worker Condvars.
pub struct Mutex<T: ?Sized> {
    inner: std::sync::Mutex<T>,
}
impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: std::sync::Mutex::new(value),
        }
    }
    pub fn into_inner(self) -> LockResult<T> {
        self.inner.into_inner()
    }
}
impl<T: ?Sized> Mutex<T> {
    pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        self.lock_for(in_worker())
    }
    fn lock_for(&self, worker: bool) -> LockResult<MutexGuard<'_, T>> {
        if worker {
            self.inner.lock()
        } else {
            retry(|| self.inner.try_lock())
        }
    }
    pub fn try_lock(&self) -> TryLockResult<MutexGuard<'_, T>> {
        self.inner.try_lock()
    }
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        self.inner.get_mut()
    }
    pub fn is_poisoned(&self) -> bool {
        self.inner.is_poisoned()
    }
    pub fn clear_poison(&self) {
        self.inner.clear_poison();
    }
}
impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}
impl<T> From<T> for Mutex<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
impl<T: ?Sized + fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(formatter)
    }
}

/// The same acquisition policy for shared and exclusive readers/writers.
pub struct RwLock<T: ?Sized> {
    inner: std::sync::RwLock<T>,
}
impl<T> RwLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: std::sync::RwLock::new(value),
        }
    }
    pub fn into_inner(self) -> LockResult<T> {
        self.inner.into_inner()
    }
}
impl<T: ?Sized> RwLock<T> {
    pub fn read(&self) -> LockResult<RwLockReadGuard<'_, T>> {
        if in_worker() {
            self.inner.read()
        } else {
            retry(|| self.inner.try_read())
        }
    }
    pub fn write(&self) -> LockResult<RwLockWriteGuard<'_, T>> {
        if in_worker() {
            self.inner.write()
        } else {
            retry(|| self.inner.try_write())
        }
    }
    pub fn try_read(&self) -> TryLockResult<RwLockReadGuard<'_, T>> {
        self.inner.try_read()
    }
    pub fn try_write(&self) -> TryLockResult<RwLockWriteGuard<'_, T>> {
        self.inner.try_write()
    }
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        self.inner.get_mut()
    }
    pub fn is_poisoned(&self) -> bool {
        self.inner.is_poisoned()
    }
    pub fn clear_poison(&self) {
        self.inner.clear_poison();
    }
}
impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}
impl<T> From<T> for RwLock<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLock<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[test]
    fn window_retry_waits_for_an_independent_lock_owner() {
        let mutex = Arc::new(Mutex::new(0));
        let contended = Arc::new(AtomicBool::new(false));
        let (ready, held) = mpsc::channel();
        let owner = mutex.clone();
        let observed = contended.clone();
        let worker = std::thread::spawn(move || {
            let mut guard = owner.lock_for(true).unwrap();
            ready.send(()).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !observed.load(Ordering::Acquire) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "window did not contend"
                );
                std::thread::yield_now();
            }
            *guard = 41;
        });
        held.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut guard = retry(|| {
            let result = mutex.try_lock();
            if matches!(result, Err(TryLockError::WouldBlock)) {
                contended.store(true, Ordering::Release);
            }
            result
        })
        .unwrap();
        *guard += 1;
        drop(guard);
        worker.join().unwrap();
        assert_eq!(*mutex.lock().unwrap(), 42);
    }

    #[test]
    fn worker_condvar_accepts_the_standard_mutex_guard() {
        let pair = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_pair = pair.clone();
        let worker = std::thread::spawn(move || {
            let guard = worker_pair.0.lock_for(true).unwrap();
            let (guard, timeout) = worker_pair
                .1
                .wait_timeout_while(guard, Duration::from_secs(5), |ready| !*ready)
                .unwrap();
            assert!(*guard && !timeout.timed_out());
        });
        *pair.0.lock().unwrap() = true;
        pair.1.notify_one();
        worker.join().unwrap();
    }

    #[test]
    fn poison_is_returned_instead_of_retried_forever() {
        let mutex = Arc::new(Mutex::new(1));
        let owner = mutex.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = owner.lock_for(true).unwrap();
                panic!("intentional poison");
            })
            .join()
            .is_err()
        );
        assert!(mutex.is_poisoned());
        *mutex.lock().unwrap_err().into_inner() = 2;
        mutex.clear_poison();
        let mut mutex = Arc::try_unwrap(mutex).unwrap();
        *mutex.get_mut().unwrap() += 1;
        assert_eq!(mutex.into_inner().unwrap(), 3);
    }

    #[test]
    fn rwlock_preserves_exclusion_and_poison_recovery() {
        let lock = Arc::new(RwLock::from(7));
        let reader = lock.read().unwrap();
        assert!(matches!(lock.try_write(), Err(TryLockError::WouldBlock)));
        assert_eq!(*lock.try_read().unwrap(), 7);
        drop(reader);
        let owner = lock.clone();
        assert!(
            std::thread::spawn(move || {
                let mut guard = owner.write().unwrap();
                *guard = 8;
                panic!("intentional poison");
            })
            .join()
            .is_err()
        );
        assert!(lock.is_poisoned());
        assert_eq!(*lock.read().unwrap_err().into_inner(), 8);
        *lock.write().unwrap_err().into_inner() = 9;
        lock.clear_poison();
        let mut lock = Arc::try_unwrap(lock).unwrap();
        *lock.get_mut().unwrap() += 1;
        assert_eq!(lock.into_inner().unwrap(), 10);
    }
}
