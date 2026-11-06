/* -*- c-basic-offset: 8 -*-
   rdesktop: A Remote Desktop Protocol client.
   Copyright (C) Matthew Chapman <matthewc.unsw.edu.au> 1999-2008

   This program is free software: you can redistribute it and/or modify
   it under the terms of the GNU General Public License as published by
   the Free Software Foundation, either version 3 of the License, or
   (at your option) any later version.

   This program is distributed in the hope that it will be useful,
   but WITHOUT ANY WARRANTY; without even the implied warranty of
   MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
   GNU General Public License for more details.

   You should have received a copy of the GNU General Public License
   along with this program.  If not, see <http://www.gnu.org/licenses/>.
*/

#include "rdesktop.h"

extern RDPDR_DEVICE g_rdpdr_device[];

static PRINTER *
get_printer_data(RD_NTHANDLE handle)
{
	int index;

	for (index = 0; index < RDPDR_MAX_DEVICES; index++)
	{
		if (handle == g_rdpdr_device[index].handle)
			return (PRINTER *) g_rdpdr_device[index].pdevice_data;
	}
	return NULL;
}

int
printer_enum_devices(uint32 * id, char *optarg)
{
	PRINTER *pprinter_data;

	char *pos = optarg;
	char *pos2;
	size_t count = 0;
	int already = 0;

	/* we need to know how many printers we've already set up
	   supplied from other -r flags than this one. */
	while (count < *id)
	{
		if (g_rdpdr_device[count].device_type == DEVICE_TYPE_PRINTER)
			already++;
		count++;
	}

	count = 0;

	if (*optarg == ':')
		optarg++;

	while ((pos = next_arg(optarg, ',')) && *id < RDPDR_MAX_DEVICES)
	{
		pprinter_data = (PRINTER *) xmalloc(sizeof(PRINTER));

		strcpy(g_rdpdr_device[*id].name, "PRN");
		strcat(g_rdpdr_device[*id].name, l_to_a(already + count + 1, 10));

		/* first printer is set as default printer */
		if ((already + count) == 0)
			pprinter_data->default_printer = True;
		else
			pprinter_data->default_printer = False;

		pos2 = next_arg(optarg, '=');
		if (*optarg == (char) 0x00)
			pprinter_data->printer = "mydeskjet";	/* set default */
		else
		{
			pprinter_data->printer = xmalloc(strlen(optarg) + 1);
			strcpy(pprinter_data->printer, optarg);
		}

		if (!pos2 || (*pos2 == (char) 0x00))
			pprinter_data->driver = "MS Publisher Imagesetter";	/* no printer driver supplied set default */
		else
		{
			pprinter_data->driver = xmalloc(strlen(pos2) + 1);
			strcpy(pprinter_data->driver, pos2);
		}

		logger(Core, Debug, "printer_enum_devices(), %s to %s driver %s",
		       g_rdpdr_device[*id].name, pprinter_data->printer, pprinter_data->driver);
		g_rdpdr_device[*id].device_type = DEVICE_TYPE_PRINTER;
		g_rdpdr_device[*id].pdevice_data = (void *) pprinter_data;
		count++;
		(*id)++;

		optarg = pos;
	}
	return count;
}

/* vintrdp: jobs go to the front-end's printing (lpr without a shell on Unix,
   the spooler on Windows) through ui_printer_*, not popen("lpr -P name"). */
static RD_NTSTATUS
printer_create(uint32 device_id, uint32 access, uint32 share_mode, uint32 disposition, uint32 flags,
	       char *filename, RD_NTHANDLE * handle)
{
	UNUSED(access);
	UNUSED(share_mode);
	UNUSED(disposition);
	UNUSED(flags);
	UNUSED(filename);
	PRINTER *pprinter_data;

	pprinter_data = (PRINTER *) g_rdpdr_device[device_id].pdevice_data;

	/* "mydeskjet" is rdesktop's name for the default printer */
	pprinter_data->job = ui_printer_open(strcmp(pprinter_data->printer, "mydeskjet") == 0 ?
					     NULL : pprinter_data->printer);
	if (pprinter_data->job <= 0)
		return RD_STATUS_ACCESS_DENIED;

	/* Kept clear of file descriptors and Windows handles of other devices */
	g_rdpdr_device[device_id].handle = 0x70000000 | (uint32) pprinter_data->job;
	*handle = g_rdpdr_device[device_id].handle;
	return RD_STATUS_SUCCESS;
}

static RD_NTSTATUS
printer_close(RD_NTHANDLE handle)
{
	int i = get_device_index(handle);
	if (i >= 0)
	{
		PRINTER *pprinter_data = g_rdpdr_device[i].pdevice_data;
		if (pprinter_data && pprinter_data->job > 0)
		{
			ui_printer_close(pprinter_data->job);
			pprinter_data->job = 0;
		}
		g_rdpdr_device[i].handle = 0;
	}
	return RD_STATUS_SUCCESS;
}

static RD_NTSTATUS
printer_write(RD_NTHANDLE handle, uint8 * data, uint32 length, uint64 offset, uint32 * result)
{
	UNUSED(offset);  /* Currently unused, MS-RDPEPC reserves for later use */
	PRINTER *pprinter_data;

	pprinter_data = get_printer_data(handle);
	if (!pprinter_data || pprinter_data->job <= 0 || !ui_printer_write(pprinter_data->job, data, length))
	{
		*result = 0;
		return RD_STATUS_INVALID_HANDLE;
	}
	*result = length;
	return RD_STATUS_SUCCESS;
}

DEVICE_FNS printer_fns = {
	printer_create,
	printer_close,
	NULL,			/* read */
	printer_write,
	NULL			/* device_control */
};
