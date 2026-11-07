use std::path::{Path, PathBuf};

use super::*;

/// A fresh directory for one test, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("vintrdp-rdpdr-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(core: &mut Core, msg: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    core.process(msg, &mut out).unwrap();
    out
}

fn irp(device: u32, file: u32, id: u32, major: u32, minor: u32, body: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u16(CTYP_CORE).u16(PAKID_CORE_DEVICE_IOREQUEST);
    w.u32(device).u32(file).u32(id).u32(major).u32(minor).bytes(body);
    w.into_vec()
}

struct Completion {
    device: u32,
    id: u32,
    status: u32,
    result: u32,
    data: Vec<u8>,
}

fn parse(msg: &[u8]) -> Completion {
    let mut r = Reader::new(msg);
    assert_eq!(r.u16().unwrap(), CTYP_CORE);
    assert_eq!(r.u16().unwrap(), PAKID_CORE_DEVICE_IOCOMPLETION);
    Completion { device: r.u32().unwrap(), id: r.u32().unwrap(), status: r.u32().unwrap(), result: r.u32().unwrap(), data: r.take_rest().to_vec() }
}

/// One IRP that completes at once.
fn call(core: &mut Core, device: u32, file: u32, major: u32, minor: u32, body: &[u8]) -> Completion {
    let out = run(core, &irp(device, file, 77, major, minor, body));
    assert_eq!(out.len(), 1, "one completion");
    let c = parse(&out[0]);
    assert_eq!((c.device, c.id), (device, 77));
    c
}

fn create_body(access: u32, disposition: u32, options: u32, name: &str) -> Vec<u8> {
    let name = crate::text::to_utf16le(name, true);
    let mut w = Writer::new();
    w.u32(access).u64(0).u32(0).u32(7).u32(disposition).u32(options).u32(name.len() as u32).bytes(&name);
    w.into_vec()
}

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_OPEN_IF: u32 = 3;
const FILE_DIRECTORY_FILE: u32 = 1;

fn open(core: &mut Core, access: u32, disposition: u32, options: u32, name: &str) -> Completion {
    call(core, 0, 0, IRP_MJ_CREATE, 0, &create_body(access, disposition, options, name))
}

fn disk_core(root: &Path) -> Core {
    Core::new("testhost", vec![disk::device("C", root.to_path_buf())])
}

#[test]
fn announce_and_name() {
    let mut core = Core::new("a-very-long-client-hostname", Vec::new());
    let mut w = Writer::new();
    w.u16(CTYP_CORE).u16(PAKID_CORE_SERVER_ANNOUNCE).u16(1).u16(0x0c).u32(0x1234_5678);
    let out = run(&mut core, w.as_slice());
    assert_eq!(out.len(), 2);
    // Version 1.12 servers choose the id; it goes back big-endian, as rdesktop sent it.
    assert_eq!(out[0], [0x72, 0x44, 0x43, 0x43, 1, 0, 5, 0, 0x12, 0x34, 0x56, 0x78]);
    let mut r = Reader::new(&out[1]);
    assert_eq!((r.u16().unwrap(), r.u16().unwrap()), (CTYP_CORE, PAKID_CORE_CLIENT_NAME));
    assert_eq!((r.u32().unwrap(), r.u32().unwrap()), (1, 0));
    let len = r.u32().unwrap() as usize;
    assert_eq!(len, 32); // 15 characters and a NUL
    assert_eq!(r.utf16(len).unwrap(), "a-very-long-cli");
    assert!(r.is_empty());

    // Older servers leave the id to us.
    let mut w = Writer::new();
    w.u16(CTYP_CORE).u16(PAKID_CORE_SERVER_ANNOUNCE).u16(1).u16(2).u32(9);
    let out = run(&mut core, w.as_slice());
    assert_eq!(&out[0][8..], &CLIENT_ID.to_be_bytes());
}

