//! Enough of `libloading` 0.8 for `ash` to load `vulkan-1.dll`.
//!
//! The real crate's Windows backend uses raw-dylib, and this GNU toolchain's
//! dlltool cannot spawn an assembler. `LoadLibraryW` comes from the import
//! library rustc already ships.
use std::ffi::{CString, OsStr};
use std::os::raw::c_void;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const i8) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}

#[cfg(unix)]
#[link(name = "dl")]
extern "C" {
    fn dlopen(filename: *const i8, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const i8) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> i32;
}

#[derive(Debug)]
pub struct Error {
    message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub struct Library(*mut c_void);

unsafe impl Send for Library {}
unsafe impl Sync for Library {}

impl Library {
    /// # Safety
    /// `filename` is a dynamic library. The returned library owns the handle.
    pub unsafe fn new<P: AsRef<OsStr>>(filename: P) -> Result<Self, Error> {
        let handle = unsafe { open(filename.as_ref()) };
        if handle.is_null() {
            Err(Error {
                message: format!(
                    "could not load {}",
                    filename.as_ref().to_string_lossy()
                ),
            })
        } else {
            Ok(Self(handle))
        }
    }

    /// # Safety
    /// `symbol` is a NUL-terminated name. `T` is the symbol's type.
    pub unsafe fn get<T>(&self, symbol: &[u8]) -> Result<Symbol<'_, T>, Error> {
        let name = CString::from_vec_with_nul(symbol.to_vec()).map_err(|e| Error {
            message: e.to_string(),
        })?;
        let ptr = unsafe { lookup(self.0, name.as_ptr()) };
        if ptr.is_null() {
            Err(Error {
                message: format!("GetProcAddress failed for {}", String::from_utf8_lossy(symbol)),
            })
        } else {
            Ok(Symbol {
                pointer: ptr,
                library: std::marker::PhantomData,
            })
        }
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        unsafe {
            close(self.0);
        }
    }
}

pub struct Symbol<'lib, T> {
    /// The symbol address. `Deref` reads this field as a `T`, which is how a
    /// function pointer is returned: the address itself is the pointer value.
    pointer: *mut c_void,
    library: std::marker::PhantomData<&'lib T>,
}

impl<T> std::ops::Deref for Symbol<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*(&self.pointer as *const *mut c_void).cast::<T>() }
    }
}

#[cfg(windows)]
unsafe fn open(filename: &OsStr) -> *mut c_void {
    use std::os::windows::ffi::OsStrExt;
    let mut wide: Vec<u16> = filename.encode_wide().collect();
    wide.push(0);
    LoadLibraryW(wide.as_ptr())
}

#[cfg(unix)]
unsafe fn open(filename: &OsStr) -> *mut c_void {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = filename.as_bytes().to_vec();
    bytes.push(0);
    dlopen(bytes.as_ptr().cast(), 1)
}

#[cfg(windows)]
unsafe fn lookup(module: *mut c_void, name: *const i8) -> *mut c_void {
    GetProcAddress(module, name)
}

#[cfg(unix)]
unsafe fn lookup(module: *mut c_void, name: *const i8) -> *mut c_void {
    dlsym(module, name)
}

#[cfg(windows)]
unsafe fn close(module: *mut c_void) {
    FreeLibrary(module);
}

#[cfg(unix)]
unsafe fn close(module: *mut c_void) {
    dlclose(module);
}

unsafe impl<T: Send> Send for Symbol<'_, T> {}
unsafe impl<T: Sync> Sync for Symbol<'_, T> {}
