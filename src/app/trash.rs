//! Handing a working file or folder to the Windows Recycle Bin.
//!
//! "Move To Trash" is a local helper: it names no cvs command, and it has to work for a
//! folder as well as a file. `std::fs::remove_file` only understands files, so the shell
//! does the move.

use std::path::Path;

/// Move one file or folder to the trash, with the reason when it cannot be moved.
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows::recycle(path)
    }
    #[cfg(not(windows))]
    {
        // There is no shared trash API on the other systems; removing the entry is still
        // better than leaving it on screen after the command said it went away.
        if path.is_dir() {
            std::fs::remove_dir_all(path).map_err(|error| error.to_string())
        } else {
            std::fs::remove_file(path).map_err(|error| error.to_string())
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    const FO_DELETE: u32 = 3;
    const FOF_SILENT: u16 = 0x0004;
    const FOF_NOCONFIRMATION: u16 = 0x0010;
    const FOF_ALLOWUNDO: u16 = 0x0040;
    const FOF_NOERRORUI: u16 = 0x0400;

    #[repr(C)]
    struct ShFileOpStructW {
        hwnd: *mut c_void,
        w_func: u32,
        p_from: *const u16,
        p_to: *const u16,
        f_flags: u16,
        f_any_operations_aborted: i32,
        h_name_mappings: *mut c_void,
        lpsz_progress_title: *const u16,
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHFileOperationW(op: *mut ShFileOpStructW) -> i32;
    }

    pub(super) fn recycle(path: &Path) -> Result<(), String> {
        // pFrom is a list of paths, each ending in a null and the whole list ending in a
        // second null. A relative path would be resolved against the shell's own folder,
        // so the working folder is joined before the call.
        let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
        from.extend_from_slice(&[0, 0]);

        let mut op = ShFileOpStructW {
            hwnd: std::ptr::null_mut(),
            w_func: FO_DELETE,
            p_from: from.as_ptr(),
            p_to: std::ptr::null(),
            f_flags: FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI,
            f_any_operations_aborted: 0,
            h_name_mappings: std::ptr::null_mut(),
            lpsz_progress_title: std::ptr::null(),
        };
        let code = unsafe { SHFileOperationW(&mut op) };
        if code != 0 {
            return Err(format!("Windows shell error {}", code));
        }
        if op.f_any_operations_aborted != 0 {
            return Err("the move was cancelled".into());
        }
        Ok(())
    }
}