#[test]
fn capabilities() {
    let mut core = Core::new("x", Vec::new());
    let mut w = Writer::new();
    w.u16(CTYP_CORE).u16(PAKID_CORE_SERVER_CAPABILITY).u16(0).u16(0);
    let out = run(&mut core, w.as_slice());
    assert_eq!(out.len(), 1);
    let mut r = Reader::new(&out[0]);
    assert_eq!((r.u16().unwrap(), r.u16().unwrap()), (CTYP_CORE, PAKID_CORE_CLIENT_CAPABILITY));
    assert_eq!(r.u16().unwrap(), 5);
    r.skip(2).unwrap();
    let mut types = Vec::new();
    while !r.is_empty() {
        let (t, len) = (r.u16().unwrap(), r.u16().unwrap());
        types.push(t);
        r.skip(len as usize - 4).unwrap();
    }
    assert_eq!(types, [1, 2, 3, 4, 5]);
    assert_eq!(out[0].len(), 8 + 0x28 + 4 * 8);
}

#[test]
fn device_list() {
    let printer = printer::Printer { name: "mydeskjet".into(), driver: "MS Publisher Imagesetter".into(), default: true };
    let devices = vec![
        disk::device("Transfer", PathBuf::from("/nonexistent")),
        Device { name: "PRN1".into(), kind: DeviceKind::Printer(printer) },
        Device { name: "COM1".into(), kind: DeviceKind::Port { device_type: DEVICE_TYPE_SERIAL, path: "/dev/null".into() } },
    ];
    let mut core = Core::new("x", devices);
    let mut w = Writer::new();
    w.u16(CTYP_CORE).u16(PAKID_CORE_CLIENTID_CONFIRM).u16(1).u16(5).u32(1);
    let out = run(&mut core, w.as_slice());
    assert_eq!(out.len(), 1);
    let mut r = Reader::new(&out[0]);
    assert_eq!((r.u16().unwrap(), r.u16().unwrap()), (CTYP_CORE, PAKID_CORE_DEVICE_LIST_ANNOUNCE));
    assert_eq!(r.u32().unwrap(), 3);

    // The drive: its DOS name cut to 7 characters, the whole name as ASCII.
    assert_eq!((r.u32().unwrap(), r.u32().unwrap()), (DEVICE_TYPE_DISK, 0));
    assert_eq!(r.bytes(8).unwrap(), b"Transfe\0");
    assert_eq!(r.u32().unwrap(), 9);
    assert_eq!(r.bytes(9).unwrap(), b"Transfer\0");

    // The printer.
    assert_eq!((r.u32().unwrap(), r.u32().unwrap()), (DEVICE_TYPE_PRINTER, 1));
    assert_eq!(r.bytes(8).unwrap(), b"PRN1\0\0\0\0");
    let len = r.u32().unwrap() as usize;
    let mut d = r.sub(len).unwrap();
    assert_eq!(d.u32().unwrap(), RDPDR_PRINTER_ANNOUNCE_FLAG_DEFAULTPRINTER);
    assert_eq!((d.u32().unwrap(), d.u32().unwrap()), (0, 0));
    let (driver, name, cached) = (d.u32().unwrap() as usize, d.u32().unwrap() as usize, d.u32().unwrap() as usize);
    assert_eq!(d.utf16(driver).unwrap(), "MS Publisher Imagesetter");
    assert_eq!(d.utf16(name).unwrap(), "mydeskjet");
    assert_eq!(d.remaining(), cached);
    assert_eq!(len, 24 + driver + name + cached);

    // The port: no data.
    assert_eq!((r.u32().unwrap(), r.u32().unwrap()), (DEVICE_TYPE_SERIAL, 2));
    assert_eq!(r.bytes(8).unwrap(), b"COM1\0\0\0\0");
    assert_eq!(r.u32().unwrap(), 0);
    assert!(r.is_empty());
}

