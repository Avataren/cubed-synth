// Black-box register-trace oracle for A2M playback (plan-opl.md, O7). It runs
// AdPlug's player through its public interface only: a Copl subclass logs
// every write the player makes, grouped by update() call (one tick), and can
// forward them to AdPlug's Nuked OPL3 wrapper to render a WAV.
//
//   trace-oracle trace  <file.a2m> <ticks>              > trace.txt
//   trace-oracle render <file.a2m> <seconds> <out.wav> [rate]
//   trace-oracle replay <writes.txt> <seconds> <out.wav> [rate]
//
// replay renders `a2m_tool writes` output (our engine's register writes)
// through the same Nuked OPL3 with the same loop, so an audio difference can
// be split between the player and the chip (plan-opl.md O7 record).
//
// With A2M_PLAYER=v2 in the environment, the file is loaded straight into
// Ca2mv2Player (AdPlug's port of AT2's own player) instead of going through
// CAdPlug::factory, which hands versions 1-8 to the older Ca2mLoader (a
// conversion onto AdPlug's generic CmodPlayer).
//
// Trace format, one tick per "T" line followed by its writes:
//   T <tick> <refresh Hz> <order> <pattern> <row> <speed> <ended 0/1>
//   W <chip> <reg hex> <val hex>
// Writes made by rewind() (before tick 0) come under "T -1".
#include <adplug/adplug.h>
#include <adplug/nemuopl.h>
#include <adplug/a2m-v2.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <vector>

class CTraceOpl : public Copl {
 public:
  CTraceOpl(Copl *fwd, FILE *log) : fwd(fwd), log(log) { currType = TYPE_OPL3; }
  void write(int reg, int val) override {
    if (log) fprintf(log, "W %d %03x %02x\n", currChip, reg, val);
    if (fwd) { fwd->setchip(currChip); fwd->write(reg, val); }
  }
  void setchip(int n) override { Copl::setchip(n); }
  void init() override { if (log) fprintf(log, "W init\n"); if (fwd) fwd->init(); }
  void update(short *buf, int samples) override { if (fwd) fwd->update(buf, samples); }
  Copl *fwd;
  FILE *log;
};

static CPlayer *open(const char *file, Copl *opl) {
  const char *which = getenv("A2M_PLAYER");
  if (!which || strcmp(which, "v2")) return CAdPlug::factory(file, opl);
  CPlayer *p = Ca2mv2Player::factory(opl);
  if (!p->load(file)) { delete p; return nullptr; }
  return p;
}

static void header(CPlayer *p, long tick, bool ended, FILE *log) {
  fprintf(log, "T %ld %.6f %u %u %u %u %d\n", tick, p->getrefresh(), p->getorder(),
          p->getpattern(), p->getrow(), p->getspeed(), ended ? 1 : 0);
}

static void put16(FILE *f, unsigned v) { fputc(v & 255, f); fputc((v >> 8) & 255, f); }
static void put32(FILE *f, unsigned long v) { put16(f, v & 0xffff); put16(f, (v >> 16) & 0xffff); }

int main(int argc, char **argv) {
  if (argc < 4) { fprintf(stderr, "usage: see source\n"); return 2; }
  bool render = !strcmp(argv[1], "render");
  if (!render && strcmp(argv[1], "trace") && strcmp(argv[1], "replay")) return 2;
  const char *file = argv[2];
  if (!render && strcmp(argv[1], "replay")) {
    long ticks = atol(argv[3]);
    CTraceOpl opl(nullptr, stdout);
    fprintf(stdout, "T -1 0 0 0 0 0 0\n");
    CPlayer *p = open(file, &opl);
    if (!p) { fprintf(stderr, "FAIL %s\n", file); return 1; }
    bool playing = true;
    for (long t = 0; t < ticks; t++) {
      header(p, t, !playing, stdout);
      playing = p->update() && playing;
    }
    delete p;
    return 0;
  }
  if (!strcmp(argv[1], "replay")) {
    // replay <writes.txt> <seconds> <out.wav> [rate]: a2m_tool's writes
    // through Nuked with the render loop below (samples, then the tick).
    FILE *in = fopen(argv[2], "r");
    double seconds = atof(argv[3]);
    int rate = argc > 5 ? atoi(argv[5]) : 49716;
    CNemuopl nuked(rate);
    nuked.init();
    long total = (long)(seconds * rate), done = 0;
    std::vector<short> pcm, buf;
    double pending = 0;
    char line[128];
    bool first = true;
    while (fgets(line, sizeof line, in) && done < total) {
      if (line[0] == 'W') {
        unsigned r, v; sscanf(line + 2, "%x %x", &r, &v);
        nuked.setchip(0); nuked.write(r, v);
      } else if (line[0] == 'T') {
        double refresh = atof(line + 2);
        if (first && refresh < 0) { first = false; continue; }
        pending += rate / refresh;
        long n = (long)pending; pending -= n;
        if (n > total - done) n = total - done;
        buf.assign(n * 2, 0);
        if (n) nuked.update(buf.data(), n);
        pcm.insert(pcm.end(), buf.begin(), buf.end());
        done += n;
      }
    }
    FILE *f = fopen(argv[4], "wb");
    unsigned long bytes = pcm.size() * 2;
    fwrite("RIFF", 1, 4, f); put32(f, 36 + bytes); fwrite("WAVEfmt ", 1, 8, f);
    put32(f, 16); put16(f, 1); put16(f, 2); put32(f, rate); put32(f, rate * 4); put16(f, 4);
    put16(f, 16); fwrite("data", 1, 4, f); put32(f, bytes);
    fwrite(pcm.data(), 2, pcm.size(), f);
    fclose(f);
    return 0;
  }
  if (argc < 5) return 2;
  double seconds = atof(argv[3]);
  int rate = argc > 5 ? atoi(argv[5]) : 49716;
  CNemuopl nuked(rate);
  CTraceOpl opl(&nuked, nullptr);
  CPlayer *p = open(file, &opl);
  if (!p) { fprintf(stderr, "FAIL %s\n", file); return 1; }
  long total = (long)(seconds * rate);
  std::vector<short> pcm;
  pcm.reserve(total * 2);
  double pending = 0;
  long done = 0;
  std::vector<short> buf;
  while (done < total) {
    // AdPlay's loop: samples until the next tick = rate / refresh.
    pending += rate / p->getrefresh();
    long n = (long)pending;
    pending -= n;
    if (n > total - done) n = total - done;
    buf.assign(n * 2, 0);
    if (n) opl.update(buf.data(), n);
    pcm.insert(pcm.end(), buf.begin(), buf.end());
    done += n;
    p->update();
  }
  FILE *f = fopen(argv[4], "wb");
  unsigned long bytes = pcm.size() * 2;
  fwrite("RIFF", 1, 4, f); put32(f, 36 + bytes); fwrite("WAVEfmt ", 1, 8, f);
  put32(f, 16); put16(f, 1); put16(f, 2); put32(f, rate); put32(f, rate * 4); put16(f, 4);
  put16(f, 16); fwrite("data", 1, 4, f); put32(f, bytes);
  fwrite(pcm.data(), 2, pcm.size(), f);
  fclose(f);
  delete p;
  return 0;
}
