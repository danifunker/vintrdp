# Distribution builds

Every file vintrdp publishes is made by a script in this directory. The GitHub
workflows install toolchains and call these scripts with explicit options, so a
local run with the same options produces the same files. Output goes to `dist/`
unless you pass `--out`.

| Script | Makes | Runs on |
|---|---|---|
| `build-deps.sh` | static GnuTLS and the GMP, Nettle and libtasn1 under it | any |
| `llvm-mingw.sh` | downloads the Windows cross toolchain | Linux |
| `build-windows.sh` | `vintrdp.exe` for x86, x64 or ARM64 | Linux |
| `build-nt4.sh` | `vintrdp.exe` for Windows NT 4.0 on ARM (PINT) or x86 | Linux, macOS |
| `package-windows.sh` | Inno Setup installer and portable zip | Windows (Git Bash) |
| `package-macos.sh` | universal `vintrdp.app` as dmg and zip; signs and notarizes when credentials are set | macOS |
| `package-linux.sh` | deb, rpm, Arch pkg, tarball | Debian 13 |
| `appimage.sh` | self-contained AppImage for x64 and arm64 | Arch container CI uses |
| `version.sh` | the version string: `X.Y.<run>`, `X.Y.Z`, or `dev.<commit>` | any |
| `release-notes.sh` | the release body, and the list of expected files | any |
| `publish-github.sh` | replaces a GitHub release with the files in a directory | any |

The packaging scripts share these options:

```
--version V      default dev.<commit>
--target TRIPLE  default the host
--out DIR        default dist/
--skip-build     package what is already in target/
```

Release builds link GnuTLS and its dependencies statically (`build-deps.sh`,
then `VINTRDP_DEPS` points the build at the result). Windows and macOS packages
therefore carry no third-party libraries, and the scripts fail if a binary
needs anything the OS doesn't ship. deb and rpm depend only on GTK 4, ALSA and libc.
The x64 and arm64 AppImages use Arch's libraries instead, which quick-sharun
bundles. sharun has no riscv64 build, so riscv64 has no AppImage.

## Windows NT 4.0

`build-nt4.sh --arch arm|x86 [--version V] [--out DIR]` builds the Windows front-end for
NT 4.0 with [ruststdwinnt4](https://github.com/danifunker/ruststdwinnt4), a Rust 1.90 std
for NT 4.0 and its `cargo-nt4`. It finds ruststdwinnt4 through `RUSTSTDWINNT4=path`, else
in `../ruststdwinnt4` next to this repository, and leaves `vintrdp-nt4-<arch>.exe` in
`--out`. `arm` is PINT (NT 4.0 on the Raspberry Pi); `x86` is NT 4.0 on x86, and the build
to try on Alpha under FX!32.

The build is pure Rust, without the `tls` feature (GnuTLS), and with sound through
`waveOut`. Nothing proprietary is needed: rustup installs the Rust that ruststdwinnt4 pins
(with `rust-src`), and `llvm-windres` compiles the icon and manifest. The script fails
when the `.exe` imports a function NT 4.0 SP0 doesn't export, or is marked for a later
Windows (ruststdwinnt4's `tools/peinfo.py`). Functions of later Windows the front-end
uses are looked up at run time (`crates/vintrdp/src/win32.rs`).

`cargo-nt4` points the `windows` crates at NT 4.0 copies, which cargo records in
`Cargo.lock`; the script puts the lock file back afterwards. On a Mac where Homebrew's
`rustc` comes before rustup's on `PATH`, put the pinned toolchain's `bin` first.

`build.yml` checks out ruststdwinnt4 at the commit in `RUSTSTDWINNT4_REF` and builds both.

## Workflows

- `release.yml` runs on every push to `main` that changes more than docs: it
  builds everything and publishes the latest release as `X.Y.<run number>`, with
  `X.Y` from `Cargo.toml`. Pushing a `vX.Y.Z` tag publishes that version instead.
- `build.yml` builds everything for one commit and checks that every file the
  downloads table links to exists. To test any branch, run it by hand (Actions >
  Build > Run workflow, or `gh workflow run build.yml --ref BRANCH`); the files
  are attached to the run and nothing is published.
- `ci.yml` runs the tests on pull requests.

All actions are on majors that run on Node 24.

## macOS signing and notarization

`build.yml` signs and notarizes the app when these repository secrets exist, and
falls back to ad-hoc signing with a warning when they do not:

| Secret | What it is |
|---|---|
| `MACOS_CERTIFICATE` | base64 of a .p12 holding the **Developer ID Application** certificate and its private key |
| `MACOS_CERTIFICATE_PWD` | the .p12 password |
| `MACOS_TEAM_ID` | Apple team ID |
| `MACOS_NOTARIZE_API_KEY`, `MACOS_NOTARIZE_API_KEY_ID`, `MACOS_NOTARIZE_API_ISSUER` | App Store Connect API key (base64 of the .p8), its key ID and the issuer ID |
| `MACOS_NOTARIZE_APPLE_ID`, `MACOS_NOTARIZE_PASSWORD` | alternative to the API key: Apple ID and app-specific password |

Locally, `package-macos.sh` reads `MACOS_SIGN_IDENTITY` and either
`NOTARY_KEY_ID`/`NOTARY_ISSUER_ID`/`NOTARY_KEY_PATH` or
`NOTARY_APPLE_ID`/`NOTARY_PASSWORD`/`NOTARY_TEAM_ID`.

## Versions

The version goes into the About window, `--version`, the macOS Info.plist, the
Windows installer and every file name. deb and rpm versions need a leading
digit, so a dev build is packaged as `0~dev.<commit>`, which sorts below any
real release.
