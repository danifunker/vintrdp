//! Device redirection ("rdpdr"): drives, printers, serial and parallel
//! ports. Port of rdesktop's rdpdr.c: the client announce, name and
//! capabilities, the device list, and the I/O requests (IRPs) the server
//! sends for each device, with the ones that can't finish at once (serial
//! reads with timeouts, wait masks, directory change notifications) kept
//! pending and completed from [`Channel::poll`].
//!
//! The devices themselves are in `disk`, `printer` (with `printercache`),
//! `serial` and `parallel` (POSIX), and `win32_ports` (Windows).

mod disk;
#[cfg(unix)]
mod parallel;
mod printer;
mod printercache;
#[cfg(unix)]
mod serial;
#[cfg(windows)]
mod win32_ports;

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{Channel, Ctx, OPTION_COMPRESS_RDP, OPTION_INITIALIZED};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::session::Shared;
use crate::stream::{Reader, Writer};

// RDPDR_HEADER components and packet ids.
const CTYP_CORE: u16 = 0x4472;
const CTYP_PRN: u16 = 0x5052;
const PAKID_CORE_SERVER_ANNOUNCE: u16 = 0x496e;
const PAKID_CORE_CLIENTID_CONFIRM: u16 = 0x4343;
const PAKID_CORE_CLIENT_NAME: u16 = 0x434e;
const PAKID_CORE_DEVICE_LIST_ANNOUNCE: u16 = 0x4441;
const PAKID_CORE_DEVICE_REPLY: u16 = 0x6472;
const PAKID_CORE_DEVICE_IOREQUEST: u16 = 0x4952;
const PAKID_CORE_DEVICE_IOCOMPLETION: u16 = 0x4943;
const PAKID_CORE_SERVER_CAPABILITY: u16 = 0x5350;
const PAKID_CORE_CLIENT_CAPABILITY: u16 = 0x4350;
const PAKID_PRN_CACHE_DATA: u16 = 0x5043;

// Device types.
const DEVICE_TYPE_SERIAL: u32 = 0x01;
const DEVICE_TYPE_PARALLEL: u32 = 0x02;
const DEVICE_TYPE_PRINTER: u32 = 0x04;
const DEVICE_TYPE_DISK: u32 = 0x08;

/// rdesktop's limit on redirected devices.
const MAX_DEVICES: usize = 0x10;

// Capability sets.
const CAP_GENERAL_TYPE: u16 = 1;
const CAP_PRINTER_TYPE: u16 = 2;
const CAP_PORT_TYPE: u16 = 3;
const CAP_DRIVE_TYPE: u16 = 4;
const CAP_SMARTCARD_TYPE: u16 = 5;
const ALL_RDPDR_IRP_MJ: u32 = 0xffff;
const RDPDR_DEVICE_REMOVE_PDUS: u32 = 1;
const RDPDR_CLIENT_DISPLAY_NAME_PDU: u32 = 2;
const RDPDR_PRINTER_ANNOUNCE_FLAG_DEFAULTPRINTER: u32 = 2;

// IRP major and minor functions.
const IRP_MJ_CREATE: u32 = 0x00;
const IRP_MJ_CLOSE: u32 = 0x02;
const IRP_MJ_READ: u32 = 0x03;
const IRP_MJ_WRITE: u32 = 0x04;
const IRP_MJ_QUERY_INFORMATION: u32 = 0x05;
const IRP_MJ_SET_INFORMATION: u32 = 0x06;
const IRP_MJ_QUERY_VOLUME_INFORMATION: u32 = 0x0a;
const IRP_MJ_DIRECTORY_CONTROL: u32 = 0x0c;
const IRP_MJ_DEVICE_CONTROL: u32 = 0x0e;
const IRP_MJ_LOCK_CONTROL: u32 = 0x11;
const IRP_MN_QUERY_DIRECTORY: u32 = 0x01;
const IRP_MN_NOTIFY_CHANGE_DIRECTORY: u32 = 0x02;

