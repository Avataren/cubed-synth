/*
 * Golden-data generator for rust-wasm/tests/ahx_render_golden.rs.
 *
 * Build (offline, one-off; NOT part of cargo test, no new dependency):
 *   gcc -O0 -fwrapv -fcommon -w -I../../../.ai/ahx/references -o /tmp/hvl_golden \
 *       hvl_golden.c ../../../.ai/ahx/references/hvl_replay.c \
 *       ../../../.ai/ahx/references/hvl_tables.c -lm
 * Run:
 *   /tmp/hvl_golden <file> <freq> <defstereo> <frames> <chunk_frames> [channel_cap]
 *
 * `channel_cap` truncates the song to its first N channels before playback
 * (`ht_Channels = N`), which is what the fixed-4 engine does to >4-channel
 * songs. Omitted = play all channels.
 *
 * Output: one line per chunk = FNV-1a-64 of the interleaved little-endian
 * int16 stereo stream `hvl_DecodeFrame` produced for that chunk, plus header
 * lines with the waves-table hash and per-feature coverage counters.
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "hvl_replay.h"
#include "hvl_tables.h"

static uint64_t fnv(uint64_t h, const unsigned char *p, size_t n)
{
  size_t i;
  for (i = 0; i < n; i++) { h ^= p[i]; h *= 0x100000001b3ULL; }
  return h;
}

int main(int argc, char **argv)
{
  struct hvl_tune *ht;
  uint32 freq, defstereo, frames, chunk, f, i;
  uint32 cov_ring = 0, cov_noise = 0, cov_filter = 0, cov_square = 0, cov_hardcut = 0, cov_vib = 0, cov_slide = 0, cov_plist = 0;
  uint64_t h, wh;
  int16 *buf;
  uint32 spf;

  if (argc < 6) { fprintf(stderr, "usage\n"); return 2; }
  freq = atoi(argv[2]); defstereo = atoi(argv[3]); frames = atoi(argv[4]); chunk = atoi(argv[5]);

  hvl_InitReplayer();
  wh = fnv(0xcbf29ce484222325ULL, (const unsigned char *)waves, WAVES_SIZE);
  printf("waves %016llx\n", (unsigned long long)wh);
  {
    uint64_t ph = fnv(0xcbf29ce484222325ULL, (const unsigned char *)panning_left, sizeof(panning_left));
    ph = fnv(ph, (const unsigned char *)panning_right, sizeof(panning_right));
    printf("panning %016llx\n", (unsigned long long)ph);
  }

  /* echo the case so the Rust side can prove a golden matches its manifest row */
  printf("case %s %u %u %u %u %d\n", strrchr(argv[1], '/') ? strrchr(argv[1], '/') + 1 : argv[1],
         freq, defstereo, frames, chunk, argc > 6 ? atoi(argv[6]) : 0);

  ht = hvl_LoadTune(argv[1], freq, defstereo);
  if (!ht) return 1;
  if (argc > 6 && (uint32)atoi(argv[6]) < ht->ht_Channels) ht->ht_Channels = atoi(argv[6]);
  hvl_InitSubsong(ht, 0);
  printf("channels %u speedmult %u instruments %u positions %u tracklen %u\n",
         ht->ht_Channels, ht->ht_SpeedMultiplier, ht->ht_InstrumentNr, ht->ht_PositionNr, ht->ht_TrackLength);

  spf = freq / 50;                       /* samples per DecodeFrame call (mult * (freq/50/mult)) */
  spf = (freq / 50 / ht->ht_SpeedMultiplier) * ht->ht_SpeedMultiplier;
  buf = malloc(spf * 4);
  h = 0xcbf29ce484222325ULL;
  for (f = 0; f < frames; f++)
  {
    hvl_DecodeFrame(ht, (int8 *)buf, (int8 *)(buf + 1), 4);
    h = fnv(h, (const unsigned char *)buf, spf * 4);
    for (i = 0; i < ht->ht_Channels; i++)
    {
      struct hvl_voice *v = &ht->ht_Voices[i];
      if (v->vc_RingMixSource) cov_ring++;
      if (v->vc_Waveform == 3) cov_noise++;
      if (v->vc_FilterOn) cov_filter++;
      if (v->vc_Waveform == 2 && v->vc_SquareOn) cov_square++;
      if (v->vc_HardCut) cov_hardcut++;
      if (v->vc_VibratoDepth) cov_vib++;
      if (v->vc_PeriodSlideOn) cov_slide++;
      if (v->vc_PerfList) cov_plist++;
    }
    if ((f + 1) % chunk == 0)
    {
      printf("%u %016llx\n", f + 1, (unsigned long long)h);
      h = 0xcbf29ce484222325ULL;
    }
  }
  printf("coverage ring %u noise %u filter %u square %u hardcut %u vibrato %u slide %u plist %u\n",
         cov_ring, cov_noise, cov_filter, cov_square, cov_hardcut, cov_vib, cov_slide, cov_plist);
  printf("songend %u posnr %d notenr %d\n", ht->ht_SongEndReached, ht->ht_PosNr, ht->ht_NoteNr);
  return 0;
}
