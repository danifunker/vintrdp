//! TLS and CredSSP (NLA) for Server 2003 SP1 and later, behind the `tls`
//! feature. Those servers want TLS 1.0 with RC4, 3DES or AES-CBC, which
//! rustls doesn't speak, so TLS sits behind [`TlsStream`]: GnuTLS implements
//! it today (`gnutls.rs`, linked, not compiled); a TLS 1.0 in Rust can follow.
//! CredSSP, NTLM and SPNEGO are in Rust. Port of rdesktop's ssl.c, tcp.c's
//! TLS parts, cssp.c, ntlmssp.c, spnego.c and utils.c's certificate store.

use std::io;
use std::net::TcpStream;
use std::sync::Arc;

use crate::error::Result;
use crate::session::Shared;

/// An established TLS connection. `read` and `write_all` may be called from
/// two threads at once (one reading, one writing), as the session does.
pub trait TlsStream: Send + Sync {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize>;
    fn write_all(&self, buf: &[u8]) -> io::Result<()>;
    /// The server certificate's subjectPublicKey (the BIT STRING's contents),
    /// which CredSSP binds to.
    fn server_public_key(&self) -> Result<Vec<u8>>;
    /// Send close_notify; reads in progress end.
    fn shutdown(&self);
}

/// Start TLS on `tcp` to `server` (the name the user typed). Checks the
/// server's certificate against the user's store (`~/.local/share/rdesktop/certs`),
/// asking through `shared.frontend.ask` about one it hasn't seen, as
/// rdesktop does. `shared.config.tls_version` limits the versions offered.
pub fn connect(tcp: TcpStream, server: &str, shared: &Shared) -> Result<Arc<dyn TlsStream>> {
    let _ = (tcp, server, shared);
    todo!("tls::connect")
}

/// Authenticate over CredSSP with NTLM (MS-CSSP), as the client:
/// `hostname` is the client's. Fails with a protocol error the user can
/// read when the server refuses the credentials.
pub fn credssp(tls: &dyn TlsStream, username: &str, domain: &str, password: &str, hostname: &str) -> Result<()> {
    let _ = (tls, username, domain, password, hostname);
    todo!("tls::credssp")
}
