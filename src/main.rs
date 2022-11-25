use std::ffi::OsStr;
use std::io::{Error, Read, Write};
use std::os::windows::io::IntoRawHandle;
use std::os::windows::prelude::OsStrExt;
use std::{mem, ptr};

//mod child;

use miow::pipe::{AnonRead, AnonWrite};
use windows_sys::core::PWSTR;
use windows_sys::Win32::Foundation::{HANDLE, S_OK};
use windows_sys::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, COORD, HPCON};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, InitializeProcThreadAttributeList, UpdateProcThreadAttribute,
    EXTENDED_STARTUPINFO_PRESENT, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
};

/// RAII Pseudoconsole.
pub struct Conpty {
    pub handle: HPCON,
}

pub struct ConptyHolder {
    conpty: Conpty,
    conout: AnonRead,
    conin: AnonWrite,
}

impl Drop for Conpty {
    fn drop(&mut self) {
        // XXX: This will block until the conout pipe is drained. Will cause a deadlock if the
        // conout pipe has already been dropped by this point.
        //
        // See PR #3084 and https://docs.microsoft.com/en-us/windows/console/closepseudoconsole.
        unsafe { ClosePseudoConsole(self.handle) }
    }
}

pub fn new() -> ConptyHolder {
    let mut pty_handle: HPCON = 0;

    // Passing 0 as the size parameter allows the "system default" buffer
    // size to be used. There may be small performance and memory advantages
    // to be gained by tuning this in the future, but it's likely a reasonable
    // start point.
    let (conout, conout_pty_handle) = miow::pipe::anonymous(0).unwrap();
    let (conin_pty_handle, conin) = miow::pipe::anonymous(0).unwrap();

    // Create the Pseudo Console, using the pipes.
    let result = unsafe {
        CreatePseudoConsole(
            COORD { X: 80, Y: 25 },
            conin_pty_handle.into_raw_handle() as HANDLE,
            conout_pty_handle.into_raw_handle() as HANDLE,
            0,
            &mut pty_handle as *mut _,
        )
    };

    assert_eq!(result, S_OK);

    let mut success;

    // Prepare child process startup info.

    let mut size: usize = 0;

    let mut startup_info_ex: STARTUPINFOEXW = unsafe { mem::zeroed() };

    startup_info_ex.StartupInfo.lpTitle = std::ptr::null_mut() as PWSTR;

    startup_info_ex.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;

    // Setting this flag but leaving all the handles as default (null) ensures the
    // PTY process does not inherit any handles from this Alacritty process.
    startup_info_ex.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;

    // Create the appropriately sized thread attribute list.
    unsafe {
        let failure =
            InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size as *mut usize) > 0;

        // This call was expected to return false.
        if failure {
            panic_shell_spawn();
        }
    }

    let mut attr_list: Box<[u8]> = vec![0; size].into_boxed_slice();

    // Set startup info's attribute list & initialize it
    //
    // Lint failure is spurious; it's because winapi's definition of PROC_THREAD_ATTRIBUTE_LIST
    // implies it is one pointer in size (32 or 64 bits) but really this is just a dummy value.
    // Casting a *mut u8 (pointer to 8 bit type) might therefore not be aligned correctly in
    // the compiler's eyes.
    #[allow(clippy::cast_ptr_alignment)]
    {
        startup_info_ex.lpAttributeList = attr_list.as_mut_ptr() as _;
    }

    unsafe {
        success = InitializeProcThreadAttributeList(
            startup_info_ex.lpAttributeList,
            1,
            0,
            &mut size as *mut usize,
        ) > 0;

        if !success {
            panic_shell_spawn();
        }
    }

    // Set thread attribute list's Pseudo Console to the specified ConPTY.
    unsafe {
        success = UpdateProcThreadAttribute(
            startup_info_ex.lpAttributeList,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            pty_handle as *mut std::ffi::c_void,
            mem::size_of::<HPCON>(),
            ptr::null_mut(),
            ptr::null_mut(),
        ) > 0;

        if !success {
            panic_shell_spawn();
        }
    }

    let cmdline = win32_string("powershell");
    let cwd: &Option<Vec<u16>> = &None;

    let mut proc_info: PROCESS_INFORMATION = unsafe { mem::zeroed() };
    unsafe {
        success = CreateProcessW(
            ptr::null(),
            cmdline.as_ptr() as PWSTR,
            ptr::null_mut(),
            ptr::null_mut(),
            false as i32,
            EXTENDED_STARTUPINFO_PRESENT,
            ptr::null_mut(),
            cwd.as_ref().map_or_else(ptr::null, |s| s.as_ptr()),
            &mut startup_info_ex.StartupInfo as *mut STARTUPINFOW,
            &mut proc_info as *mut PROCESS_INFORMATION,
        ) > 0;

        if !success {
            panic_shell_spawn();
        }
    }

    //let child_watcher = ChildExitWatcher::new(proc_info.hProcess).unwrap();
    let conpty = Conpty {
        handle: pty_handle as HPCON,
    };

    return ConptyHolder {
        conpty,
        conout,
        conin,
    };
}

// Panic with the last os error as message.
fn panic_shell_spawn() {
    panic!("Unable to spawn shell: {}", Error::last_os_error());
}

/// Converts the string slice into a Windows-standard representation for "W"-
/// suffixed function variants, which accept UTF-16 encoded string values.
pub fn win32_string<S: AsRef<OsStr> + ?Sized>(value: &S) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

const CMD:&str = "date\recho 0123456789abcdefghijklmnopqrst0123456789abcdefghijklmnopqrst0123456789abcdefghijklmnopqrst0123456789abcdefghijklmnopqrst0123456789abcdefghijklmnopqrst0123456789abcdefghijklmnopqrst0123456789abcdefghijklmnopqrst0123456789abcdefghijklmnopqrst0123456789abcdefghijklmnopqrstENDENDENDEND\r";

fn main() {
    let mut conpty = new();
    let write_res = conpty.conin.write(CMD.as_bytes());
    println!("Write: {:?}", write_res);
    let mut f = std::fs::File::create("log.log").expect("create log file failed");
    loop {
        let mut buf = [0u8; 1];
        conpty.conout.read(&mut buf).expect("Read failed");
        let c = if buf[0] >= 32 && buf[0] <= 127 {
            f.write(&buf).expect("write file failed");
            buf[0] as char
        } else {
            f.write(format!("\\x{:02x}", buf[0]).as_bytes())
                .expect("write file failed");
            '#'
        };
        println!("Read: {0} {1:x}", c, buf[0]);
        f.flush().expect("flush file failed");
    }
}