#[test]
fn file_round_trip() {
    let tmp = TempDir::new("files");
    let mut core = disk_core(&tmp.0);

    // Create a directory and a file in it.
    let c = open(&mut core, GENERIC_READ, FILE_CREATE, FILE_DIRECTORY_FILE, "\\sub");
    assert_eq!((c.status, c.data.as_slice()), (STATUS_SUCCESS, &[2u8][..]));
    let dir = c.result;
    assert_eq!(call(&mut core, 0, dir, IRP_MJ_CLOSE, 0, &[0; 32]).status, STATUS_SUCCESS);
    assert!(tmp.0.join("sub").is_dir());

    let c = open(&mut core, GENERIC_READ | GENERIC_WRITE, FILE_CREATE, 0, "\\sub\\Hello.txt");
    assert_eq!((c.status, c.data.as_slice()), (STATUS_SUCCESS, &[2u8][..]));
    let file = c.result;
    assert_ne!(file, 0);
    // Creating it again collides.
    assert_eq!(open(&mut core, GENERIC_WRITE, FILE_CREATE, 0, "\\sub\\Hello.txt").status, STATUS_OBJECT_NAME_COLLISION);

    // Write at an offset, then read it back.
    let mut w = Writer::new();
    w.u32(5).u64(2).zeros(20).bytes(b"world");
    let c = call(&mut core, 0, file, IRP_MJ_WRITE, 0, w.as_slice());
    assert_eq!((c.status, c.result, c.data.as_slice()), (STATUS_SUCCESS, 5, &[0u8][..]));
    let mut w = Writer::new();
    w.u32(100).u64(0).zeros(20);
    let c = call(&mut core, 0, file, IRP_MJ_READ, 0, w.as_slice());
    assert_eq!((c.status, c.result, c.data.as_slice()), (STATUS_SUCCESS, 7, &b"\0\0world"[..]));

    // FileStandardInformation: the size, and not a directory.
    let c = call(&mut core, 0, file, IRP_MJ_QUERY_INFORMATION, 0, &5u32.to_le_bytes());
    assert_eq!((c.status, c.result as usize), (STATUS_SUCCESS, c.data.len()));
    let mut r = Reader::new(&c.data);
    assert_eq!((r.u64().unwrap(), r.u64().unwrap()), (7, 7));
    r.skip(5).unwrap();
    assert_eq!(r.u8().unwrap(), 0);

    // Volume information answers.
    let c = call(&mut core, 0, file, IRP_MJ_QUERY_VOLUME_INFORMATION, 0, &5u32.to_le_bytes());
    assert_eq!(c.status, STATUS_SUCCESS);
    let c = call(&mut core, 0, file, IRP_MJ_QUERY_VOLUME_INFORMATION, 0, &7u32.to_le_bytes());
    assert_eq!((c.status, c.data.len()), (STATUS_SUCCESS, 32));

    assert_eq!(call(&mut core, 0, file, IRP_MJ_CLOSE, 0, &[0; 32]).status, STATUS_SUCCESS);
    assert_eq!(std::fs::read(tmp.0.join("sub/Hello.txt")).unwrap(), b"\0\0world");
    // The handle is gone.
    assert_eq!(call(&mut core, 0, file, IRP_MJ_READ, 0, &[0; 32]).status, STATUS_INVALID_HANDLE);
}

fn query_directory(core: &mut Core, dir: u32, class: u32, pattern: Option<&str>) -> Completion {
    let mut w = Writer::new();
    w.u32(class).u8(u8::from(pattern.is_some()));
    match pattern {
        Some(p) => {
            let p = crate::text::to_utf16le(p, true);
            w.u32(p.len() as u32).zeros(23).bytes(&p);
        }
        None => {
            w.u32(0).zeros(23);
        }
    }
    call(core, 0, dir, IRP_MJ_DIRECTORY_CONTROL, IRP_MN_QUERY_DIRECTORY, w.as_slice())
}

/// The name in a FileBothDirectoryInformation entry.
fn both_name(data: &[u8]) -> String {
    let mut r = Reader::new(data);
    r.skip(8 + 32 + 16).unwrap();
    let _attributes = r.u32().unwrap();
    let len = r.u32().unwrap() as usize;
    r.skip(4 + 1 + 24).unwrap();
    String::from_utf16_lossy(&r.bytes(len).unwrap().chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>())
}

