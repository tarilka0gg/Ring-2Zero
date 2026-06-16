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
