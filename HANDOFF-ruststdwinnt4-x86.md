# Handoff: ruststdwinnt4's x86 target (`i686-nt4-windows-msvc`)

You're picking up work in `~/repos/ruststdwinnt4` on m900 (x86_64 Ubuntu). Read its
README first, especially the new "The x86 target" section. The repo's rules still apply:
only functions NT 4.0 SP0 exports (`tools/peinfo.py --defs` checks every image), no
Microsoft code, Rust 1.90.

## Where things stand

Branch **`x86`**, commit **`61b2905`** "x86 Windows NT 4.0: the i686-nt4-windows-msvc
target", on top of `main` (`1a48f02`). It is committed but **not merged and not pushed**.
`main` is untouched.

It adds an x86 target alongside PINT:

- **`targets/i686-nt4-windows-msvc.json`**: Rust's `i686-pc-windows-msvc` with vendor
  `nt4`, cpu `pentium`, features `-mmx,-sse,-sse2` (NT 4.0 never enables SSE), no
  `rustc-abi` (x87 floats), no implicit TLS, panic abort, `/OSVERSION:4.0
  /SUBSYSTEM:CONSOLE,4.0`, msvc-lld.
- **`imports/x86/*.def`**: made by `tools/gen-defs.py --arch x86` from the system DLLs of
  `/run/media/dani/BigDisk/winnt-x86/img/nt4-vanilla.qcow2` (NT 4.0 Workstation **SP1**;
  extracted to `/run/media/dani/BigDisk/nt4work/x86-dlls`). SP1's export names equal the
  SP0 lists for every DLL; I compared them against `imports/arm`. The only differences
  are architecture ones: NTDLL's x87 emulator and C helpers, and PINT's cuts in advapi32
  `Crypt*`, oleaut32 and msvcrt. kernel32 is 675 = 675.
  - __stdcall names carry their argument bytes (`CreateFileW@28`). Those come from the
    `windows_i686_msvc-0.53.1` crate's `windows.0.53.0.lib` (`--decorations`), plus
    `EXTRA_DECORATIONS` in gen-defs.py for undocumented NTDLL calls; only
    `RtlDosPathNameToNtPathName_U` so far.
  - `dbghelp.def` is the same empty placeholder as ARM's.
- **`tools/implib.py`** (new): writes x86 import libraries (short import objects, name
  type `undecorate`/`noprefix`). rust-lld's `/def` can't express "link `_F@8`, import
  `F`"; I tried every spelling. `mkimplibs.sh x86` uses it. Its output was checked
  against `llvm-dlltool -k` and links with rust-lld.