#[test]
fn directory_queries() {
    let tmp = TempDir::new("dirs");
    for f in ["a.txt", "B.TXT", "c.doc"] {
        std::fs::write(tmp.0.join(f), f).unwrap();
    }
    let mut core = disk_core(&tmp.0);
    let dir = open(&mut core, GENERIC_READ, FILE_OPEN, FILE_DIRECTORY_FILE, "\\").result;

    let mut names = Vec::new();
    let mut c = query_directory(&mut core, dir, 3, Some("\\*.txt"));
    while c.status == STATUS_SUCCESS {
        assert_eq!(c.result as usize, c.data.len());
        names.push(both_name(&c.data));
        c = query_directory(&mut core, dir, 3, None);
    }
    assert_eq!(c.status, STATUS_NO_MORE_FILES);
    names.sort();
    assert_eq!(names, ["B.TXT", "a.txt"]);

    // A new pattern starts over; "*" lists "." and ".." too.
    let mut count = 0;
    let mut c = query_directory(&mut core, dir, 12, Some("\\*"));
    while c.status == STATUS_SUCCESS {
        count += 1;
        c = query_directory(&mut core, dir, 12, None);
    }
    assert_eq!(count, 5);
    // Unknown classes are refused.
    assert_eq!(query_directory(&mut core, dir, 99, Some("\\*")).status, STATUS_INVALID_PARAMETER);
}

#[test]
fn rename_and_delete() {
    let tmp = TempDir::new("rename");
    let mut core = disk_core(&tmp.0);
    let file = open(&mut core, GENERIC_READ | GENERIC_WRITE, FILE_OPEN_IF, 0, "\\old.txt").result;

    let set = |class: u32, buf: &[u8]| {
        let mut w = Writer::new();
        w.u32(class).u32(buf.len() as u32).zeros(24).bytes(buf);
        w.into_vec()
    };
    let rename = |name: &str| {
        let name = crate::text::to_utf16le(name, false);
        let mut w = Writer::new();
        w.u8(1).u8(0).u32(name.len() as u32).bytes(&name);
        set(10, w.as_slice())
    };
    // The server can't rename out of the drive.
    let c = call(&mut core, 0, file, IRP_MJ_SET_INFORMATION, 0, &rename("\\..\\escaped.txt"));
    assert_eq!(c.status, STATUS_ACCESS_DENIED);
    let c = call(&mut core, 0, file, IRP_MJ_SET_INFORMATION, 0, &rename("\\new.txt"));
    assert_eq!(c.status, STATUS_SUCCESS);
    assert!(tmp.0.join("new.txt").exists() && !tmp.0.join("old.txt").exists());

    // Grow it, then delete it on close.
    let c = call(&mut core, 0, file, IRP_MJ_SET_INFORMATION, 0, &set(20, &10u64.to_le_bytes()));
    assert_eq!(c.status, STATUS_SUCCESS);
    assert_eq!(std::fs::metadata(tmp.0.join("new.txt")).unwrap().len(), 10);
    let c = call(&mut core, 0, file, IRP_MJ_SET_INFORMATION, 0, &set(13, &[1]));
    assert_eq!(c.status, STATUS_SUCCESS);
    assert_eq!(call(&mut core, 0, file, IRP_MJ_CLOSE, 0, &[0; 32]).status, STATUS_SUCCESS);
    assert!(!tmp.0.join("new.txt").exists());
}

#[test]
fn escapes_are_refused() {
    let tmp = TempDir::new("escape");
    let mut core = disk_core(&tmp.0);
    for name in ["\\..", "\\..\\x", "\\a\\..\\..\\x", "/../x"] {
        assert_eq!(open(&mut core, GENERIC_READ, FILE_OPEN, 0, name).status, STATUS_ACCESS_DENIED, "{name}");
    }
}

