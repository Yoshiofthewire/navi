#include <assert.h>
#include "navi-indicator.h"

int main(void) {
    const uint32_t blue = 0x2ba3d8ff, green = 0x5bd6a2ff, purple = 0xb69cffff;
    for (size_t slice = 0; slice < 8; ++slice) {
        assert(navi_slice_color(0, slice) == blue);
        assert(navi_slice_color(1, slice) == (slice == 0 ? green : blue));
        assert(navi_slice_color(7, slice) == (slice < 7 ? green : blue));
        assert(navi_slice_color(8, slice) == green);
        assert(navi_slice_color(9, slice) == (slice == 0 ? purple : green));
        assert(navi_slice_color(16, slice) == purple);
        assert(navi_slice_color(48, slice) == blue);
    }
    assert(navi_character_count(NULL, 0) == 0);
    assert(navi_character_count("abcdefgh", 8) == 8);
    assert(navi_character_count("a\xc3\xa9\xf0\x9f\x94\x92", 7) == 3);
    return 0;
}