- **nt4compat**:
  - `helpers_x86.rs`: names NTDLL for `_alldiv`/`_aulldiv`/`_allrem`/`_aullrem`/
    `_allmul`/shifts/`_chkstk`, defines `_fltused`, and `__gnu_h2f_ieee`/`__gnu_f2h_ieee`
    (extern "C"). nt4rt includes it by `#[path]` too.
  - `x86.rs`: `RtlCaptureContext` (XP on x86; std's backtraces call it) as a naked
    function filling CONTEXT_CONTROL|INTEGER.
  - **A real bug fix in `imp.rs` and `crt.rs`.** On x86, LLVM prefixes `_` to every C
    symbol, `export_name` included. The old x86 `cfg_attr`s produced
    `___imp__X@N`/`___tls_index`, so nothing resolved. They now use `_imp__X@N` and
    `_tls_index`/`_tls_used`.
- **std patch**: compiler_builtins' `partial_availability` (exp, hypot...) is enabled for
  `nt4` even on x86 without SSE2. Upstream leaves it out there because of
  rust-lang/rust#114479: an x87 return quiets a signalling NaN.
- **cargo-nt4**: `i686-*` gives ARCH=x86, and PINT's SDK is used for ARM only.
  **peinfo.py**: strips `@N` from .def names when checking.
- **`tests/x86/run-exe.sh`** (new): runs programs on an x86 NT 4.0 disk in QEMU.
  - It boots an overlay of the image, logs on as Administrator (blank password) by
    `sendkey`/`type` through `tests/pint/mon.py`, runs the programs from a CD-ROM
    (genisoimage) with their output in `C:\TMP\RUST`, then reads it back off the overlay
    with `qemu-img convert` + `7z`.
  - NT 4.0's disk is NTFS 1.2, which Linux shouldn't write: hence the CD and the typing.

## Evidence it works

Built with `NT4_TARGET=i686-nt4-windows-msvc tools/cargo-nt4 build --release -p ...`:

- `hello` and `stdtest` pass every check on NT 4.0 SP1 in QEMU (`-cpu pentium`). That
  covers threads/locks, TLS destructors, RtlGenRandom, files, processes, args, sleep,
  TCP, float maths. Run `stdtest` with `ARGS_STDTEST='one "two words"'`.
- vintrdp (`/run/media/dani/BigDisk/vintrdp-rust`, `--no-default-features --features
  sound`) and its headless example build for x86, pass `peinfo --defs imports/x86`, and
  ran:
  - on NT 4.0 in QEMU (connected to Server 2003, drew the desktop);
  - on Server 2003 itself (headless and GUI connected to Windows 2000).
- ARM is unaffected: `stdtest`, `hello` and vintrdp still build for PINT and pass the
  ARM import check. They weren't rerun on PINT after these changes.

Command:

    cd /run/media/dani/BigDisk/nt4work/x86run && TMPDIR=$PWD WAIT=150 \
      QEMU=/usr/bin/qemu-system-i386 ARGS_STDTEST='one "two words"' \
      ~/repos/ruststdwinnt4/tests/x86/run-exe.sh \
      /run/media/dani/BigDisk/winnt-x86/img/nt4-vanilla.qcow2 \
      ~/repos/ruststdwinnt4/target/i686-nt4-windows-msvc/release/stdtest.exe

## Your job

1. **Review the `x86` branch and merge it into `main`** if it holds up. Things to check
   while reviewing:
   - `tools/implib.py`: archive layout and the sorted second linker member. Also, an
     import library normally carries `__IMPORT_DESCRIPTOR_*`/`__NULL_IMPORT_DESCRIPTOR`
     objects; lld doesn't need them, MS link would.
   - `x86.rs` `RtlCaptureContext` offsets against the i386 CONTEXT layout. It has only
     been linked, never exercised: nothing in the tests takes a backtrace.
   - That nothing ARM changed meaning, especially the `crt.rs` TLS symbol names.
2. **Run PINT's tests again** (`tests/pint/run-exe.sh`) with `hello` and `stdtest` on the
   merged tree, to prove ARM is unchanged at run time.
3. **Harden `tests/x86/run-exe.sh`**:
   - Its default `QEMU=qemu-system-i386` finds `/usr/local/bin`'s, which has **no user
     networking** (`-netdev user` fails). Default to one that has it, or check for it.
   - It uses fixed sleeps (90 s boot, 25 s logon...). Wait for something real instead:
     poll the screen via `screendump`, or have RUNTESTS.CMD signal over the network.
   - It depends on the image's Administrator having a blank password: document it or
     make it a setting.
4. **C for x86.** PINT's SDK compiles ARM only, so the x86 target has no C compiler: no
   GnuTLS on x86 NT 4.0, which vintrdp's `tls` feature needs.
   - Candidates:
     - Visual C++ 4.2/5/6 for x86 under Wine: the `tenox7/nt40sdk` Docker image, or the
       MSVC40.iso in `/run/media/dani/BigDisk/winnt-x86/iso`;
     - clang-cl targeting `i686-pc-windows-msvc` with NT 4.0 headers.
   - Wire it into cargo-nt4 the way `tools/nt4-cl` does for ARM, then reuse vintrdp's
     `scripts/dist/build-deps-nt4.sh` (vintrdp's `nt4` branch).
   - Don't link anything the import check rejects.
5. **Small things**:
   - neither ARM nor x86 has a `winspool.drv` list (vintrdp now finds the spooler at run
     time, so it's optional);
   - consider whether `gen-defs.py`'s `--cd` wording should say "an installed
     system32" for x86;
   - add x86 to the README's examples where PINT is the only one shown.

## Don't

- Don't push anything or touch `main` without the user saying so.
- Don't write to `nt4-vanilla.qcow2`: work on overlays.
- m900's root disk is nearly full (18 GB): build output and VM work go under
  `/run/media/dani/BigDisk`.
- Don't change vintrdp; that's a separate repo. If ruststdwinnt4 needs something from
  vintrdp, report it.
