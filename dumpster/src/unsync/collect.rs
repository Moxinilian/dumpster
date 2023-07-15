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

//! Implementations of the single-threaded garbage-collection logic.

use std::{
    alloc::{dealloc, Layout},
    cell::{Cell, RefCell},
    collections::{hash_map::Entry, HashMap, HashSet},
    num::NonZeroUsize,
    ops::Deref,
    ptr::{addr_of_mut, drop_in_place, NonNull},
};

use crate::{unsync::Gc, Collectable, Destroyer, OpaquePtr, Visitor};

use super::GcBox;

thread_local! {
    /// The global collection of allocation information for this thread.
    pub(super) static DUMPSTER: Dumpster = Dumpster {
        to_collect: RefCell::new(HashMap::new()),
        n_ref_drops: Cell::new(0),
        n_refs_living: Cell::new(0),
    };
}

/// A dumpster is a collection of all the garbage that may or may not need to be cleaned up.
/// It also contains information relevant to when a sweep should be triggered.
pub(super) struct Dumpster {
    /// A map from allocation IDs for allocations which may need to be collected to pointers to
    /// their allocations.
    to_collect: RefCell<HashMap<AllocationId, Cleanup>>,
    /// The number of times a reference has been dropped since the last collection was triggered.
    n_ref_drops: Cell<usize>,
    /// The number of references that currently exist in the entire heap and stack.
    n_refs_living: Cell<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
/// A unique identifier for an allocated garbage-collected block.
///
/// It contains a pointer to the reference count of the allocation.
struct AllocationId(pub NonNull<Cell<usize>>);

impl AllocationId {
    /// Get the reference count of the allocation that this allocation ID points to.
    unsafe fn count(self) -> usize {
        self.0.as_ref().get()
    }
}

impl<T> From<NonNull<GcBox<T>>> for AllocationId
where
    T: Collectable + ?Sized,
{
    /// Get an allocation ID from a pointer to an allocation.
    fn from(value: NonNull<GcBox<T>>) -> Self {
        AllocationId(value.cast())
    }
}

#[derive(Debug)]
/// The necessary information required to collect some garbage-collected data.
/// This data is stored in a map from allocation IDs to the necessary cleanup operation.
struct Cleanup {
    /// The function which is called to build the reference graph and find all allocations
    /// reachable from this allocation.
    build_graph_fn: unsafe fn(OpaquePtr, &mut BuildRefGraph),
    /// The function which is called to sweep out and mark allocations reachable from this
    /// allocation as reachable.
    sweep_fn: unsafe fn(OpaquePtr, &mut Sweep),
    /// The function which is called to destroy all [`Gc`]s owned by this allocation prior to
    /// dropping it.
    destroy_gcs_fn: unsafe fn(OpaquePtr, &mut DestroyGcs),
    /// An opaque pointer to the allocation.
    ptr: OpaquePtr,
}

impl Cleanup {
    /// Construct a new cleanup for an allocation.
    fn new<T: Collectable + ?Sized>(box_ptr: NonNull<GcBox<T>>) -> Cleanup {
        Cleanup {
            build_graph_fn: apply_visitor::<T, BuildRefGraph>,
            sweep_fn: apply_visitor::<T, Sweep>,
            destroy_gcs_fn: destroy_gcs::<T>,
            ptr: OpaquePtr::new(box_ptr),
        }
    }
}

/// Apply a visitor to some opaque pointer.
///
/// # Safety
///
/// `T` must be the same type that `ptr` was created with via [`OpaquePtr::new`].
unsafe fn apply_visitor<T: Collectable + ?Sized, V: Visitor>(ptr: OpaquePtr, visitor: &mut V) {
    let specified: NonNull<GcBox<T>> = ptr.specify();
    let _ = specified.as_ref().value.accept(visitor);
}

/// Destroy the garbage-collected values of some opaquely-defined type.
///
/// # Safety
///
/// `T` must be the same type that `ptr` was created with via [`OpaquePtr::new`].
unsafe fn destroy_gcs<T: Collectable + ?Sized>(ptr: OpaquePtr, destroyer: &mut DestroyGcs) {
    let mut specific_ptr = ptr.specify::<GcBox<T>>();
    specific_ptr.as_mut().ref_count.set(0);
    specific_ptr.as_mut().value.destroy_gcs(destroyer);

    destroyer.collection_queue.push((
        specific_ptr.as_ptr().cast(),
        Layout::for_value(specific_ptr.as_ref()),
    ));
    drop_in_place(addr_of_mut!(specific_ptr.as_mut().value));
}

impl Dumpster {
    /// Collect all unreachable allocations that this dumpster is responsible for.
    pub fn collect_all(&self) {
        self.n_ref_drops.set(0);

        unsafe {
            let mut ref_graph_build = BuildRefGraph {
                visited: HashSet::new(),
                ref_state: HashMap::new(),
            };

            for (k, v) in self.to_collect.borrow().iter() {
                if !ref_graph_build.visited.contains(k) {
                    ref_graph_build.visited.insert(*k);
                    (v.build_graph_fn)(v.ptr, &mut ref_graph_build);
                }
            }

            let mut sweep = Sweep {
                visited: HashSet::new(),
            };
            for (id, reachability) in ref_graph_build
                .ref_state
                .iter()
                .filter(|(id, reachability)| id.count() != reachability.cyclic_ref_count.into())
            {
                sweep.visited.insert(*id);
                (reachability.sweep_fn)(reachability.ptr, &mut sweep);
            }

            // any allocations which we didn't find must also be roots
            for (id, cleanup) in self
                .to_collect
                .borrow()
                .iter()
                .filter(|(id, _)| !ref_graph_build.ref_state.contains_key(id))
            {
                sweep.visited.insert(*id);
                (cleanup.sweep_fn)(cleanup.ptr, &mut sweep);
            }

            let mut destroy = DestroyGcs {
                visited: HashSet::new(),
                collection_queue: Vec::new(),
                reachable: sweep.visited,
            };
            // any allocation not found in the sweep must be freed
            for (id, cleanup) in self.to_collect.borrow_mut().drain() {
                if !destroy.reachable.contains(&id) && destroy.visited.insert(id) {
                    (cleanup.destroy_gcs_fn)(cleanup.ptr, &mut destroy);
                }
            }

            for (ptr, layout) in destroy.collection_queue {
                dealloc(ptr, layout);
            }
        }
    }

