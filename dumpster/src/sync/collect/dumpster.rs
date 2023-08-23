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

use std::{cell::UnsafeCell, ptr::NonNull, sync::atomic::AtomicPtr};

use crate::sync::GcBox;

use super::TrashCan;

/// The size of the dumpster hash table.
const TABLE_SIZE: usize = 1 << 12;

/// A hashmap for storing cleanup information for an allocation.
struct Dumpster {
    // TODO what do I put in here?
    /// The underlying table where we store information about allocations which need to be cleaned
    /// up.
    table: NonNull<[Entry; TABLE_SIZE]>,
}

/// An entry in the dumpster table.
struct Entry {
    /// The key.
    /// This is a pointer to the allocation for which we're storing data.
    /// This will be null for a vacant entry.
    key: AtomicPtr<GcBox<()>>,
    /// The value.
    /// This is the necessary information to clean up the allocation pointed to by `key`.
    value: UnsafeCell<TrashCan>,
}
