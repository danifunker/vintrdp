/* rdpsnd output driver: hands PCM to the player in src/audio.rs. That keeps a
   buffer of its own and never blocks, so there are no descriptors to wait on;
   this just feeds it whenever the main loop comes round. */
#include "rdesktop.h"
#include "rdpsnd.h"
#include "rdpsnd_dsp.h"

/* src/audio.rs */
RD_BOOL vintrdp_audio_open(void);
void vintrdp_audio_close(void);
void vintrdp_audio_set_format(uint32 rate, uint16 channels, uint16 bits);
void vintrdp_audio_write(const uint8 * data, uint32 size);
uint32 vintrdp_audio_queued_us(void);

/* Whatever sits in the player is heard that much later, so it only gets the
   next packet when it's nearly out. A packet is already a third of a second. */
#define BUFFER_AHEAD_US 50000
#define POLL_US 10000

static RD_WAVEFORMATEX format;
static struct audio_driver driver;

static void
add_fds(int *n, fd_set * rfds, fd_set * wfds, struct timeval *tv)
{
	UNUSED(n);
	UNUSED(rfds);
	UNUSED(wfds);
	if (!rdpsnd_queue_empty() && (tv->tv_sec > 0 || tv->tv_usec > POLL_US))
	{
		tv->tv_sec = 0;
		tv->tv_usec = POLL_US;
	}
}

static void
check_fds(fd_set * rfds, fd_set * wfds)
{
	UNUSED(rfds);
	UNUSED(wfds);
	while (!rdpsnd_queue_empty() && vintrdp_audio_queued_us() < BUFFER_AHEAD_US)
	{
		struct audio_packet *packet = rdpsnd_queue_current_packet();
		STREAM s = packet->s;
		unsigned int size = s_remaining(s);
		STREAM out = rdpsnd_dsp_process(s->p, size, &driver, &format);
		vintrdp_audio_write(out->data, s_length(out));
		s_free(out);
		rdpsnd_queue_next(vintrdp_audio_queued_us());
	}
}

static RD_BOOL
format_supported(RD_WAVEFORMATEX * pwfx)
{
	return pwfx->wFormatTag == WAVE_FORMAT_PCM && (pwfx->nChannels == 1 || pwfx->nChannels == 2)
		&& (pwfx->wBitsPerSample == 8 || pwfx->wBitsPerSample == 16);
}

static RD_BOOL
set_format(RD_WAVEFORMATEX * pwfx)
{
	format = *pwfx;
	vintrdp_audio_set_format(pwfx->nSamplesPerSec, pwfx->nChannels, pwfx->wBitsPerSample);
	return True;
}

struct audio_driver *
vintrdp_register(char *options)
{
	UNUSED(options);
	memset(&driver, 0, sizeof(driver));
	driver.name = "vintrdp";
	driver.description = "vintrdp audio output";
	driver.add_fds = add_fds;
	driver.check_fds = check_fds;
	driver.wave_out_open = vintrdp_audio_open;
	driver.wave_out_close = vintrdp_audio_close;
	driver.wave_out_format_supported = format_supported;
	driver.wave_out_set_format = set_format;
	driver.wave_out_volume = rdpsnd_dsp_softvol_set;
	return &driver;
}
