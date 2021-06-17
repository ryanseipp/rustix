//! Functions which operate on file descriptors.

use crate::io;
#[cfg(all(libc, not(target_os = "wasi")))]
use crate::negone_err;
#[cfg(all(libc, not(target_os = "redox")))]
use crate::zero_ok;
use io_lifetimes::{AsFd, BorrowedFd, OwnedFd};
#[cfg(all(libc, not(any(target_os = "wasi", target_os = "fuchsia"))))]
use std::ffi::OsString;
#[cfg(not(target_os = "redox"))]
use std::mem::MaybeUninit;
#[cfg(all(libc, unix, not(any(target_os = "wasi", target_os = "fuchsia"))))]
use std::os::unix::ffi::OsStringExt;
#[cfg(libc)]
use {
    errno::errno,
    unsafe_io::os::posish::{AsRawFd, FromRawFd},
};

/// `ioctl(fd, FIONREAD)`.
#[cfg(not(target_os = "redox"))]
#[inline]
pub fn ioctl_fionread<'f, Fd: AsFd<'f>>(fd: Fd) -> io::Result<u64> {
    let fd = fd.as_fd();
    _ioctl_fionread(fd)
}

#[cfg(all(libc, not(target_os = "redox")))]
fn _ioctl_fionread(fd: BorrowedFd<'_>) -> io::Result<u64> {
    let mut nread = MaybeUninit::<libc::c_int>::uninit();
    unsafe {
        zero_ok(libc::ioctl(
            fd.as_raw_fd() as libc::c_int,
            libc::FIONREAD,
            nread.as_mut_ptr(),
        ))?;
        // `FIONREAD` returns the number of bytes silently casted to a `c_int`,
        // even when this is lossy. The best we can do is convert it back to a
        // `u64` without sign-extending it back first.
        Ok(nread.assume_init() as libc::c_uint as u64)
    }
}

#[cfg(linux_raw)]
#[inline]
fn _ioctl_fionread(fd: BorrowedFd<'_>) -> io::Result<u64> {
    crate::linux_raw::ioctl_fionread(fd)
}

/// `isatty(fd)`
#[inline]
pub fn isatty<'f, Fd: AsFd<'f>>(fd: Fd) -> bool {
    let fd = fd.as_fd();
    _isatty(fd)
}

#[cfg(libc)]
fn _isatty(fd: BorrowedFd<'_>) -> bool {
    let res = unsafe { libc::isatty(fd.as_raw_fd() as libc::c_int) };
    if res == 0 {
        match errno().0 {
            #[cfg(not(any(target_os = "android", target_os = "linux")))]
            libc::ENOTTY => false,

            // Old Linux versions reportedly return `EINVAL`.
            // https://man7.org/linux/man-pages/man3/isatty.3.html#ERRORS
            #[cfg(any(target_os = "android", target_os = "linux"))]
            libc::ENOTTY | libc::EINVAL => false,

            // Darwin mysteriously returns `EOPNOTSUPP` sometimes.
            #[cfg(any(target_os = "ios", target_os = "macos"))]
            libc::EOPNOTSUPP => false,

            err => panic!("unexpected error from isatty: {:?}", err),
        }
    } else {
        true
    }
}

#[cfg(linux_raw)]
fn _isatty(fd: BorrowedFd<'_>) -> bool {
    match crate::linux_raw::ioctl_tiocgwinsz(fd) {
        Ok(_) => true,
        Err(err) => match err {
            #[cfg(not(any(target_os = "android", target_os = "linux")))]
            io::Error::NOTTY => false,

            // Old Linux versions reportedly return `EINVAL`.
            // https://man7.org/linux/man-pages/man3/isatty.3.html#ERRORS
            #[cfg(any(target_os = "android", target_os = "linux"))]
            io::Error::NOTTY | io::Error::INVAL => false,

            // Darwin mysteriously returns `EOPNOTSUPP` sometimes.
            #[cfg(any(target_os = "ios", target_os = "macos"))]
            io::Error::OPNOTSUPP => false,

            _ => panic!("unexpected error from isatty: {:?}", err),
        },
    }
}

/// Returns a pair of booleans indicating whether the file descriptor is
/// readable and/or writeable, respectively. Unlike [`is_file_read_write`],
/// this correctly detects whether sockets have been shutdown, partially or
/// completely.
///
/// [`is_file_read_write`]: crate::fs::is_file_read_write
#[cfg(not(target_os = "redox"))]
#[inline]
pub fn is_read_write<'f, Fd: AsFd<'f>>(fd: Fd) -> io::Result<(bool, bool)> {
    let fd = fd.as_fd();
    _is_read_write(fd)
}

