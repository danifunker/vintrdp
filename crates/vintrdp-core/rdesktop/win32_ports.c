/* vintrdp: serial and parallel port redirection on Windows.

   The server's requests for a port are Windows' own serial and parallel
   IOCTLs, so most of them go straight to DeviceIoControl() on the local port.
   Three kinds are handled here instead, because rdesktop's main loop can't
   wait on a Windows handle:

   - Reads and writes complete at once (rdpdr.c treats these ports as
     blocking on Windows). The port's own timeouts are set so a read returns
     whatever has arrived, possibly nothing, instead of waiting.
   - The server's timeouts (SET/GET_TIMEOUTS) are kept for it, not applied.
   - WAIT_ON_MASK stays pending, and serial_get_event() completes it when the
     main loop polls (see process_fds() in shim.rs).

   The serial.c and parallel.c this replaces are POSIX (termios, /dev). */

#include "rdesktop.h"

#define IOCTL_SERIAL_SET_TIMEOUTS	0x001B001C
#define IOCTL_SERIAL_GET_TIMEOUTS	0x001B0020
#define IOCTL_SERIAL_GET_WAIT_MASK	0x001B0040
#define IOCTL_SERIAL_SET_WAIT_MASK	0x001B0044
#define IOCTL_SERIAL_WAIT_ON_MASK	0x001B0048

extern RDPDR_DEVICE g_rdpdr_device[];

typedef struct
{
	HANDLE port;
	uint32 wait_mask;
	uint32 timeouts[5];	/* as the server set them */
	DWORD modem;		/* modem status at the last event check */
	RD_BOOL sent;		/* written since the last EV_TXEMPTY */
} WIN_PORT;

static WIN_PORT *
port_of(RD_NTHANDLE handle)
{
	int i = get_device_index(handle);
	return i >= 0 ? (WIN_PORT *) g_rdpdr_device[i].pdevice_data : NULL;
}

/* ':COM1=COM3,COM2=\\.\COM10' (escapes already handled by next_arg) */
static int
enum_ports(uint32 * id, char *optarg, uint32 type)
{
	char *pos, *pos2;
	int count = 0;

	optarg++;		/* skip the colon */
	while ((pos = next_arg(optarg, ',')) && *id < RDPDR_MAX_DEVICES)
	{
		pos2 = next_arg(optarg, '=');
		if (!pos2 || !*pos2 || strlen(optarg) >= sizeof(g_rdpdr_device[*id].name))
		{
			optarg = pos;
			continue;
		}
		strcpy(g_rdpdr_device[*id].name, optarg);
		toupper_str(g_rdpdr_device[*id].name);
		g_rdpdr_device[*id].local_path = xstrdup(pos2);
		g_rdpdr_device[*id].device_type = type;
		g_rdpdr_device[*id].pdevice_data = xmalloc(sizeof(WIN_PORT));
		memset(g_rdpdr_device[*id].pdevice_data, 0, sizeof(WIN_PORT));
		g_rdpdr_device[*id].handle = 0;
		logger(Core, Debug, "enum_ports(), %s to %s", g_rdpdr_device[*id].name, pos2);
		count++;
		(*id)++;
		optarg = pos;
	}
	return count;
}

int
serial_enum_devices(uint32 * id, char *optarg)
{
	return enum_ports(id, optarg, DEVICE_TYPE_SERIAL);
}

int
parallel_enum_devices(uint32 * id, char *optarg)
{
	return enum_ports(id, optarg, DEVICE_TYPE_PARALLEL);
}

