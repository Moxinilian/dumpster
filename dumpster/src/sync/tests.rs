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

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

struct DropCount<'a>(&'a AtomicUsize);

impl<'a> Drop for DropCount<'a> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe impl Collectable for DropCount<'_> {
    fn accept<V: crate::Visitor>(&self, _: &mut V) {}

    unsafe fn destroy_gcs<D: crate::Destroyer>(&mut self, _: &mut D) {}
}

#[test]
fn single_alloc() {
    let drop_count = AtomicUsize::new(0);
    let gc1 = Gc::new(DropCount(&drop_count));

    assert_eq!(drop_count.load(Ordering::Relaxed), 0);
    drop(gc1);
    assert_eq!(drop_count.load(Ordering::Relaxed), 1);
}

#[test]
fn ref_count() {
    let drop_count = AtomicUsize::new(0);
    let gc1 = Gc::new(DropCount(&drop_count));
    let gc2 = Gc::clone(&gc1);

    assert_eq!(drop_count.load(Ordering::Relaxed), 0);
    drop(gc1);
    assert_eq!(drop_count.load(Ordering::Relaxed), 0);
    drop(gc2);
    assert_eq!(drop_count.load(Ordering::Relaxed), 1);
}