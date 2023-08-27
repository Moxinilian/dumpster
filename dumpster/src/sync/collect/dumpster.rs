/*
   dumpster, a cycle-tracking garbage collector for Rust.
   Copyright (C) 2023 Clayton Ramsey.

   This program is free software: you can redistribute it and/or modify
   it under the terms of the GNU General Public License as published by
   the Free Software Foundation, either version 3 of the License, or
   (at your option) any later version.

   This program is distributed in the hope that it will be useful,
   but WITHOUT ANY WARRANTY; without even the implied warranty of
   MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
   GNU General Public License for more details.

   You should have received a copy of the GNU General Public License
   along with this program.  If not, see <http://www.gnu.org/licenses/>.
*/

//! Implementation of a custom hash-map used by the collection algorithm.
//!
//! This hash-map exclusively uses thin pointers as its keys and [`TrashCan`]s as its values, and
//! uses clever CAS algorithms to locklessly allow edits to the table.

#![allow(unused)]

use std::{
    alloc::{alloc_zeroed, Layout},
    cell::UnsafeCell,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    mem::{transmute, MaybeUninit},
    ptr::{null, null_mut, NonNull},
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use crate::sync::GcBox;

use super::{AllocationId, TrashCan};

/// The size of the dumpster hash table.
const TABLE_SIZE: usize = 1 << 12;

#[derive(Debug)]
/// A hashmap for storing cleanup information for an allocation.
pub(super) struct Dumpster {
    /// The underlying table where we store information about allocations which need to be cleaned
    /// up.
    table: Box<[Entry; TABLE_SIZE]>,
    /// The number of entries currently in the table.
    n_entries: AtomicUsize,
}

/// An iterator over a [`Dumpster`].
pub(super) struct Iterator {
    /// The dumpster we're iterating over.
    dumpster: Dumpster,
    /// Our current index in the dumpster's table.
    idx: usize,
}

/// An entry in the [`Dumpster`] table.
struct Entry {
    /// The key.
    /// This is a pointer to the allocation for which we're storing data.
    /// This will be null for a vacant entry.
    key: AtomicPtr<GcBox<()>>,
    /// The value.
    /// This is the necessary information to clean up the allocation pointed to by `key`.
    value: UnsafeCell<TrashCan>,
}

impl Dumpster {
    /// Construct a new, empty dumpster.
    pub fn new() -> Dumpster {
        Dumpster {
            table: unsafe {
                Box::from_raw(alloc_zeroed(Layout::new::<[Entry; TABLE_SIZE]>()).cast())
            },
            n_entries: AtomicUsize::new(0),
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    /// Attempt to insert an entry into the dumpster.
    ///
    /// Returns `Ok(true)` if a new element was inserted, and `Ok(false)` if an element was removed.
    ///
    /// # Errors
    ///
    /// This function will return an error if the dumpster is full.
    pub fn try_insert(&self, key: AllocationId, value: TrashCan) -> Result<bool, ()> {
        // println!("before insert: {self:?}");
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let hash_idx = hasher.finish() as usize;
        for offset in 0..TABLE_SIZE {
            let idx: usize = (hash_idx + offset) & (TABLE_SIZE - 1);

            match self.table[idx].key.compare_exchange(
                null_mut(),
                key.0.as_ptr(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    unsafe { self.table[idx].value.get().write(value) };
                    self.n_entries.fetch_add(1, Ordering::Relaxed);

                    // println!("after insert: {self:?}");
                    return Ok(true);
                }
                Err(e) if e == key.0.as_ptr() => {
                    // println!("after insert: {self:?}");
                    return Ok(false);
                }
                _ => (),
            }
        }

        // println!("after insert: {self:?}");
        Err(())
    }

    #[allow(clippy::cast_possible_truncation)]
    /// Attempt to remove an entry from this dumpster.
    ///
    /// Returns `true` if an entry was removed and `false` otherwise.
    pub fn remove(&self, key: AllocationId) -> bool {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let hash_idx = hasher.finish() as usize;
        for offset in 0..TABLE_SIZE {
            let idx: usize = (hash_idx + offset) & (TABLE_SIZE - 1);

            match self.table[idx].key.compare_exchange(
                key.0.as_ptr(),
                null_mut(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.n_entries.fetch_sub(1, Ordering::Relaxed);
                    return true;
                }
                Err(e) if e.is_null() => return false,
                _ => (),
            }
        }

        false
    }

    /// Get the number of entries currently in the dumpster.
    pub fn len(&self) -> usize {
        self.n_entries.load(Ordering::Relaxed)
    }

    /// Determine whether this dumpster is full (and needs to be emptied).
    pub fn is_full(&self) -> bool {
        self.len() >= (TABLE_SIZE / 2)
    }
}

impl IntoIterator for Dumpster {
    type Item = (AllocationId, TrashCan);

    type IntoIter = Iterator;

    fn into_iter(self) -> Self::IntoIter {
        Iterator {
            dumpster: self,
            idx: 0,
        }
    }
}

impl std::iter::Iterator for Iterator {
    type Item = (AllocationId, TrashCan);

    fn next(&mut self) -> Option<Self::Item> {
        while self.idx < TABLE_SIZE {
            let k = self.dumpster.table[self.idx].key.load(Ordering::Relaxed);
            self.idx += 1;
            if !k.is_null() {
                return Some((AllocationId(NonNull::new(k).unwrap()), unsafe {
                    *self.dumpster.table[self.idx - 1].value.get_mut()
                }));
            }
        }

        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(TABLE_SIZE - self.idx))
    }
}

unsafe impl Send for Dumpster {}
unsafe impl Sync for Dumpster {}

impl Default for Dumpster {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry").field("key", &self.key).finish()
    }
}