// NTSTATUS values.
const STATUS_SUCCESS: u32 = 0x0000_0000;
const STATUS_NOT_IMPLEMENTED: u32 = 0x0000_0001;
const STATUS_PENDING: u32 = 0x0000_0103;
const STATUS_NOTIFY_ENUM_DIR: u32 = 0x0000_010c;
const STATUS_NO_MORE_FILES: u32 = 0x8000_0006;
const STATUS_DEVICE_PAPER_EMPTY: u32 = 0x8000_000e;
const STATUS_DEVICE_POWERED_OFF: u32 = 0x8000_000f;
const STATUS_DEVICE_OFF_LINE: u32 = 0x8000_0010;
const STATUS_UNSUCCESSFUL: u32 = 0xc000_0001;
const STATUS_INVALID_HANDLE: u32 = 0xc000_0008;
const STATUS_INVALID_PARAMETER: u32 = 0xc000_000d;
const STATUS_NO_SUCH_FILE: u32 = 0xc000_000f;
const STATUS_INVALID_DEVICE_REQUEST: u32 = 0xc000_0010;
const STATUS_ACCESS_DENIED: u32 = 0xc000_0022;
const STATUS_BUFFER_TOO_SMALL: u32 = 0xc000_0023;
const STATUS_OBJECT_NAME_INVALID: u32 = 0xc000_0033;
const STATUS_OBJECT_NAME_COLLISION: u32 = 0xc000_0035;
const STATUS_LOCK_NOT_GRANTED: u32 = 0xc000_0055;
const STATUS_RANGE_NOT_LOCKED: u32 = 0xc000_007e;
const STATUS_DISK_FULL: u32 = 0xc000_007f;
const STATUS_FILE_IS_A_DIRECTORY: u32 = 0xc000_00ba;
const STATUS_NOT_SUPPORTED: u32 = 0xc000_00bb;
const STATUS_DIRECTORY_NOT_EMPTY: u32 = 0xc000_0101;
const STATUS_TIMEOUT: u32 = 0xc000_0102;
const STATUS_NOT_A_DIRECTORY: u32 = 0xc000_0103;
const STATUS_TOO_MANY_OPENED_FILES: u32 = 0xc000_011f;
const STATUS_CANCELLED: u32 = 0xc000_0120;

/// rdesktop's client id for servers older than version 1.12, which expect the
/// client to make one up.
const CLIENT_ID: u32 = 0x815e_d39d;

/// The most one read or device control may ask us to buffer. Servers ask for
/// 64 KiB at most; this keeps a hostile one from making us allocate gigabytes.
const MAX_IO: u32 = 0x0100_0000;

/// How many handles the server may hold open at once.
const MAX_HANDLES: usize = 1024;

/// How often pending port I/O is polled. Ports can't be waited on alongside
/// the network, so rdesktop polled wait masks every 5 ms; reads go the same way.
const PORT_POLL: Duration = Duration::from_millis(10);

/// How often a pending directory change notification rescans its directory
/// when nothing the server did could have changed it.
const NOTIFY_POLL: Duration = Duration::from_secs(1);

/// A local serial or parallel port, open for the server.
trait Port: Send {
    /// Read what has arrived, without waiting: `WouldBlock` (or, where the
    /// port's reads return at once, `Ok(0)`) when nothing has.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// Write what the port takes without waiting (`WouldBlock` if nothing).
    fn write(&mut self, data: &[u8]) -> io::Result<usize>;

    /// How a read of `length` bytes completes.
    fn read_policy(&self, length: u32) -> ReadPolicy;

    /// The status for a failed read or write.
    fn error_status(&self, _e: &io::Error) -> u32 {
        STATUS_UNSUCCESSFUL
    }

    /// An IOCTL from the server, with at most `out_max` bytes of output.
    fn control(&mut self, code: u32, input: &[u8], out_max: usize) -> Control;

    /// The events in the wait mask that have happened since the last check,
    /// for a pending WAIT_ON_MASK.
    fn event(&mut self) -> Option<u32>;
}

/// How a port's read completes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ReadPolicy {
    /// With whatever has arrived, possibly nothing.
    Immediate,
    /// When the buffer is full, at the total timeout (with what has arrived,
    /// or STATUS_TIMEOUT if nothing), or when `interval` passes between
    /// characters.
    Wait { total: Option<Duration>, interval: Option<Duration> },
}

/// The outcome of a port IOCTL.
#[derive(Debug, PartialEq)]
enum Control {
    Done(Vec<u8>),
    Failed(u32),
    /// WAIT_ON_MASK with nothing to report yet.
    Pending,
    /// SERIAL_PURGE: cancel the pending reads and/or writes.
    Purge { read: bool, write: bool },
}

