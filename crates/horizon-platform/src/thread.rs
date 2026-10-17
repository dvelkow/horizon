//! Long-running work uses browser Workers; short scoped preparation stays
//! serial so the main browser event loop never waits for a newly spawned worker.
use std::marker::PhantomData;
pub use wasm_thread::*;

pub struct Scope<'scope, 'env: 'scope> {
    marker: PhantomData<&'scope mut &'env ()>,
}
pub struct ScopedJoinHandle<'scope, T> {
    value: T,
    marker: PhantomData<&'scope ()>,
}
pub fn scope<'env, F, T>(f: F) -> T
where
    F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> T,
{
    f(&Scope {
        marker: PhantomData,
    })
}
impl<'scope, 'env> Scope<'scope, 'env> {
    pub fn spawn<F, T>(&'scope self, f: F) -> ScopedJoinHandle<'scope, T>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        ScopedJoinHandle {
            value: f(),
            marker: PhantomData,
        }
    }
}
impl<T> ScopedJoinHandle<'_, T> {
    pub fn join(self) -> std::thread::Result<T> {
        Ok(self.value)
    }
    pub fn is_finished(&self) -> bool {
        true
    }
}