#[cfg(all(libc, not(any(target_os = "redox", target_os = "wasi"))))]
fn _is_read_write(fd: BorrowedFd<'_>) -> io::Result<(bool, bool)> {
    let (mut read, mut write) = crate::fs::fd::_is_file_read_write(fd)?;
    let mut not_socket = false;
    if read {
        // Do a `recv` with `PEEK` and `DONTWAIT` for 1 byte. A 0 indicates
        // the read side is shut down; an `EWOULDBLOCK` indicates the read
        // side is still open.
        match unsafe {
            libc::recv(
                fd.as_raw_fd(),
                MaybeUninit::<[u8; 1]>::uninit()
                    .as_mut_ptr()
                    .cast::<libc::c_void>(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        } {
            0 => read = false,
            -1 => {
                #[allow(unreachable_patterns)] // EAGAIN may equal EWOULDBLOCK
                match errno().0 {
                    libc::EAGAIN | libc::EWOULDBLOCK => (),
                    libc::ENOTSOCK => not_socket = true,
                    err => return Err(io::Error(err)),
                }
            }
            _ => (),
        }
    }
    if write && !not_socket {
        // Do a `send` with `DONTWAIT` for 0 bytes. An `EPIPE` indicates
        // the write side is shut down.
        match unsafe { libc::send(fd.as_raw_fd(), [].as_ptr(), 0, libc::MSG_DONTWAIT) } {
            -1 => {
                #[allow(unreachable_patterns)] // EAGAIN may equal EWOULDBLOCK
                match errno().0 {
                    libc::EAGAIN | libc::EWOULDBLOCK => (),
                    libc::ENOTSOCK => (),
                    libc::EPIPE => write = false,
                    err => return Err(io::Error(err)),
                }
            }
            _ => (),
        }
    }
    Ok((read, write))
}

#[cfg(target_os = "wasi")]
fn _is_read_write(_fd: BorrowedFd<'_>) -> io::Result<(bool, bool)> {
    todo!("Implement is_read_write for WASI in terms of fd_fdstat_get");
}

#[cfg(linux_raw)]
fn _is_read_write(fd: BorrowedFd<'_>) -> io::Result<(bool, bool)> {
    let (mut read, mut write) = crate::fs::fd::_is_file_read_write(fd)?;
    let mut not_socket = false;
    if read {
        // Do a `recv` with `PEEK` and `DONTWAIT` for 1 byte. A 0 indicates
        // the read side is shut down; an `EWOULDBLOCK` indicates the read
        // side is still open.
        let mut buf = MaybeUninit::<u8>::uninit();
        let mut view = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr(), 1) };
        match crate::linux_raw::recv(
            fd,
            &mut view,
            linux_raw_sys::general::MSG_PEEK | linux_raw_sys::general::MSG_DONTWAIT,
        ) {
            Ok(0) => read = false,
            Err(err) => {
                #[allow(unreachable_patterns)] // EAGAIN may equal EWOULDBLOCK
                match err {
                    io::Error::AGAIN | io::Error::WOULDBLOCK => (),
                    io::Error::NOTSOCK => not_socket = true,
                    _ => return Err(err),
                }
            }
            Ok(_) => (),
        }
    }
    if write && !not_socket {
        // Do a `send` with `DONTWAIT` for 0 bytes. An `EPIPE` indicates
        // the write side is shut down.
        match crate::linux_raw::send(fd, &[], linux_raw_sys::general::MSG_DONTWAIT) {
            Err(err) => {
                #[allow(unreachable_patterns)] // EAGAIN equals EWOULDBLOCK
                match err {
                    io::Error::AGAIN | io::Error::WOULDBLOCK => (),
                    io::Error::NOTSOCK => (),
                    io::Error::PIPE => write = false,
                    _ => return Err(err),
                }
            }
            Ok(_) => (),
        }
    }
    Ok((read, write))
}

/// `dup(fd)`
#[cfg(not(target_os = "wasi"))]
#[inline]
pub fn dup<'f, Fd: AsFd<'f>>(fd: Fd) -> io::Result<OwnedFd> {
    let fd = fd.as_fd();
    _dup(fd)
}

#[cfg(all(libc, not(target_os = "wasi")))]
fn _dup(fd: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    unsafe {
        negone_err(libc::dup(fd.as_raw_fd() as libc::c_int)).map(|raw| OwnedFd::from_raw_fd(raw))
    }
}

#[cfg(linux_raw)]
#[inline]
fn _dup(fd: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    crate::linux_raw::dup(fd)
}

/// `ttyname_r(fd)`
///
/// If `reuse` is non-empty, reuse its buffer to store the result if possible.
#[cfg(all(libc, not(any(target_os = "wasi", target_os = "fuchsia"))))]
#[inline]
pub fn ttyname<'f, Fd: AsFd<'f>>(dirfd: Fd, reuse: OsString) -> io::Result<OsString> {
    let dirfd = dirfd.as_fd();
    _ttyname(dirfd, reuse)
}

#[cfg(all(libc, not(any(target_os = "wasi", target_os = "fuchsia"))))]
fn _ttyname(dirfd: BorrowedFd<'_>, reuse: OsString) -> io::Result<OsString> {
    let mut buffer = reuse.into_vec();

    // Start with a buffer big enough for the vast majority of paths.
    // This and the `reserve` below would be a good candidate for `try_reserve`.
    // https://github.com/rust-lang/rust/issues/48043
    buffer.clear();
    buffer.reserve(256);

    unsafe {
        loop {
            match libc::ttyname_r(
                dirfd.as_raw_fd() as libc::c_int,
                buffer.as_mut_ptr().cast::<libc::c_char>(),
                buffer.capacity(),
            ) {
                // Use `Vec`'s builtin capacity-doubling strategy.
                libc::ERANGE => buffer.reserve(1),
                0 => {
                    buffer.set_len(libc::strlen(buffer.as_ptr().cast::<libc::c_char>()));
                    return Ok(OsString::from_vec(buffer));
                }
                errno => return Err(io::Error(errno)),
            }
        }
    }
}
