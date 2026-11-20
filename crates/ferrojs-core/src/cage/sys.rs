//! Platform memory backends for the cage (D-027).
//!
//! Each backend provides the same three operations on page-aligned
//! ranges:
//!
//! - `reserve(len)`: reserve `len` bytes of address space with no access
//!   (`PROT_NONE` / `PAGE_NOACCESS`). Touching it faults.
//! - `commit(ptr, len)`: make a range inside the reservation readable and
//!   writable. Freshly committed memory reads as zero.
//! - `release(ptr, len)`: give the whole reservation back.
//!
//! Under Miri, and on platforms with neither `mmap` nor `VirtualAlloc`, the
//! reservation is an ordinary zeroed `std::alloc` block and `commit` does
//! nothing: there are no guard pages, but the cage's checked accessors
//! still refuse every access outside committed chunks.

use std::ptr::NonNull;

#[cfg(all(unix, not(miri)))]
mod imp {
    use std::ptr::{self, NonNull};

    #[cfg(any(target_os = "linux", target_os = "android"))]
    const NORESERVE: libc::c_int = libc::MAP_NORESERVE;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const NORESERVE: libc::c_int = 0;

    pub(crate) fn reserve(len: usize) -> Option<NonNull<u8>> {
        // SAFETY: an anonymous private mapping at an address the kernel
        // chooses; no existing memory is affected. The result is checked
        // for MAP_FAILED before use.
        let p = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | NORESERVE,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            None
        } else {
            NonNull::new(p.cast::<u8>())
        }
    }

    /// # Safety
    /// `ptr..ptr + len` must be page-aligned and lie inside a reservation
    /// made by `reserve` that has not been released.
    pub(crate) unsafe fn commit(ptr: *mut u8, len: usize) -> bool {
        // SAFETY: the caller guarantees the range is inside our own
        // reservation, so changing its protection touches no other memory.
        unsafe { libc::mprotect(ptr.cast(), len, libc::PROT_READ | libc::PROT_WRITE) == 0 }
    }

    /// # Safety
    /// `ptr` and `len` must be exactly a reservation returned by `reserve`,
    /// and nothing may use it afterwards.
    pub(crate) unsafe fn release(ptr: *mut u8, len: usize) {
        // SAFETY: the caller passes back exactly the mapping `reserve` made.
        let rc = unsafe { libc::munmap(ptr.cast(), len) };
        debug_assert_eq!(rc, 0, "munmap of the cage failed");
    }
}

#[cfg(all(windows, not(miri)))]
mod imp {
    use std::ptr::{self, NonNull};
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS,
        PAGE_READWRITE,
    };

    pub(crate) fn reserve(len: usize) -> Option<NonNull<u8>> {
        // SAFETY: reserves fresh address space at an address the system
        // chooses; no existing memory is affected. Null means failure.
        let p = unsafe { VirtualAlloc(ptr::null(), len, MEM_RESERVE, PAGE_NOACCESS) };
        NonNull::new(p.cast::<u8>())
    }

    /// # Safety
    /// `ptr..ptr + len` must be page-aligned and lie inside a reservation
    /// made by `reserve` that has not been released.
    pub(crate) unsafe fn commit(ptr: *mut u8, len: usize) -> bool {
        // SAFETY: the caller guarantees the range is inside our own
        // reservation; MEM_COMMIT only backs it with zeroed pages.
        let p = unsafe { VirtualAlloc(ptr.cast_const().cast(), len, MEM_COMMIT, PAGE_READWRITE) };
        !p.is_null()
    }

    /// # Safety
    /// `ptr` must be exactly a reservation returned by `reserve`, and
    /// nothing may use it afterwards.
    pub(crate) unsafe fn release(ptr: *mut u8, _len: usize) {
        // SAFETY: MEM_RELEASE with size 0 frees exactly the reservation
        // `reserve` made at this base address.
        let ok = unsafe { VirtualFree(ptr.cast(), 0, MEM_RELEASE) };
        debug_assert_ne!(ok, 0, "VirtualFree of the cage failed");
    }
}

#[cfg(any(miri, not(any(unix, windows))))]
mod imp {
    use std::alloc::{self, Layout};
    use std::ptr::NonNull;

    /// Alignment of the stand-in reservation: one guard-sized unit, like a
    /// real reservation's page alignment.
    const ALIGN: usize = super::super::GUARD_SIZE as usize;

    pub(crate) fn reserve(len: usize) -> Option<NonNull<u8>> {
        let layout = Layout::from_size_align(len, ALIGN).ok()?;
        // SAFETY: `len` is non-zero (the cage checks its size before
        // reserving), so the layout has a non-zero size.
        NonNull::new(unsafe { alloc::alloc_zeroed(layout) })
    }

    /// # Safety
    /// `ptr..ptr + len` must lie inside a reservation made by `reserve`.
    pub(crate) unsafe fn commit(_ptr: *mut u8, _len: usize) -> bool {
        // The whole block is already allocated and zeroed.
        true
    }

    /// # Safety
    /// `ptr` and `len` must be exactly a reservation returned by `reserve`,
    /// and nothing may use it afterwards.
    pub(crate) unsafe fn release(ptr: *mut u8, len: usize) {
        // `reserve` succeeded with this exact layout, so it is valid.
        let layout = Layout::from_size_align(len, ALIGN).expect("layout was valid at reserve");
        // SAFETY: `ptr` came from `alloc_zeroed` with this same layout and
        // is freed exactly once (by the cage's Drop).
        unsafe { alloc::dealloc(ptr, layout) }
    }
}

pub(crate) use imp::{commit, release};

pub(crate) fn reserve(len: usize) -> Option<NonNull<u8>> {
    if len == 0 {
        return None;
    }
    imp::reserve(len)
}
