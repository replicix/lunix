//! A zero-initialised heap buffer aligned to 4096 bytes, as `O_DIRECT` and
//! fixed io_uring buffers need.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::ptr::NonNull;

/// Alignment of every buffer (the journal block).
pub(crate) const ALIGN: usize = 4096;

/// An owned, 4096-aligned, zero-initialised byte buffer. The address never
/// changes for the lifetime of the value, so it may be registered with a ring.
pub(crate) struct AlignedBuf {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: the buffer is uniquely owned heap memory with no interior sharing;
// moving it to another thread, or reading it from several, is as safe as for
// a `Box<[u8]>`.
unsafe impl Send for AlignedBuf {}
// SAFETY: `&AlignedBuf` only exposes `&[u8]` and a raw const pointer.
unsafe impl Sync for AlignedBuf {}

impl AlignedBuf {
    /// Allocates `len` zero bytes; `len` must be a non-zero multiple of 4096.
    pub(crate) fn zeroed(len: usize) -> std::io::Result<Self> {
        if len == 0 || !len.is_multiple_of(ALIGN) {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }
        let layout = Layout::from_size_align(len, ALIGN)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        // SAFETY: the layout has a non-zero size.
        let raw = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(raw)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
        Ok(Self { ptr, len })
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn as_ptr(&self) -> *const u8 {
        self.ptr.as_ptr()
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` points at `len` initialised bytes owned by `self`.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, and `&mut self` is unique.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // The layout was valid at construction, so it is valid again.
        if let Ok(layout) = Layout::from_size_align(self.len, ALIGN) {
            // SAFETY: allocated in `zeroed` with this exact layout.
            unsafe { dealloc(self.ptr.as_ptr(), layout) };
        }
    }
}

impl std::fmt::Debug for AlignedBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AlignedBuf({} bytes)", self.len)
    }
}