    /// Mark an allocation as "dirty," implying that it may need to be swept through later to find
    /// out if it has any references pointing to it.
    pub unsafe fn mark_dirty<T: Collectable + ?Sized>(&self, box_ptr: NonNull<GcBox<T>>) {
        self.to_collect
            .borrow_mut()
            .entry(AllocationId::from(box_ptr))
            .or_insert_with(|| Cleanup::new(box_ptr));
    }

    /// Mark an allocation as "cleaned," implying that the allocation is about to be destroyed and
    /// therefore should not be cleaned up later.
    pub fn mark_cleaned<T: Collectable + ?Sized>(&self, box_ptr: NonNull<GcBox<T>>) {
        self.to_collect
            .borrow_mut()
            .remove(&AllocationId::from(box_ptr));
    }

    /// Notify the dumpster that a garbage-collected pointer has been dropped.
    ///
    /// This may trigger a sweep of the heap, but is guaranteed to be amortized to _O(1)_.
    pub fn notify_dropped_gc(&self) {
        self.n_ref_drops.set(self.n_ref_drops.get() + 1);
        let old_refs_living = self.n_refs_living.get();
        assert_ne!(
            old_refs_living, 0,
            "underflow on unsync::Gc number of living Gcs"
        );
        self.n_refs_living.set(old_refs_living - 1);

        // check if it's been a long time since the last time we collected all
        // the garbage.
        // if so, go and collect it all again (amortized O(1))
        if self.n_ref_drops.get() << 1 >= self.n_refs_living.get() {
            self.collect_all();
        }
    }