#[test]
fn path_sanitising() {
    let root = Path::new("/share");
    assert_eq!(disk::resolve(root, "\\a\\b.txt"), Some(PathBuf::from("/share/a/b.txt")));
    assert_eq!(disk::resolve(root, "\\a\\.\\\\b\\"), Some(PathBuf::from("/share/a/b")));
    assert_eq!(disk::resolve(root, ""), Some(PathBuf::from("/share")));
    assert_eq!(disk::resolve(root, "\\a\\..\\b"), None);
    assert_eq!(disk::resolve(root, "\\a/../../b"), None);
    assert_eq!(disk::resolve(root, "\\a\0b"), None);
    // "b.." and "..b" are ordinary names.
    assert!(disk::resolve(root, "\\b..\\..b").is_some());
}

#[test]
fn windows_path_components() {
    use disk::component_ok;
    for bad in ["..", "C:", "a:stream", ". .", "...", "CON", "con.txt", "Com1", "LPT9.log", "nul ", "a|b", "a*"] {
        assert!(!component_ok(bad, true), "{bad}");
    }
    for good in ["COM10", "CONSOLE", "a.b", "...b", "b..."] {
        assert!(component_ok(good, true), "{good}");
    }
    // Elsewhere only ".." matters.
    assert!(component_ok("CON", false) && component_ok("a:b", false));
}

#[test]
fn wildcards() {
    use disk::wildcard_match as m;
    assert!(m("*", "anything") && m("*.*", "noext"));
    assert!(m("*.txt", "A.TXT") && !m("*.txt", "a.txt.bak"));
    assert!(m("a?c", "abc") && !m("a?c", "ac"));
    assert!(m("readme", "README"));
    // DOS_STAR stops at the last dot; DOS_QM and DOS_DOT match nothing at the end.
    assert!(m("<.txt", "a.b.txt") && m("<", "ab") && !m("<", "a.b"));
    assert!(m("a>>", "a") && m("a>>", "abc") && !m("a>>", "abcd"));
    assert!(m("a\"", "a") && m("a\"txt", "a.txt") && !m("a\"txt", "atxt"));
    // A hostile pattern doesn't take forever.
    let long = "*a".repeat(500);
    assert!(!m(&long, &"a".repeat(255)));
}

#[test]
fn notify_change() {
    let tmp = TempDir::new("notify");
    let mut core = disk_core(&tmp.0);
    let dir = open(&mut core, GENERIC_READ, FILE_OPEN, FILE_DIRECTORY_FILE, "\\").result;
    let mut w = Writer::new();
    w.u8(0).u32(0x17).zeros(27);
    let out = run(&mut core, &irp(0, dir, 5, IRP_MJ_DIRECTORY_CONTROL, IRP_MN_NOTIFY_CHANGE_DIRECTORY, w.as_slice()));
    assert!(out.is_empty(), "pending");
    assert!(core.deadline().is_some());

    let mut out = Vec::new();
    core.poll(&mut out);
    assert!(out.is_empty());

    // A file created through the drive wakes it.
    let f = open(&mut core, GENERIC_WRITE, FILE_CREATE, 0, "\\new").result;
    core.poll(&mut out);
    assert_eq!(out.len(), 1);
    let c = parse(&out[0]);
    assert_eq!((c.id, c.status), (5, STATUS_NOTIFY_ENUM_DIR));
    assert!(core.deadline().is_none());

    // Closing a directory cancels its pending notification first.
    run(&mut core, &irp(0, dir, 6, IRP_MJ_DIRECTORY_CONTROL, IRP_MN_NOTIFY_CHANGE_DIRECTORY, w.as_slice()));
    let out = run(&mut core, &irp(0, dir, 7, IRP_MJ_CLOSE, 0, &[0; 32]));
    let (a, b) = (parse(&out[0]), parse(&out[1]));
    assert_eq!((a.id, a.status, b.id, b.status), (6, STATUS_CANCELLED, 7, STATUS_SUCCESS));
    call(&mut core, 0, f, IRP_MJ_CLOSE, 0, &[0; 32]);
}

