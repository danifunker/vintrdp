/* The small part of POSIX fnmatch() that disk.c needs: Windows directory
   wildcards, * and ?, matched without regard to case. */
#ifndef VINTRDP_FNMATCH_H
#define VINTRDP_FNMATCH_H
#include <ctype.h>

#define FNM_NOMATCH 1

static inline int
fnmatch(const char *pattern, const char *name, int flags)
{
	(void) flags;
	for (; *pattern; pattern++, name++)
	{
		if (*pattern == '*')
		{
			while (*pattern == '*')
				pattern++;
			if (!*pattern)
				return 0;
			for (; *name; name++)
				if (fnmatch(pattern, name, 0) == 0)
					return 0;
			return FNM_NOMATCH;
		}
		if (!*name)
			return FNM_NOMATCH;
		if (*pattern != '?' && tolower((unsigned char) *pattern) != tolower((unsigned char) *name))
			return FNM_NOMATCH;
	}
	return *name ? FNM_NOMATCH : 0;
}
#endif