    pub fn notify_created_gc(&self) {
        self.n_refs_living.set(self.n_refs_living.get() + 1);
    }
}

impl Drop for Dumpster {
    fn drop(&mut self) {
        // cleanup any leftover allocations
        self.collect_all();
    }
}

/// The data required to construct the graph of reachable allocations.
struct BuildRefGraph {
    /// The set of allocations which have already been visited.
    visited: HashSet<AllocationId>,
    /// A map from allocation identifiers to information about their reachability.
    ref_state: HashMap<AllocationId, Reachability>,
}

#[derive(Debug)]
/// Information about the reachability of a structure.
struct Reachability {
    /// The number of references found to this structure which are contained within the heap.
    /// If this number is equal to the allocations reference count, it is unreachable.
    cyclic_ref_count: NonZeroUsize,
    /// An opaque pointer to the allocation under concern.
    ptr: OpaquePtr,
    /// A function used to sweep from `ptr` if this allocation is proven reachable.
    sweep_fn: unsafe fn(OpaquePtr, &mut Sweep),
}

impl Visitor for BuildRefGraph {
    fn visit_sync<T>(&mut self, _: &crate::sync::Gc<T>)
    where
        T: Collectable + Sync + ?Sized,
    {
        // because `Gc` is `!Sync`, we know we won't find a `Gc` this way and can return
        // immediately.
    }

    fn visit_unsync<T>(&mut self, gc: &Gc<T>)
    where
        T: Collectable + ?Sized,
    {
        let next_id = AllocationId::from(gc.ptr.unwrap());
        match self.ref_state.entry(next_id) {
            Entry::Occupied(ref mut o) => {
                o.get_mut().cyclic_ref_count = o.get().cyclic_ref_count.saturating_add(1);
            }
            Entry::Vacant(v) => {
                v.insert(Reachability {
                    cyclic_ref_count: NonZeroUsize::MIN,
                    ptr: OpaquePtr::new(gc.ptr.unwrap()),
                    sweep_fn: apply_visitor::<T, Sweep>,
                });
            }
        }
        if self.visited.insert(next_id) {
            gc.deref().accept(self).unwrap();
        }
    }
}

/// A sweep, which marks allocations as reachable.
struct Sweep {
    /// The set of allocations which have been marked as reachable.
    visited: HashSet<AllocationId>,
}

impl Visitor for Sweep {
    fn visit_sync<T>(&mut self, _: &crate::sync::Gc<T>)
    where
        T: Collectable + Sync + ?Sized,
    {
        // because `Gc` is `!Sync`, we know we won't find a `Gc` this way and can return
        // immediately.
    }

    fn visit_unsync<T>(&mut self, gc: &Gc<T>)
    where
        T: Collectable + ?Sized,
    {
        if self.visited.insert(AllocationId::from(gc.ptr.unwrap())) {
            gc.deref().accept(self).unwrap();
        }
    }
}

/// The data used to destroy all garbage collected pointers in unreachable allocations.
struct DestroyGcs {
    /// The set of allocations which have been visited already.
    visited: HashSet<AllocationId>,
    /// The data used to call [`dealloc`] on an allocation, deferred until after the destruction of
    /// all garbage-collected pointers is complete.
    collection_queue: Vec<(*mut u8, Layout)>,
    /// The set of allocations which are still reachable by the program.
    /// These should not be destroyed!
    reachable: HashSet<AllocationId>,
}

impl Destroyer for DestroyGcs {
    fn visit_sync<T>(&mut self, _: &mut crate::sync::Gc<T>)
    where
        T: Collectable + Sync + ?Sized,
    {
        // because `Gc` is `!Sync`, we know we won't find a `Gc` this way and can return
        // immediately.
    }

    fn visit_unsync<T>(&mut self, gc: &mut Gc<T>)
    where
        T: Collectable + ?Sized,
    {
        unsafe {
            if let Some(mut p) = gc.ptr {
                let id = AllocationId::from(p);
                gc.ptr = None;
                if !self.reachable.contains(&id) && self.visited.insert(id) {
                    p.as_mut().ref_count.set(0);
                    p.as_mut().value.destroy_gcs(self);
                    self.collection_queue
                        .push((p.as_ptr().cast(), Layout::for_value(p.as_ref())));
                    drop_in_place(addr_of_mut!(p.as_mut().value));
                }
            }
        }
    }
}
