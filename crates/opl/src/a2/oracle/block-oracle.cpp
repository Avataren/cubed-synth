// Black-box oracle for the A2M decompressors (plan-opl.md, O6).
//
// Runs AdPlug's exported depackers (depack.h, sixdepack.h, unlzh.h) on every
// data block of every .a2m given on the command line, and prints one line per
// block: path, block index, packer, unpacked size, FNV-1a-64 of the output.
// AdPlug is LGPL: it is linked here only to produce reference data, the same
// way golden/opl3-oracle.cpp uses ymfm; none of its code is in the crate.
#include <adplug/depack.h>
#include <adplug/sixdepack.h>
#include <adplug/unlzh.h>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

static uint64_t fnv(const unsigned char *p, size_t n) {
  uint64_t h = 0xcbf29ce484222325ull;
  for (size_t i = 0; i < n; i++) { h ^= p[i]; h *= 0x100000001b3ull; }
  return h;
}

int main(int argc, char **argv) {
  std::vector<unsigned char> out(4 << 20);
  for (int a = 1; a < argc; a++) {
    FILE *f = fopen(argv[a], "rb");
    if (!f) return 1;
    std::vector<unsigned char> b;
    int c;
    while ((c = fgetc(f)) != EOF) b.push_back(c);
    fclose(f);
    int v = b[14], npat = b[15];
    int fields = v <= 4 ? 5 : v <= 8 ? 9 : 17, wide = v >= 9;
    int per = v <= 4 ? 16 : 8;
    const char *packer = (v == 1 || v == 5) ? "sixpack" : v <= 11 ? "aplib" : "lzh";
    size_t at = 16 + fields * (wide ? 4 : 2);
    int blocks = 1 + (npat + per - 1) / per;
    for (int i = 0; i < blocks && i < fields; i++) {
      size_t len = wide ? (b[16 + 4 * i] | b[17 + 4 * i] << 8 | b[18 + 4 * i] << 16 | (size_t)b[19 + 4 * i] << 24)
                        : (b[16 + 2 * i] | b[17 + 2 * i] << 8);
      if (at + len > b.size()) { printf("%s\t%d\t%s\tpast-end\t-\n", argv[a], i, packer); break; }
      std::vector<unsigned char> in(b.begin() + at, b.begin() + at + len);
      in.resize(len + 8);
      long n;
      if (!strcmp(packer, "sixpack"))
        n = (long)Sixdepak::decode((unsigned short *)in.data(), len, out.data(), out.size());
      else if (!strcmp(packer, "aplib"))
        n = (long)(int)aP_depack(in.data(), out.data(), len, out.size());
      else
        n = LZH_decompress((char *)in.data(), (char *)out.data(), len, out.size());
      if (n < 0) printf("%s\t%d\t%s\terror\t-\n", argv[a], i, packer);
      else printf("%s\t%d\t%s\t%ld\t%016llx\n", argv[a], i, packer, n, (unsigned long long)fnv(out.data(), n));
      at += len;
    }
  }
}
