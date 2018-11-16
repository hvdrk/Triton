/*
 * This Source Code Form is subject to the terms of the Mozilla Public License,
 * v. 2.0. If a copy of the MPL was not distributed with this file, You can
 * obtain one at http://mozilla.org/MPL/2.0/.
 *
 *
 * Copyright 2018 German Research Center for Artificial Intelligence (DFKI)
 * Author: Clemens Lutz <clemens.lutz@dfki.de>
 */

extern crate accel;

use self::accel::mvec::MVec;
use self::accel::uvec::UVec;

use std::any::Any;
use std::ops::Deref;
use std::ops::DerefMut;

pub use self::Mem::*;
#[derive(Debug)]
pub enum Mem<T> {
    SysMem(Vec<T>),
    CudaDevMem(MVec<T>),
    CudaUniMem(UVec<T>),
}

impl<T: Any + Copy> Mem<T> {
    pub fn len(&self) -> usize {
        match self {
            SysMem(ref m) => m.len(),
            CudaDevMem(ref m) => m.len(),
            CudaUniMem(ref m) => m.len(),
        }
    }

    pub fn as_any(&self) -> &Any {
        match self {
            SysMem(ref m) => m as &Any,
            CudaDevMem(ref m) => m as &Any,
            CudaUniMem(ref m) => m as &Any,
        }
    }
}

impl<T> From<DerefMem<T>> for Mem<T> {
    fn from(demem: DerefMem<T>) -> Mem<T> {
        match demem {
            DerefMem::SysMem(m) => Mem::SysMem(m),
            DerefMem::CudaUniMem(m) => Mem::CudaUniMem(m),
        }
    }
}

#[derive(Debug)]
pub enum DerefMem<T> {
    SysMem(Vec<T>),
    CudaUniMem(UVec<T>),
}

impl<T> Deref for DerefMem<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        match self {
            DerefMem::SysMem(m) => m.as_slice(),
            DerefMem::CudaUniMem(m) => m.as_slice(),
        }
    }
}

impl<T> DerefMut for DerefMem<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        match self {
            DerefMem::SysMem(m) => m.as_mut_slice(),
            DerefMem::CudaUniMem(m) => m.as_slice_mut(),
        }
    }
}
