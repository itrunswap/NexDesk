#pragma once
#include <stdbool.h>
#include <stdint.h>

enum NexDeskYuvMatrix {
    ND_601_VIDEO = 2, ND_601_FULL,
    ND_709_VIDEO, ND_709_FULL,
    ND_2020_VIDEO, ND_2020_FULL
};

bool nexdesk_nv12_to_rgb(const uint8_t *y, int ys, const uint8_t *uv, int uvs,
                         uint8_t *out, int stride, int width, int height, int color, bool rgba);
void nexdesk_planar_to_rgb(const uint8_t *y, int ys, const uint8_t *u, int us,
                           const uint8_t *v, int vs, uint8_t *out, int stride,
                           int width, int height, int color, bool rgba, int sampling);