#[test]
fn locks() {
    let tmp = TempDir::new("locks");
    let mut core = disk_core(&tmp.0);
    let a = open(&mut core, GENERIC_READ | GENERIC_WRITE, FILE_OPEN_IF, 0, "\\f").result;
    let b = open(&mut core, GENERIC_READ, FILE_OPEN, 0, "\\f").result;
    let lock = |op: u32, offset: u64, length: u64| {
        let mut w = Writer::new();
        w.u32(op).u32(0).u32(1).zeros(20).u64(length).u64(offset);
        w.into_vec()
    };
    assert_eq!(call(&mut core, 0, a, IRP_MJ_LOCK_CONTROL, 0, &lock(3, 0, 10)).status, STATUS_SUCCESS);
    assert_eq!(call(&mut core, 0, b, IRP_MJ_LOCK_CONTROL, 0, &lock(2, 5, 10)).status, STATUS_LOCK_NOT_GRANTED);
    assert_eq!(call(&mut core, 0, b, IRP_MJ_LOCK_CONTROL, 0, &lock(2, 10, 10)).status, STATUS_SUCCESS);
    assert_eq!(call(&mut core, 0, b, IRP_MJ_LOCK_CONTROL, 0, &lock(4, 0, 10)).status, STATUS_RANGE_NOT_LOCKED);
    // Closing releases a handle's locks.
    call(&mut core, 0, a, IRP_MJ_CLOSE, 0, &[0; 32]);
    assert_eq!(call(&mut core, 0, b, IRP_MJ_LOCK_CONTROL, 0, &lock(3, 0, 5)).status, STATUS_SUCCESS);
}

#[test]
fn hostile_requests() {
    let tmp = TempDir::new("hostile");
    let mut core = disk_core(&tmp.0);
    // Truncated: a completion saying so, not a panic.
    let c = call(&mut core, 0, 0, IRP_MJ_CREATE, 0, &[1, 2, 3]);
    assert_eq!(c.status, STATUS_INVALID_PARAMETER);
    // A write claiming more data than it carries.
    let file = open(&mut core, GENERIC_WRITE, FILE_CREATE, 0, "\\w").result;
    let mut w = Writer::new();
    w.u32(u32::MAX).u64(0).zeros(20).bytes(b"abc");
    assert_eq!(call(&mut core, 0, file, IRP_MJ_WRITE, 0, w.as_slice()).status, STATUS_INVALID_PARAMETER);
    // A read asking for 4 GiB gets what the file has.
    let mut w = Writer::new();
    w.u32(u32::MAX).u64(u64::MAX).zeros(20);
    assert_eq!(call(&mut core, 0, file, IRP_MJ_READ, 0, w.as_slice()).status, STATUS_SUCCESS);
    // Unknown devices are ignored, as rdesktop did; unknown handles refused.
    assert!(run(&mut core, &irp(9, 0, 1, IRP_MJ_READ, 0, &[0; 32])).is_empty());
    assert_eq!(call(&mut core, 0, 12345, IRP_MJ_READ, 0, &[0; 32]).status, STATUS_INVALID_HANDLE);

    // Garbage of every length never panics.
    let mut seed = 0x1234_5678u32;
    for len in 0..300 {
        let mut msg = vec![0x72, 0x44, 0x52, 0x49];
        for _ in 0..len {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            msg.push((seed >> 16) as u8);
        }
        if msg.len() > 8 {
            msg[4..8].copy_from_slice(&0u32.to_le_bytes()); // device 0
        }
        let mut out = Vec::new();
        let _ = core.process(&msg, &mut out);
        core.poll(&mut out);
    }
}

#[test]
fn printer_cache_parses() {
    // Names that would leave the cache are ignored, and short messages fail
    // cleanly.
    let mut w = Writer::new();
    let name = crate::text::to_utf16le("../../evil", true);
    w.u32(3).u32(name.len() as u32).bytes(&name);
    let mut r = Reader::new(w.as_slice());
    printercache::process(&mut r).unwrap();
    let mut r = Reader::new(&[2, 0, 0, 0, 0xff, 0xff]);
    assert!(printercache::process(&mut r).is_err());
}
