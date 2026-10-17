// Black-box oracle for decoded A2M fields (plan-opl.md, O6): what AdPlug's
// public player API reports. One line per file: path, type, then title,
// author and each instrument name as hex bytes, tab separated.
#include <adplug/adplug.h>
#include <adplug/silentopl.h>
#include <cstdio>

static void hex(const std::string &s) {
  putchar('\t');
  for (unsigned char c : s) printf("%02x", c);
}

int main(int argc, char **argv) {
  for (int a = 1; a < argc; a++) {
    CSilentopl opl;
    CPlayer *p = CAdPlug::factory(argv[a], &opl);
    printf("%s", argv[a]);
    if (!p) { printf("\tFAIL\n"); continue; }
    printf("\t%s", p->gettype().c_str());
    hex(p->gettitle());
    hex(p->getauthor());
    printf("\t%u", p->getinstruments());
    for (unsigned i = 0; i < p->getinstruments(); i++) hex(p->getinstrument(i));
    printf("\n");
    delete p;
  }
}
