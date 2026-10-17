// Render oracle for rust-wasm/src/opl: ymfm's YMF262 (BSD-3, Aaron Giles),
// built by regen.sh against a pinned ymfm commit.
// stdin: script lines  "w <reg hex> <val hex>" | "s <n>" | "# comment"
// stdout: raw little-endian int16 pairs (left, right), one per sample.
#include "ymfm_opl.h"
#include <cstdio>
#include <cstring>
#include <vector>

struct Intf : ymfm::ymfm_interface {};

int main() {
    Intf intf;
    ymfm::ymf262 chip(intf);
    chip.reset();
    char line[256];
    ymfm::ymf262::output_data out;
    while (fgets(line, sizeof line, stdin)) {
        unsigned reg, val, n;
        if (sscanf(line, " w %x %x", &reg, &val) == 2) {
            chip.write((reg & 0x100) ? 2 : 0, reg & 0xff);
            chip.write((reg & 0x100) ? 3 : 1, val);
        } else if (sscanf(line, " s %u", &n) == 1) {
            for (unsigned i = 0; i < n; i++) {
                chip.generate(&out, 1);
                int16_t lr[2] = { (int16_t)out.data[0], (int16_t)out.data[1] };
                fwrite(lr, 2, 2, stdout);
            }
        }
    }
    return 0;
}