fn open_port(kind: u32, path: &str) -> io::Result<Box<dyn Port>> {
    #[cfg(unix)]
    {
        if kind == DEVICE_TYPE_SERIAL {
            return Ok(Box::new(serial::Serial::open(path)?));
        }
        Ok(Box::new(parallel::Parallel::open(path)?))
    }
    #[cfg(windows)]
    {
        Ok(Box::new(win32_ports::WinPort::open(path, kind == DEVICE_TYPE_SERIAL)?))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (kind, path);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// A device announced to the server.
struct Device {
    /// The preferred DOS name: "C", "PRN1", "COM1".
    name: String,
    kind: DeviceKind,
}

enum DeviceKind {
    Disk(disk::Drive),
    Printer(printer::Printer),
    /// A serial or parallel port and the local device behind it.
    Port { device_type: u32, path: String },
}

impl Device {
    fn device_type(&self) -> u32 {
        match &self.kind {
            DeviceKind::Disk(_) => DEVICE_TYPE_DISK,
            DeviceKind::Printer(_) => DEVICE_TYPE_PRINTER,
            DeviceKind::Port { device_type, .. } => *device_type,
        }
    }
}

/// What a handle the server holds refers to.
enum Open {
    File(disk::DiskFile),
    Job(crate::printing::PrintJob),
    Port(Box<dyn Port>),
}

struct Handle {
    device: u32,
    open: Open,
}

/// An IRP waiting on a device.
struct Pending {
    device: u32,
    handle: u32,
    id: u32,
    op: PendingOp,
}

enum PendingOp {
    Read { buf: Vec<u8>, filled: usize, total: Option<Instant>, interval: Option<Duration>, last: Instant },
    Write { data: Vec<u8>, done: usize },
    WaitMask,
    Notify,
}

impl PendingOp {
    fn major(&self) -> u32 {
        match self {
            PendingOp::Read { .. } => IRP_MJ_READ,
            PendingOp::Write { .. } => IRP_MJ_WRITE,
            PendingOp::WaitMask => IRP_MJ_DEVICE_CONTROL,
            PendingOp::Notify => IRP_MJ_DIRECTORY_CONTROL,
        }
    }

    fn is_port(&self) -> bool {
        !matches!(self, PendingOp::Notify)
    }
}

/// How an IRP ends right away: the completion's status, its 4-byte result
/// field, and the bytes after it.
struct Reply {
    status: u32,
    result: u32,
    data: Vec<u8>,
}

impl Reply {
    fn status(status: u32) -> Reply {
        Reply { status, result: 0, data: Vec::new() }
    }

    fn with(status: u32, result: u32, data: Vec<u8>) -> Reply {
        Reply { status, result, data }
    }

    /// A reply carrying `data` and its length as the result.
    fn data(status: u32, data: Vec<u8>) -> Reply {
        Reply { status, result: len32(data.len()), data }
    }
}

fn len32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The protocol state, apart from the session so tests can drive it.
struct Core {
    client_name: String,
    client_id: u32,
    devices: Vec<Device>,
    handles: BTreeMap<u32, Handle>,
    next_handle: u32,
    pending: Vec<Pending>,
    locks: disk::Locks,
    /// Something the server did may have changed a directory it watches.
    notify_stamp: bool,
    last_notify_scan: Instant,
    /// Completions an IRP caused for other IRPs (cancelled by a close or a
    /// purge), sent before its own.
    deferred: Vec<Vec<u8>>,
    verbose: bool,
}

impl Core {
    fn new(client_name: &str, devices: Vec<Device>) -> Core {
        // rdesktop's g_hostname held 15 characters.
        let client_name: String = client_name.chars().take(15).collect();
        Core {
            client_name,
            client_id: 0,
            devices,
            handles: BTreeMap::new(),
            next_handle: 1,
            pending: Vec::new(),
            locks: disk::Locks::default(),
            notify_stamp: false,
            last_notify_scan: Instant::now(),
            deferred: Vec::new(),
            verbose: false,
        }
    }

    /// The devices `config` redirects, as rdesktop's session set them up.
    fn devices_from(config: &Config) -> Vec<Device> {
        let mut devices = Vec::new();
        // Files on the clipboard pass through a drive with a letter for a
        // name; that's how the server turns C:\x into \\tsclient\c\x. It only
        // ever holds copies of what's being sent, so start it empty.
        if let Some(dir) = &config.transfer {
            let _ = std::fs::remove_dir_all(dir);
            match std::fs::create_dir_all(dir.join("temp")) {
                Ok(()) => devices.push(disk::device("C", dir.clone())),
                Err(e) => eprintln!("vintrdp: can't create the transfer drive {}: {e}", dir.display()),
            }
        }
        for (name, path) in &config.disks {
            devices.push(disk::device(name, path.clone()));
        }
        for (i, p) in config.printers.iter().enumerate() {
            devices.push(printer::device(i, p));
        }
        for (port, path) in &config.serial_ports {
            devices.push(Device { name: port.to_ascii_uppercase(), kind: DeviceKind::Port { device_type: DEVICE_TYPE_SERIAL, path: path.clone() } });
        }
        for (port, path) in &config.parallel_ports {
            devices.push(Device { name: port.to_ascii_uppercase(), kind: DeviceKind::Port { device_type: DEVICE_TYPE_PARALLEL, path: path.clone() } });
        }
        if devices.len() > MAX_DEVICES {
            eprintln!("vintrdp: only the first {MAX_DEVICES} redirected devices are offered");
            devices.truncate(MAX_DEVICES);
        }
        devices
    }

    fn process(&mut self, data: &[u8], out: &mut Vec<Vec<u8>>) -> Result<()> {
        let mut r = Reader::new(data);
        let component = r.u16()?;
        let pakid = r.u16()?;
        match (component, pakid) {
            (CTYP_CORE, PAKID_CORE_DEVICE_IOREQUEST) => self.process_irp(&mut r, out),
            (CTYP_CORE, PAKID_CORE_SERVER_ANNOUNCE) => {
                r.skip(2)?; // VersionMajor
                let minor = r.u16()?;
                self.client_id = r.u32()?;
                // Servers before 1.12 leave the client id to the client.
                if minor < 0x000c {
                    self.client_id = CLIENT_ID;
                }
                // A new announce starts the sequence over: what the server
                // had open, and what it was waiting for, is gone.
                self.close_all();
                out.push(self.announce_reply());
                out.push(self.name_request());
            }
            (CTYP_CORE, PAKID_CORE_CLIENTID_CONFIRM) => out.push(self.device_list_announce()),
            (CTYP_CORE, PAKID_CORE_DEVICE_REPLY) => {
                let device = r.u32()?;
                let status = r.u32().unwrap_or(0);
                if self.verbose || status != STATUS_SUCCESS {
                    eprintln!("vintrdp: rdpdr: device {device} announced, status {status:#x}");
                }
            }
            (CTYP_CORE, PAKID_CORE_SERVER_CAPABILITY) => out.push(capability_response()),
            (CTYP_PRN, PAKID_PRN_CACHE_DATA) => printercache::process(&mut r)?,
            _ => {
                if self.verbose {
                    eprintln!("vintrdp: rdpdr: unhandled packet {pakid:#x} of component {component:#x}");
                }
            }
        }
        Ok(())
    }

    /// Close everything the server had open, forgetting what was pending.
    fn close_all(&mut self) {
        for (_, h) in std::mem::take(&mut self.handles) {
            if let Open::File(f) = h.open {
                f.close();
            }
        }
        self.pending.clear();
        self.locks = disk::Locks::default();
    }

    /// DR_CORE_CLIENT_ANNOUNCE_RSP.
    fn announce_reply(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(12);
        w.u16(CTYP_CORE).u16(PAKID_CORE_CLIENTID_CONFIRM);
        w.u16(1).u16(5); // version 1.5
        // rdesktop sends the id big-endian; servers have always taken it.
        w.u32_be(self.client_id);
        w.into_vec()
    }

    /// DR_CORE_CLIENT_NAME_REQ.
    fn name_request(&self) -> Vec<u8> {
        let name = crate::text::to_utf16le(&self.client_name, true);
        let mut w = Writer::with_capacity(16 + name.len());
        w.u16(CTYP_CORE).u16(PAKID_CORE_CLIENT_NAME);
        w.u32(1); // UnicodeFlag
        w.u32(0); // CodePage
        w.u32(len32(name.len()));
        w.bytes(&name);
        w.into_vec()
    }

    /// DR_CORE_DEVICELIST_ANNOUNCE_REQ.
    fn device_list_announce(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u16(CTYP_CORE).u16(PAKID_CORE_DEVICE_LIST_ANNOUNCE);
        w.u32(len32(self.devices.len()));
        for (id, dev) in self.devices.iter().enumerate() {
            w.u32(dev.device_type());
            w.u32(len32(id));
            w.bytes(&dos_name(&dev.name));
            match &dev.kind {
                // The spec says DeviceData is a Unicode name, but servers want
                // the name in ASCII, like a longer PreferredDosName.
                DeviceKind::Disk(_) => {
                    let mut name = crate::text::to_cp1252(&dev.name);
                    name.push(0);
                    w.u32(len32(name.len())).bytes(&name);
                }
                DeviceKind::Printer(p) => p.announce(&mut w),
                DeviceKind::Port { .. } => {
                    w.u32(0);
                }
            }
        }
        w.into_vec()
    }

    /// A DR_DEVICE_IOREQUEST (after its RDPDR_HEADER).
    fn process_irp(&mut self, r: &mut Reader, out: &mut Vec<Vec<u8>>) {
        let header = (|| Ok::<_, Error>((r.u32()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?)))();
        let Ok((device, file, id, major, minor)) = header else {
            eprintln!("vintrdp: rdpdr: truncated I/O request");
            return;
        };
        if device as usize >= self.devices.len() {
            eprintln!("vintrdp: rdpdr: I/O request for unknown device {device}");
            return;
        }
        let reply = self.irp(device, file, id, major, minor, r);
        out.append(&mut self.deferred);
        let reply = match reply {
            Ok(Some(reply)) => reply,
            Ok(None) => return, // pending
            Err(_) => {
                eprintln!("vintrdp: rdpdr: malformed I/O request (major {major:#x}, minor {minor:#x})");
                Reply::status(STATUS_INVALID_PARAMETER)
            }
        };
        out.push(completion(device, id, reply.status, reply.result, &reply.data));
    }

    /// Carry out one IRP: its reply, or None if it's now pending.
    fn irp(&mut self, device: u32, file: u32, id: u32, major: u32, minor: u32, r: &mut Reader) -> Result<Option<Reply>> {
        let is_disk = matches!(self.devices[device as usize].kind, DeviceKind::Disk(_));
        if is_disk && !matches!(major, IRP_MJ_READ | IRP_MJ_QUERY_INFORMATION | IRP_MJ_QUERY_VOLUME_INFORMATION) {
            self.notify_stamp = true;
        }
        if major != IRP_MJ_CREATE && !self.handles.get(&file).is_some_and(|h| h.device == device) {
            return Ok(Some(Reply::status(STATUS_INVALID_HANDLE)));
        }
        let reply = match major {
            IRP_MJ_CREATE => self.create(device, r)?,
            IRP_MJ_CLOSE => self.close(file),
            IRP_MJ_READ => {
                let length = r.u32()?;
                let offset = r.u64()?;
                return self.read(device, file, id, length, offset);
            }
            IRP_MJ_WRITE => {
                let length = r.u32()?;
                let offset = r.u64()?;
                r.skip(20)?;
                let data = r.bytes(length as usize)?;
                return self.write(device, file, id, data, offset);
            }
            IRP_MJ_DEVICE_CONTROL => {
                let out_len = r.u32()?;
                let in_len = r.u32()?;
                let code = r.u32()?;
                r.skip(20)?;
                let input = r.bytes(in_len as usize)?;
                return Ok(self.device_control(device, file, id, code, input, out_len));
            }
            _ if !is_disk => Reply::status(if major <= IRP_MJ_LOCK_CONTROL { STATUS_INVALID_HANDLE } else { STATUS_INVALID_DEVICE_REQUEST }),
            IRP_MJ_QUERY_INFORMATION => {
                let class = r.u32()?;
                let f = self.file(file);
                let mut w = Writer::new();
                let status = f.map_or(STATUS_INVALID_HANDLE, |f| f.query_information(class, &mut w));
                Reply::data(status, w.into_vec())
            }
            IRP_MJ_SET_INFORMATION => {
                let class = r.u32()?;
                let length = r.u32()?;
                r.skip(24)?;
                let buf = r.rest();
                let buf = &buf[..buf.len().min(length as usize)];
                let root = self.drive_root(device);
                match self.handles.get_mut(&file) {
                    Some(Handle { open: Open::File(f), .. }) => Reply::status(f.set_information(&root, class, buf)),
                    _ => Reply::status(STATUS_INVALID_HANDLE),
                }
            }
            IRP_MJ_QUERY_VOLUME_INFORMATION => {
                let class = r.u32()?;
                let mut w = Writer::new();
                let status = self.file(file).map_or(STATUS_INVALID_HANDLE, |f| disk::query_volume_information(&f.path, class, &mut w));
                Reply::data(status, w.into_vec())
            }
            IRP_MJ_DIRECTORY_CONTROL => match minor {
                IRP_MN_QUERY_DIRECTORY => {
                    let class = r.u32()?;
                    let initial = r.u8()? != 0;
                    let length = r.u32()?;
                    r.skip(23)?;
                    let pattern = if length > 0 { Some(r.utf16(length as usize)?) } else { None };
                    let mut w = Writer::new();
                    let status = match self.handles.get_mut(&file) {
                        Some(Handle { open: Open::File(f), .. }) => f.query_directory(class, initial, pattern.as_deref(), &mut w),
                        _ => STATUS_INVALID_HANDLE,
                    };
                    // rdesktop always sent at least a padding byte.
                    let mut data = w.into_vec();
                    if data.is_empty() {
                        data.push(0);
                    }
                    Reply::data(status, data)
                }
                IRP_MN_NOTIFY_CHANGE_DIRECTORY => {
                    let _watch_tree = r.u8()?;
                    let _filter = r.u32()?;
                    let status = match self.handles.get_mut(&file) {
                        Some(Handle { open: Open::File(f), .. }) => f.start_notify(),
                        _ => STATUS_INVALID_HANDLE,
                    };
                    if status == STATUS_PENDING {
                        self.pending.push(Pending { device, handle: file, id, op: PendingOp::Notify });
                        return Ok(None);
                    }
                    Reply::status(status)
                }
                _ => Reply::status(STATUS_INVALID_PARAMETER),
            },
            IRP_MJ_LOCK_CONTROL => {
                let op = r.u32()?;
                let _flags = r.u32()?;
                let count = r.u32()?;
                r.skip(20)?;
                let mut ranges = Vec::new();
                for _ in 0..count {
                    let length = r.u64()?;
                    let offset = r.u64()?;
                    ranges.push((offset, length));
                }
                let path = self.file(file).map(|f| f.path.clone()).unwrap_or_default();
                Reply::status(self.locks.apply(&path, file, op, &ranges))
            }
            _ => Reply::status(STATUS_INVALID_DEVICE_REQUEST),
        };
        Ok(Some(reply))
    }

    fn file(&self, handle: u32) -> Option<&disk::DiskFile> {
        match self.handles.get(&handle) {
            Some(Handle { open: Open::File(f), .. }) => Some(f),
            _ => None,
        }
    }

    fn drive_root(&self, device: u32) -> PathBuf {
        match &self.devices[device as usize].kind {
            DeviceKind::Disk(d) => d.root.clone(),
            _ => PathBuf::new(),
        }
    }

    fn create(&mut self, device: u32, r: &mut Reader) -> Result<Reply> {
        // DR_CREATE_REQ
        let access = r.u32()?;
        r.skip(8)?; // AllocationSize
        r.skip(4)?; // FileAttributes
        let _share = r.u32()?;
        let disposition = r.u32()?;
        let options = r.u32()?;
        let length = r.u32()?;
        let name = if length > 0 { r.utf16(length as usize)? } else { String::new() };

        if self.handles.len() >= MAX_HANDLES {
            return Ok(Reply::with(STATUS_TOO_MANY_OPENED_FILES, 0, vec![0]));
        }
        let opened = match &self.devices[device as usize].kind {
            DeviceKind::Disk(d) => disk::DiskFile::create(&d.root, access, disposition, options, &name).map(|(f, info)| (Open::File(f), info)),
            DeviceKind::Printer(p) => p.open().map(|j| (Open::Job(j), 0)),
            DeviceKind::Port { device_type, path } => match open_port(*device_type, path) {
                Ok(port) => Ok((Open::Port(port), 0)),
                Err(e) => {
                    eprintln!("vintrdp: can't open {path}: {e}");
                    Err(STATUS_ACCESS_DENIED)
                }
            },
        };
        Ok(match opened {
            Ok((open, info)) => {
                let handle = self.new_handle();
                self.handles.insert(handle, Handle { device, open });
                Reply::with(STATUS_SUCCESS, handle, vec![info])
            }
            Err(status) => Reply::with(status, 0, vec![0]),
        })
    }

    fn new_handle(&mut self) -> u32 {
        loop {
            let h = self.next_handle;
            self.next_handle = self.next_handle.checked_add(1).unwrap_or(1);
            if h != 0 && !self.handles.contains_key(&h) {
                return h;
            }
        }
    }

    fn close(&mut self, file: u32) -> Reply {
        let Some(h) = self.handles.remove(&file) else {
            return Reply::status(STATUS_INVALID_HANDLE);
        };
        self.locks.release(file);
        let mut out = Vec::new();
        // rdesktop timed out a closed serial port's requests, and cancelled
        // the rest.
        let serial = matches!(self.devices[h.device as usize].kind, DeviceKind::Port { device_type: DEVICE_TYPE_SERIAL, .. });
        self.abort(file, None, if serial { STATUS_TIMEOUT } else { STATUS_CANCELLED }, &mut out);
        self.deferred.append(&mut out);
        match h.open {
            Open::File(f) => Reply::status(f.close()),
            Open::Job(_) | Open::Port(_) => Reply::status(STATUS_SUCCESS),
        }
    }

    /// Complete the pending requests on `handle` (only `major`'s, if given)
    /// with `status`.
    fn abort(&mut self, handle: u32, major: Option<u32>, status: u32, out: &mut Vec<Vec<u8>>) {
        self.pending.retain(|p| {
            if p.handle == handle && major.is_none_or(|m| m == p.op.major()) {
                out.push(completion(p.device, p.id, status, 0, &[0]));
                false
            } else {
                true
            }
        });
    }

    fn read(&mut self, device: u32, file: u32, id: u32, length: u32, offset: u64) -> Result<Option<Reply>> {
        let length = length.min(MAX_IO);
        let Some(h) = self.handles.get_mut(&file) else {
            return Ok(Some(Reply::status(STATUS_INVALID_HANDLE)));
        };
        match &mut h.open {
            Open::File(f) => {
                let (status, data) = f.read(length, offset);
                Ok(Some(Reply::data(status, data)))
            }
            Open::Job(_) => Ok(Some(Reply::status(STATUS_NOT_SUPPORTED))),
            Open::Port(port) => {
                let policy = port.read_policy(length);
                let mut buf = vec![0; length as usize];
                let n = match port.read(&mut buf) {
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => 0,
                    Err(e) => {
                        let status = port.error_status(&e);
                        return Ok(Some(Reply::status(status)));
                    }
                };
                if policy == ReadPolicy::Immediate || n == buf.len() {
                    buf.truncate(n);
                    return Ok(Some(Reply::data(STATUS_SUCCESS, buf)));
                }
                let ReadPolicy::Wait { total, interval } = policy else { unreachable!() };
                let now = Instant::now();
                let total = total.and_then(|t| now.checked_add(t));
                self.pending.push(Pending { device, handle: file, id, op: PendingOp::Read { buf, filled: n, total, interval, last: now } });
                Ok(None)
            }
        }
    }

    fn write(&mut self, device: u32, file: u32, id: u32, data: &[u8], offset: u64) -> Result<Option<Reply>> {
        let Some(h) = self.handles.get_mut(&file) else {
            return Ok(Some(Reply::status(STATUS_INVALID_HANDLE)));
        };
        let (status, n) = match &mut h.open {
            Open::File(f) => f.write(data, offset),
            Open::Job(job) => match job.write(data) {
                Ok(()) => (STATUS_SUCCESS, len32(data.len())),
                Err(_) => (STATUS_INVALID_HANDLE, 0),
            },
            Open::Port(port) => {
                let n = match port.write(data) {
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => 0,
                    Err(e) => return Ok(Some(Reply::with(port.error_status(&e), 0, vec![0]))),
                };
                if n < data.len() {
                    self.pending.push(Pending { device, handle: file, id, op: PendingOp::Write { data: data.to_vec(), done: n } });
                    return Ok(None);
                }
                (STATUS_SUCCESS, len32(n))
            }
        };
        Ok(Some(Reply::with(status, n, vec![0])))
    }

    fn device_control(&mut self, device: u32, file: u32, id: u32, code: u32, input: &[u8], out_len: u32) -> Option<Reply> {
        let out_max = out_len.min(MAX_IO) as usize;
        let control = match self.handles.get_mut(&file).map(|h| &mut h.open) {
            Some(Open::Port(port)) => port.control(code, input, out_max),
            // rdesktop's disk_device_control() turned every request down.
            Some(Open::File(_)) => Control::Failed(STATUS_INVALID_PARAMETER),
            _ => Control::Failed(STATUS_NOT_SUPPORTED),
        };
        Some(match control {
            Control::Done(data) if data.len() > out_max => Reply::status(STATUS_BUFFER_TOO_SMALL),
            Control::Done(data) => Reply::data(STATUS_SUCCESS, data),
            Control::Failed(status) => Reply::status(status),
            Control::Pending => {
                self.pending.push(Pending { device, handle: file, id, op: PendingOp::WaitMask });
                return None;
            }
            Control::Purge { read, write } => {
                let mut out = Vec::new();
                if write {
                    self.abort(file, Some(IRP_MJ_WRITE), STATUS_CANCELLED, &mut out);
                }
                if read {
                    self.abort(file, Some(IRP_MJ_READ), STATUS_CANCELLED, &mut out);
                }
                self.deferred.append(&mut out);
                Reply::status(STATUS_SUCCESS)
            }
        })
    }

    /// Move pending I/O along, completing what has finished.
    fn poll(&mut self, out: &mut Vec<Vec<u8>>) {
        if self.pending.is_empty() {
            return;
        }
        let now = Instant::now();
        let scan = self.notify_stamp || now.duration_since(self.last_notify_scan) >= NOTIFY_POLL;
        // Wait masks first, as rdesktop did: the server should hear that a
        // character arrived before the read that returns it completes.
        for wait_masks in [true, false] {
            let mut i = 0;
            while i < self.pending.len() {
                if matches!(self.pending[i].op, PendingOp::WaitMask) != wait_masks {
                    i += 1;
                    continue;
                }
                match self.progress(i, now, scan) {
                    Some(msg) => {
                        out.push(msg);
                        self.pending.remove(i);
                    }
                    None => i += 1,
                }
            }
        }
        if scan {
            self.notify_stamp = false;
            self.last_notify_scan = now;
        }
    }

    /// Try to finish pending request `i`: its completion if it's done.
    fn progress(&mut self, i: usize, now: Instant, scan: bool) -> Option<Vec<u8>> {
        let p = &mut self.pending[i];
        let (device, id) = (p.device, p.id);
        let Some(h) = self.handles.get_mut(&p.handle) else {
            return Some(completion(device, id, STATUS_CANCELLED, 0, &[0]));
        };
        match (&mut p.op, &mut h.open) {
            (PendingOp::WaitMask, Open::Port(port)) => port.event().map(|ev| completion(device, id, STATUS_SUCCESS, 4, &ev.to_le_bytes())),
            (PendingOp::Read { buf, filled, total, interval, last }, Open::Port(port)) => {
                let mut end = None;
                while *filled < buf.len() {
                    match port.read(&mut buf[*filled..]) {
                        Ok(0) => {
                            end = Some(STATUS_SUCCESS);
                            break;
                        }
                        Ok(n) => {
                            *filled += n;
                            *last = now;
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => {
                            end = Some(if *filled > 0 { STATUS_SUCCESS } else { port.error_status(&e) });
                            break;
                        }
                    }
                }
                let n = *filled;
                if n == buf.len() {
                    end = Some(STATUS_SUCCESS);
                } else if end.is_none() && total.is_some_and(|t| now >= t) {
                    // A read that timed out with nothing is STATUS_TIMEOUT.
                    if n == 0 {
                        return Some(completion(device, id, STATUS_TIMEOUT, 0, &[0]));
                    }
                    end = Some(STATUS_SUCCESS);
                } else if end.is_none() && n > 0 && interval.is_some_and(|iv| now.duration_since(*last) >= iv) {
                    end = Some(STATUS_SUCCESS);
                }
                end.map(|status| completion(device, id, status, len32(n), &buf[..n]))
            }
            (PendingOp::Write { data, done }, Open::Port(port)) => {
                let mut end = None;
                while *done < data.len() {
                    match port.write(&data[*done..]) {
                        Ok(0) => {
                            end = Some(STATUS_SUCCESS);
                            break;
                        }
                        Ok(n) => *done += n,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => {
                            end = Some(if *done > 0 { STATUS_SUCCESS } else { port.error_status(&e) });
                            break;
                        }
                    }
                }
                if *done == data.len() {
                    end = Some(STATUS_SUCCESS);
                }
                end.map(|status| completion(device, id, status, len32(*done), &[0]))
            }
            (PendingOp::Notify, Open::File(f)) => {
                if !scan {
                    return None;
                }
                let status = f.check_notify();
                (status != STATUS_PENDING).then(|| completion(device, id, status, 0, &[]))
            }
            _ => Some(completion(device, id, STATUS_CANCELLED, 0, &[0])),
        }
    }

    fn deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        let mut deadline: Option<Instant> = None;
        if self.pending.iter().any(|p| p.op.is_port()) {
            deadline = now.checked_add(PORT_POLL);
        }
        if self.pending.iter().any(|p| matches!(p.op, PendingOp::Notify)) {
            let at = if self.notify_stamp { Some(now) } else { self.last_notify_scan.checked_add(NOTIFY_POLL) };
            deadline = match (deadline, at) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        deadline
    }
}

/// DR_DEVICE_IOCOMPLETION. A server that left too little room for the
/// output gets STATUS_BUFFER_TOO_SMALL and nothing else.
fn completion(device: u32, id: u32, status: u32, result: u32, data: &[u8]) -> Vec<u8> {
    let (result, data) = if status == STATUS_BUFFER_TOO_SMALL { (0, &[][..]) } else { (result, data) };
    let mut w = Writer::with_capacity(20 + data.len());
    w.u16(CTYP_CORE).u16(PAKID_CORE_DEVICE_IOCOMPLETION);
    w.u32(device).u32(id).u32(status).u32(result).bytes(data);
    w.into_vec()
}

/// A PreferredDosName: at most 7 ASCII characters, NUL-padded to 8 bytes.
fn dos_name(name: &str) -> [u8; 8] {
    let mut out = [0u8; 8];
    for (o, b) in out.iter_mut().zip(crate::text::to_cp1252(name).into_iter().take(7)) {
        *o = b;
    }
    out
}

/// DR_CORE_CAPABILITY_RSP: rdesktop's five capability sets.
fn capability_response() -> Vec<u8> {
    let mut w = Writer::with_capacity(0x50);
    w.u16(CTYP_CORE).u16(PAKID_CORE_CLIENT_CAPABILITY);
    w.u16(5).u16(0); // numCapabilities, padding

    w.u16(CAP_GENERAL_TYPE).u16(0x28);
    w.u32(1); // GENERAL_CAPABILITY_VERSION_01
    w.u32(0).u32(0); // osType, osVersion
    w.u16(1).u16(5); // protocol version 1.5
    w.u32(ALL_RDPDR_IRP_MJ).u32(0); // ioCode1, ioCode2
    w.u32(RDPDR_DEVICE_REMOVE_PDUS | RDPDR_CLIENT_DISPLAY_NAME_PDU);
    w.u32(0).u32(0); // extraFlags1, extraFlags2

    for (cap, version) in [(CAP_PRINTER_TYPE, 1), (CAP_PORT_TYPE, 1), (CAP_DRIVE_TYPE, 2), (CAP_SMARTCARD_TYPE, 1)] {
        w.u16(cap).u16(8).u32(version);
    }
    w.into_vec()
}

pub struct Rdpdr {
    _shared: Arc<Shared>,
    core: Core,
}

impl Rdpdr {
    /// The devices come from `shared.config` (disks, the transfer drive,
    /// printers, serial and parallel ports). The transfer drive is emptied
    /// here, at the start of the session.
    pub fn new(shared: Arc<Shared>) -> Self {
        let config = &shared.config;
        let mut core = Core::new(&config.client_hostname(), Core::devices_from(config));
        core.verbose = config.verbose;
        Rdpdr { _shared: shared, core }
    }

    fn send(out: Vec<Vec<u8>>, ctx: &mut Ctx) {
        for m in out {
            ctx.send(m);
        }
    }
}

impl Channel for Rdpdr {
    fn name(&self) -> &'static str {
        "rdpdr"
    }

    /// rdesktop's options for the channel.
    fn options(&self) -> u32 {
        OPTION_INITIALIZED | OPTION_COMPRESS_RDP
    }

    fn process(&mut self, data: &[u8], ctx: &mut Ctx) -> Result<()> {
        let mut out = Vec::new();
        let result = self.core.process(data, &mut out);
        Self::send(out, ctx);
        result
    }

    fn poll(&mut self, ctx: &mut Ctx) {
        let mut out = Vec::new();
        self.core.poll(&mut out);
        Self::send(out, ctx);
    }

    fn deadline(&self) -> Option<Instant> {
        self.core.deadline()
    }

    fn disconnected(&mut self) {
        self.core.close_all();
    }
}

#[cfg(test)]
mod tests;
