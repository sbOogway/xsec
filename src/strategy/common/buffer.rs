//! [`BoundedQueue`]: the fixed-capacity ring buffer behind the runtime's
//! rolling close-price windows ([`RuntimeState::prices`](super::RuntimeState)).
//! One per instrument, sized to the strategy's formation window; each new bar
//! close is pushed with [`BoundedQueue::push_back_overwrite`], evicting the
//! oldest once the window is full.

use std::collections::VecDeque;

pub struct BoundedQueue<T> {
    pub inner: VecDeque<T>,
    capacity: usize,
}

impl<T> BoundedQueue<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    // VARIANT A: Reject the item if full
    pub fn try_push_back(&mut self, item: T) -> Result<(), T> {
        if self.inner.len() >= self.capacity {
            return Err(item); // Return item back to caller
        }
        self.inner.push_back(item);
        Ok(())
    }

    // VARIANT B: Evict the oldest item (ring buffer behavior)
    pub fn push_back_overwrite(&mut self, item: T) -> Option<T> {
        let mut evicted = None;
        if self.inner.len() >= self.capacity {
            evicted = self.inner.pop_front(); // Evict oldest
        }
        self.inner.push_back(item);
        evicted
    }

    pub fn pop_front(&mut self) -> Option<T> {
        self.inner.pop_front()
    }
}