static RD_NTSTATUS
port_create(uint32 device_id, uint32 access, uint32 share_mode, uint32 disposition,
	    uint32 flags, char *filename, RD_NTHANDLE * handle)
{
	char path[MAX_PATH];
	const char *local = g_rdpdr_device[device_id].local_path;
	WIN_PORT *p = (WIN_PORT *) g_rdpdr_device[device_id].pdevice_data;
	COMMTIMEOUTS now = { MAXDWORD, 0, 0, 0, 0 };	/* reads return at once */
	UNUSED(access);
	UNUSED(share_mode);
	UNUSED(disposition);
	UNUSED(flags);
	UNUSED(filename);

	/* "COM3" -> "\\.\COM3", which also reaches COM10 and up */
	snprintf(path, sizeof(path), "%s%s", strncmp(local, "\\\\", 2) ? "\\\\.\\" : "", local);
	p->port = CreateFileA(path, GENERIC_READ | GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
	if (p->port == INVALID_HANDLE_VALUE)
	{
		logger(Core, Error, "port_create(), can't open %s, error %lu", path, GetLastError());
		p->port = NULL;
		return RD_STATUS_ACCESS_DENIED;
	}
	if (g_rdpdr_device[device_id].device_type == DEVICE_TYPE_SERIAL)
	{
		SetCommTimeouts(p->port, &now);
		GetCommModemStatus(p->port, &p->modem);
	}
	p->wait_mask = 0;
	p->sent = False;

	/* Kernel handles fit in 32 bits, even on 64-bit Windows. */
	g_rdpdr_device[device_id].handle = (RD_NTHANDLE) (uintptr_t) p->port;
	*handle = g_rdpdr_device[device_id].handle;
	return RD_STATUS_SUCCESS;
}

static RD_NTSTATUS
port_close(RD_NTHANDLE handle)
{
	int i = get_device_index(handle);
	if (i >= 0)
	{
		WIN_PORT *p = (WIN_PORT *) g_rdpdr_device[i].pdevice_data;
		if (p && p->port)
			CloseHandle(p->port);
		if (p)
			p->port = NULL;
		g_rdpdr_device[i].handle = 0;
	}
	return RD_STATUS_SUCCESS;
}

static RD_NTSTATUS
port_read(RD_NTHANDLE handle, uint8 * data, uint32 length, uint64 offset, uint32 * result)
{
	WIN_PORT *p = port_of(handle);
	DWORD n = 0;
	UNUSED(offset);
	if (!p || !ReadFile(p->port, data, length, &n, NULL))
	{
		*result = 0;
		return RD_STATUS_INVALID_HANDLE;
	}
	*result = n;
	return RD_STATUS_SUCCESS;
}

static RD_NTSTATUS
port_write(RD_NTHANDLE handle, uint8 * data, uint32 length, uint64 offset, uint32 * result)
{
	WIN_PORT *p = port_of(handle);
	DWORD n = 0;
	UNUSED(offset);
	if (!p || !WriteFile(p->port, data, length, &n, NULL))
	{
		*result = 0;
		return RD_STATUS_INVALID_HANDLE;
	}
	p->sent = True;
	*result = n;
	return RD_STATUS_SUCCESS;
}

/* The server's wait mask events that have happened, if any. */
static uint32
port_events(WIN_PORT * p)
{
	DWORD errors = 0, modem = 0;
	COMSTAT stat;
	uint32 events = 0;

	if (!p || !p->port || !p->wait_mask)
		return 0;
	memset(&stat, 0, sizeof(stat));
	if (ClearCommError(p->port, &errors, &stat))
	{
		if (stat.cbInQue > 0)
			events |= EV_RXCHAR;
		if (p->sent && stat.cbOutQue == 0)
			events |= EV_TXEMPTY;
		if (errors)
			events |= EV_ERR;
	}
	if (GetCommModemStatus(p->port, &modem))
	{
		DWORD changed = modem ^ p->modem;
		if (changed & MS_CTS_ON)
			events |= EV_CTS;
		if (changed & MS_DSR_ON)
			events |= EV_DSR;
		if (changed & MS_RLSD_ON)
			events |= EV_RLSD;
		if (changed & modem & MS_RING_ON)
			events |= EV_RING;
	}
	events &= p->wait_mask;
	if (events)
	{
		p->modem = modem;
		if (events & EV_TXEMPTY)
			p->sent = False;
	}
	return events;
}

RD_BOOL
serial_get_event(RD_NTHANDLE handle, uint32 * result)
{
	uint32 events = port_events(port_of(handle));
	if (!events)
		return False;
	*result = events;
	return True;
}

RD_BOOL
serial_get_timeout(RD_NTHANDLE handle, uint32 length, uint32 * timeout, uint32 * itv_timeout)
{
	UNUSED(handle);
	UNUSED(length);
	*timeout = 0;
	*itv_timeout = 0;
	return False;
}

static RD_NTSTATUS
port_device_control(RD_NTHANDLE handle, uint32 request, STREAM in, STREAM out)
{
	WIN_PORT *p = port_of(handle);
	uint8 *input = NULL, *output = NULL;
	size_t in_len, out_len;
	DWORD returned = 0;
	int i;

	if (!p || !p->port)
		return RD_STATUS_INVALID_HANDLE;

	switch (request)
	{
		case IOCTL_SERIAL_SET_TIMEOUTS:
			for (i = 0; i < 5 && s_check_rem(in, 4); i++)
				in_uint32_le(in, p->timeouts[i]);
			return RD_STATUS_SUCCESS;

		case IOCTL_SERIAL_GET_TIMEOUTS:
			for (i = 0; i < 5; i++)
				out_uint32_le(out, p->timeouts[i]);
			return RD_STATUS_SUCCESS;

		case IOCTL_SERIAL_SET_WAIT_MASK:
			if (s_check_rem(in, 4))
				in_uint32_le(in, p->wait_mask);
			GetCommModemStatus(p->port, &p->modem);
			return RD_STATUS_SUCCESS;

		case IOCTL_SERIAL_GET_WAIT_MASK:
			out_uint32_le(out, p->wait_mask);
			return RD_STATUS_SUCCESS;

		case IOCTL_SERIAL_WAIT_ON_MASK:
		{
			uint32 events = port_events(p);
			if (!events)
				return RD_STATUS_PENDING;
			out_uint32_le(out, events);
			return RD_STATUS_SUCCESS;
		}
	}

	/* Everything else is the local port's business. */
	in_len = s_remaining(in);
	if (in_len)
		in_uint8p(in, input, in_len);
	out_len = s_left(out) > 0x14 ? s_left(out) - 0x14 : 0;
	if (out_len)
		output = out->p;
	if (!DeviceIoControl(p->port, request, input, (DWORD) in_len, output, (DWORD) out_len, &returned, NULL))
	{
		logger(Core, Debug, "port_device_control(), request 0x%x failed, error %lu", request,
		       GetLastError());
		return RD_STATUS_INVALID_PARAMETER;
	}
	if (returned)
		out->p += returned;
	return RD_STATUS_SUCCESS;
}

DEVICE_FNS serial_fns = {
	port_create,
	port_close,
	port_read,
	port_write,
	port_device_control
};

DEVICE_FNS parallel_fns = {
	port_create,
	port_close,
	port_read,
	port_write,
	port_device_control
};
