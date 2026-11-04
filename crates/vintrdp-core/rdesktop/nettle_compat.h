/* Nettle 4 dropped the length argument from *_digest(). rdesktop's ssl.c
 * handles this, but cssp.c and ntlmssp.c still use the Nettle 3 signature.
 * Force-included by build.rs so ../rdesktop stays untouched. */
#include <nettle/version.h>
#include <nettle/sha2.h>
#include <nettle/md4.h>
#if NETTLE_VERSION_MAJOR >= 4
#undef sha256_digest
#define sha256_digest(ctx, len, out) nettle_sha256_digest(ctx, out)
#undef md4_digest
#define md4_digest(ctx, len, out) nettle_md4_digest(ctx, out)
#endif
