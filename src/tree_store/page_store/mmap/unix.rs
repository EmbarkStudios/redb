use super::*;
use std::os::unix::io::AsRawFd;

pub(super) struct FileLock {
    fd: libc::c_int,
}

impl FileLock {
    pub(super) fn new(file: &File) -> Result<Self> {
        let fd = file.as_raw_fd();
        let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                Err(Error::DatabaseAlreadyOpen)
            } else {
                Err(Error::Io(err))
            }
        } else {
            Ok(Self { fd })
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.fd, libc::LOCK_UN) };
    }
}

pub(super) struct MmapInner {
    pub(super) mmap: *mut u8,
    pub(super) capacity: usize,
}

impl MmapInner {
    pub(super) fn create_mapping(file: &File, max_capacity: usize) -> Result<Self> {
        let mmap = unsafe {
            libc::mmap(
                ptr::null_mut(),
                max_capacity as libc::size_t,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if mmap == libc::MAP_FAILED {
            Err(io::Error::last_os_error().into())
        } else {
            Ok(Self {
                mmap: mmap as *mut u8,
                capacity: max_capacity,
            })
        }
    }

    /// Safety: if new_len < len(), caller must ensure that no references to memory in new_len..len() exist
    pub(super) unsafe fn resize(&self, owner: &Mmap) -> Result<()> {
        let mmap = libc::mmap(
            self.mmap as *mut libc::c_void,
            self.capacity as libc::size_t,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED | libc::MAP_FIXED,
            owner.file.as_raw_fd(),
            0,
        );

        if mmap == libc::MAP_FAILED {
            Err(io::Error::last_os_error().into())
        } else {
            assert_eq!(mmap as *mut u8, self.mmap);
            Ok(())
        }
    }

    pub(super) fn flush(&self, owner: &Mmap) -> Result {
        // Disable fsync when fuzzing, since it doesn't test crash consistency
        #[cfg(not(fuzzing))]
        {
            #[cfg(not(target_os = "macos"))]
            {
                let result = unsafe {
                    libc::msync(
                        self.mmap as *mut libc::c_void,
                        owner.len() as libc::size_t,
                        libc::MS_SYNC,
                    )
                };
                if result != 0 {
                    return Err(io::Error::last_os_error().into());
                }
            }
            #[cfg(target_os = "macos")]
            {
                let code = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_FULLFSYNC) };
                if code == -1 {
                    return Err(io::Error::last_os_error().into());
                }
            }
        }
        Ok(())
    }

    pub(super) fn eventual_flush(&self, owner: &Mmap) -> Result {
        #[cfg(not(target_os = "macos"))]
        {
            return self.flush(owner);
        }
        #[cfg(all(target_os = "macos", not(fuzzing)))]
        {
            // TODO: It may be unsafe to mix F_BARRIERFSYNC with writes to the mmap.
            //       Investigate switching to `write()`
            let code = unsafe { libc::fcntl(owner.file.as_raw_fd(), libc::F_BARRIERFSYNC) };
            if code == -1 {
                Err(io::Error::last_os_error().into());
            } else {
                Ok(())
            }
        }
    }
}

impl Drop for MmapInner {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(
                self.mmap as *mut libc::c_void,
                self.capacity as libc::size_t,
            );
        }
    }
}