use std::os::unix::io::{FromRawFd, OwnedFd};

pub struct ShmBuffer {
    fd: OwnedFd,
    ptr: *mut libc::c_void,
    size: usize,
}

impl ShmBuffer {
    pub fn new(size: usize) -> Result<Self, std::io::Error> {
        let fd = unsafe {
            libc::syscall(libc::SYS_memfd_create, b"screencopy\0".as_ptr(), 0u32) as i32
        };

        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }

        // Check ftruncate return value
        let ret = unsafe { libc::ftruncate(fd, size as libc::off_t) };
        if ret != 0 {
            unsafe { libc::close(fd); }
            return Err(std::io::Error::last_os_error());
        }

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };

        if ptr == libc::MAP_FAILED {
            unsafe { libc::close(fd); }
            return Err(std::io::Error::last_os_error());
        }

        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            ptr,
            size,
        })
    }

    pub fn fd(&self) -> &OwnedFd {
        &self.fd
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr as *const u8, self.size) }
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr, self.size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;

    #[test]
    fn new_buffer_has_the_requested_size_and_starts_zeroed() {
        let buf = ShmBuffer::new(4096).expect("memfd_create should work in the test sandbox");
        assert_eq!(buf.as_slice().len(), 4096);
        assert!(buf.as_slice().iter().all(|&b| b == 0), "a freshly ftruncate'd memfd should read as all zeros");
    }

    #[test]
    fn fd_is_a_valid_descriptor() {
        let buf = ShmBuffer::new(4096).unwrap();
        assert!(buf.fd().as_raw_fd() >= 0);
    }

    #[test]
    fn different_buffers_get_different_sizes_correctly() {
        let small = ShmBuffer::new(64).unwrap();
        let large = ShmBuffer::new(1 << 20).unwrap();
        assert_eq!(small.as_slice().len(), 64);
        assert_eq!(large.as_slice().len(), 1 << 20);
    }
}
