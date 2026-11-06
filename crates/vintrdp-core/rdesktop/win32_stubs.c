/* What the core calls in the files build.rs leaves out on Windows. */
#include "rdesktop.h"

/* rdpear.c: the Kerberos helper behind restricted admin mode (a Unix
   subprocess). No credentials means cssp.c reports the failure. */
STREAM
rdpear_call_helper(const char *package_name, STREAM request)
{
	(void) package_name;
	(void) request;
	return NULL;
}
